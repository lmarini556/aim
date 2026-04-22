#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use aim_lib::http::auth;
use aim_lib::native::{poller, tray};
use tauri_plugin_global_shortcut::GlobalShortcutExt;
use aim_lib::infra::paths;
use aim_lib::services::instances::{AppState, InstancesCache, PendingFocus, PsCache};
use aim_lib::services::summarizer::Summarizer;
use aim_lib::infra::tmux::TmuxConfig;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Manager;
use tracing::info;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn find_resource_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // Dev: target/debug/aim → ../../.. → project root → resources/
    let dev = exe.parent()?.parent()?.parent()?.join("resources");
    if dev.is_dir() {
        return Some(dev);
    }
    // Bundled: AIM.app/Contents/MacOS/aim → ../Resources/
    let bundled = exe.parent()?.parent()?.join("Resources");
    if bundled.is_dir() {
        return Some(bundled);
    }
    // Cargo manifest fallback
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
    if manifest.is_dir() {
        return Some(manifest);
    }
    None
}

fn resolve_tmux_bin() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("CIU_TMUX") {
        let pb = std::path::PathBuf::from(&p);
        if pb.exists() {
            return pb;
        }
    }

    let bundled = paths::HOOK_BIN_DIR.join("tmux");
    if bundled.exists() {
        return bundled;
    }

    if let Ok(output) = std::process::Command::new("which")
        .arg("tmux")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return std::path::PathBuf::from(path);
            }
        }
    }

    std::path::PathBuf::from("/opt/homebrew/bin/tmux")
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    paths::ensure_dirs();

    let token = auth::load_or_create_token();
    let tmux_bin = resolve_tmux_bin();
    info!("tmux binary: {}", tmux_bin.display());

    let tmux_config = TmuxConfig {
        tmux_bin,
        socket_name: "ciu".to_string(),
        name_prefix: "ciu-".to_string(),
    };

    aim_lib::infra::tmux::session::ensure_global_keys(&tmux_config);

    let state = Arc::new(AppState {
        tmux_config: tmux_config.clone(),
        auth_token: token.clone(),
        server_start: now(),
        instances_cache: Mutex::new(InstancesCache {
            at: 0.0,
            data: Vec::new(),
        }),
        pending_focus: Mutex::new(PendingFocus {
            sid: None,
            ts: 0.0,
        }),
        ps_cache: Mutex::new(PsCache {
            at: 0.0,
            map: HashMap::new(),
        }),
    });

    let resource_dir = find_resource_dir();
    if let Some(ref res) = resource_dir {
        let hook_src = res.join("hook_writer.py");
        if hook_src.exists() {
            aim_lib::services::hooks::install(&hook_src);
        }
        let tmux_src = res.join("tmux");
        let tmux_dest = paths::HOOK_BIN_DIR.join("tmux");
        if tmux_src.exists() && !tmux_dest.exists() {
            let _ = std::fs::copy(&tmux_src, &tmux_dest);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&tmux_dest, std::fs::Permissions::from_mode(0o755));
            }
            info!("bundled tmux copied to {}", tmux_dest.display());
        }
    }

    let host = std::env::var("CIU_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port: u16 = std::env::var("CIU_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7878);

    let router = aim_lib::http::build_router(state.clone());
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");
    info!("axum server listening on {addr}");

    let auth_url = format!("http://{addr}/auth?t={token}");
    println!();
    println!("{}", "=".repeat(72));
    println!("  AIM (Agent Instances Manager)");
    println!("{}", "=".repeat(72));
    println!("  Dashboard:     http://{addr}");
    println!("  Authenticate:  {auth_url}");
    println!("  Token file:    {}", paths::TOKEN_FILE.display());
    println!("  tmux:          {}", tmux_config.tmux_bin.display());
    println!("{}", "=".repeat(72));
    println!();

    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });

    let state_for_poller = state.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(move |app| {
            let handle = app.handle().clone();
            tray::setup(&handle)?;

            let handle_shortcut = handle.clone();
            app.global_shortcut().on_shortcut("CmdOrCtrl+Shift+C", move |_app, _shortcut, event| {
                if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                    if let Some(win) = handle_shortcut.get_webview_window("main") {
                        if win.is_visible().unwrap_or(false) {
                            let _ = win.hide();
                        } else {
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                }
            })?;

            let summarizer = Arc::new(Summarizer::new());
            let poller_handle = handle.clone();
            tokio::spawn(poller::run(poller_handle, state_for_poller, summarizer));

            if let Some(win) = handle.get_webview_window("main") {
                let url = format!("http://127.0.0.1:{port}/?t={token}");
                let _ = win.eval(&format!("window.location.replace('{url}')"));
                let _ = win.show();
                #[cfg(target_os = "macos")]
                aim_lib::native::webview_menu::disable_context_menu(&win);
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error running tauri application");
}
