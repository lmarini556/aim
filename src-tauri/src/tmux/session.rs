use std::sync::Once;

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use rand::Rng;
use serde::Serialize;

use crate::error::{CiuError, Result};

use super::command::{tmux_run, tmux_run_checked, tmux_spawn_session};
use super::TmuxConfig;

static GLOBAL_KEYS_ONCE: Once = Once::new();

#[derive(Debug, Clone, Serialize)]
pub struct TmuxSession {
    pub our_sid: String,
    pub name: String,
    pub created_at: f64,
    pub cwd: String,
}

pub fn new_our_sid() -> String {
    let mut rng = rand::rng();
    let bytes: [u8; 6] = rng.random();
    hex::encode(bytes)
}

pub fn session_name(config: &TmuxConfig, our_sid: &str) -> String {
    format!("{}{}", config.name_prefix, our_sid)
}

pub fn ensure_global_keys(config: &TmuxConfig) {
    let config = config.clone();
    GLOBAL_KEYS_ONCE.call_once(move || {
        let _ = tmux_run(
            &config,
            &["set-option", "-g", "extended-keys", "always"],
        );
        let _ = tmux_run(
            &config,
            &[
                "set-option",
                "-g",
                "-a",
                "terminal-features",
                ",xterm-256color:extkeys",
            ],
        );
        let _ = tmux_run(
            &config,
            &[
                "bind-key",
                "-n",
                "S-Enter",
                "send-keys",
                "Escape",
                "[13;2u",
            ],
        );
    });
}

pub fn list_sessions(config: &TmuxConfig) -> Result<Vec<TmuxSession>> {
    let out = tmux_run(
        config,
        &[
            "list-sessions",
            "-F",
            "#{session_name}|#{session_created}|#{pane_current_path}",
        ],
    )?;

    if !out.success {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    for line in out.stdout.lines() {
        let parts: Vec<&str> = line.splitn(3, '|').collect();
        if parts.len() != 3 {
            continue;
        }
        let name = parts[0];
        if !name.starts_with(&config.name_prefix) {
            continue;
        }
        let our_sid = name
            .strip_prefix(&config.name_prefix)
            .unwrap_or_default()
            .to_string();
        let created_at = parts[1].parse::<f64>().unwrap_or(0.0);
        let cwd = parts[2].to_string();
        sessions.push(TmuxSession {
            our_sid,
            name: name.to_string(),
            created_at,
            cwd,
        });
    }

    Ok(sessions)
}

pub fn session_exists(config: &TmuxConfig, our_sid: &str) -> bool {
    let name = session_name(config, our_sid);
    tmux_run(config, &["has-session", "-t", &name])
        .map(|o| o.success)
        .unwrap_or(false)
}

pub fn spawn(
    config: &TmuxConfig,
    cwd: &str,
    command: &[String],
    extra_env: Vec<(String, String)>,
) -> Result<String> {
    let our_sid = new_our_sid();
    let name = session_name(config, &our_sid);

    ensure_global_keys(config);

    let env_flag = format!("CLAUDE_INSTANCES_UI_OWNED={our_sid}");

    let mut args: Vec<String> = vec![
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        name.clone(),
        "-c".into(),
        cwd.into(),
        "-x".into(),
        "200".into(),
        "-y".into(),
        "50".into(),
        "-e".into(),
        env_flag,
    ];
    for part in command {
        args.push(part.clone());
    }

    let mut env_overlay = extra_env;
    env_overlay.push(("CLAUDE_INSTANCES_UI_OWNED".into(), our_sid.clone()));

    tmux_spawn_session(config, args, env_overlay)?;

    let session_opts: &[(&str, &str)] = &[
        ("window-size", "manual"),
        ("mouse", "on"),
        ("history-limit", "50000"),
        ("remain-on-exit", "on"),
        ("extended-keys", "always"),
    ];
    for (key, val) in session_opts {
        let _ = tmux_run(config, &["set-option", "-t", &name, key, val]);
    }

    let _ = tmux_run(
        config,
        &[
            "set-option",
            "-t",
            &name,
            "-a",
            "terminal-features",
            ",xterm-256color:extkeys",
        ],
    );

    Ok(our_sid)
}

pub fn kill_session(config: &TmuxConfig, our_sid: &str) -> Result<()> {
    let name = session_name(config, our_sid);
    let _ = tmux_run(config, &["kill-session", "-t", &name]);
    Ok(())
}

pub fn resize_window(config: &TmuxConfig, our_sid: &str, cols: u32, rows: u32) -> Result<()> {
    let name = session_name(config, our_sid);
    let cols_str = cols.to_string();
    let rows_str = rows.to_string();
    tmux_run_checked(
        config,
        &["resize-window", "-t", &name, "-x", &cols_str, "-y", &rows_str],
    )?;
    Ok(())
}

pub fn capture_pane(config: &TmuxConfig, our_sid: &str) -> Result<Vec<u8>> {
    let name = session_name(config, our_sid);
    let out = tmux_run_checked(config, &["capture-pane", "-p", "-e", "-t", &name])?;
    Ok(out.stdout.into_bytes())
}

pub fn send_bytes(config: &TmuxConfig, our_sid: &str, data: &[u8]) -> Result<()> {
    let name = session_name(config, our_sid);
    for chunk in data.chunks(512) {
        let hex_pairs: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let mut args = vec!["send-keys", "-H", "-t", &name];
        let hex_refs: Vec<&str> = hex_pairs.iter().map(|s| s.as_str()).collect();
        args.extend(hex_refs);
        tmux_run_checked(config, &args)?;
    }
    Ok(())
}

pub fn send_enter(config: &TmuxConfig, our_sid: &str) -> Result<()> {
    let name = session_name(config, our_sid);
    tmux_run_checked(config, &["send-keys", "-t", &name, "Enter"])?;
    Ok(())
}

pub fn send_signal(config: &TmuxConfig, our_sid: &str, sig: &str) -> Result<()> {
    let name = session_name(config, our_sid);

    let out = tmux_run_checked(
        config,
        &["display-message", "-t", &name, "-p", "#{pane_pid}"],
    )?;
    let pane_pid: i32 = out
        .stdout
        .trim()
        .parse()
        .map_err(|_| CiuError::Other(format!("invalid pane_pid: {}", out.stdout.trim())))?;

    let ps_output = std::process::Command::new("ps")
        .args(["-o", "pid=,ppid=,stat=", "-ax"])
        .output()?;
    let ps_text = String::from_utf8_lossy(&ps_output.stdout);

    let mut fg_pid: Option<i32> = None;
    for line in ps_text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let pid: i32 = match fields[0].parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let ppid: i32 = match fields[1].parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let stat = fields[2];
        if ppid == pane_pid && stat.contains('+') {
            fg_pid = Some(pid);
        }
    }

    let target_pid = fg_pid.unwrap_or(pane_pid);

    let signal = match sig.to_uppercase().as_str() {
        "SIGINT" | "INT" | "2" => Signal::SIGINT,
        "SIGTERM" | "TERM" | "15" => Signal::SIGTERM,
        "SIGKILL" | "KILL" | "9" => Signal::SIGKILL,
        "SIGHUP" | "HUP" | "1" => Signal::SIGHUP,
        "SIGUSR1" | "USR1" | "10" => Signal::SIGUSR1,
        "SIGUSR2" | "USR2" | "12" => Signal::SIGUSR2,
        "SIGTSTP" | "TSTP" | "20" => Signal::SIGTSTP,
        "SIGCONT" | "CONT" | "18" => Signal::SIGCONT,
        _ => {
            return Err(CiuError::Other(format!("unsupported signal: {sig}")));
        }
    };

    kill(Pid::from_raw(target_pid), signal)?;
    Ok(())
}

pub fn send_shift_enter(config: &TmuxConfig, our_sid: &str) -> Result<()> {
    let name = session_name(config, our_sid);
    tmux_run_checked(
        config,
        &[
            "send-keys", "-H", "-t", &name, "1b", "5b", "31", "33", "3b", "32", "75",
        ],
    )?;
    Ok(())
}
