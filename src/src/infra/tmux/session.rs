use std::sync::Once;

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use rand::Rng;
use serde::Serialize;

use crate::domain::error::{CiuError, Result};

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

        // Right-click opens a persistent context menu on *release* (not press),
        // so it doesn't auto-dismiss like tmux's default MouseDown3 popup.
        let _ = tmux_run(&config, &["unbind-key", "-T", "root", "MouseDown3Pane"]);
        let _ = tmux_run(&config, &["unbind-key", "-T", "root", "MouseUp3Pane"]);
        let _ = tmux_run(
            &config,
            &[
                "bind-key", "-T", "root", "MouseUp3Pane",
                "display-menu", "-O", "-t", "=", "-x", "M", "-y", "M",
                "Copy", "c", "send-keys -X copy-pipe-no-clear pbcopy",
                "Paste", "v", "paste-buffer -p",
                "", "", "",
                "Clear history", "C", "clear-history",
            ],
        );

        for table in ["copy-mode", "copy-mode-vi"] {
            // Drag-end: copy to macOS clipboard but keep the highlight.
            let _ = tmux_run(
                &config,
                &[
                    "bind-key", "-T", table, "MouseDragEnd1Pane",
                    "send-keys", "-X", "copy-pipe-no-clear", "pbcopy",
                ],
            );
            // Plain left-click exits copy-mode so the selection clears
            // without needing to reach for Escape.
            let _ = tmux_run(
                &config,
                &[
                    "bind-key", "-T", table, "MouseDown1Pane",
                    "send-keys", "-X", "cancel",
                ],
            );
        }
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

fn foreground_pid_from_ps(ps_text: &str, pane_pid: i32) -> Option<i32> {
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
    fg_pid
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

    let target_pid = foreground_pid_from_ps(&ps_text, pane_pid).unwrap_or(pane_pid);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::set_test_home;
    use crate::infra::tmux::command::test_stub::{cfg, write_stub_tmux, STUB_LOCK};

    #[test]
    fn new_our_sid_is_12_hex_chars() {
        let s = new_our_sid();
        assert_eq!(s.len(), 12);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn new_our_sid_is_random() {
        let a = new_our_sid();
        let b = new_our_sid();
        assert_ne!(a, b);
    }

    #[test]
    fn session_name_formats_with_prefix() {
        let c = TmuxConfig {
            tmux_bin: "/x".into(),
            socket_name: "s".into(),
            name_prefix: "ciu-".into(),
        };
        assert_eq!(session_name(&c, "abc"), "ciu-abc");
    }

    #[test]
    fn session_name_empty_prefix() {
        let c = TmuxConfig {
            tmux_bin: "/x".into(),
            socket_name: "s".into(),
            name_prefix: "".into(),
        };
        assert_eq!(session_name(&c, "abc"), "abc");
    }

    #[test]
    fn tmux_session_serializable() {
        let s = TmuxSession {
            our_sid: "s".into(),
            name: "ciu-s".into(),
            created_at: 1.0,
            cwd: "/tmp".into(),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v.get("our_sid").and_then(|x| x.as_str()), Some("s"));
        assert_eq!(v.get("name").and_then(|x| x.as_str()), Some("ciu-s"));
    }

    #[test]
    fn tmux_session_clone_preserves_fields() {
        let s = TmuxSession {
            our_sid: "a".into(),
            name: "b".into(),
            created_at: 1.0,
            cwd: "/c".into(),
        };
        let s2 = s.clone();
        assert_eq!(s.name, s2.name);
        assert!(format!("{s:?}").contains("TmuxSession"));
    }

    #[test]
    fn list_sessions_empty_when_run_fails() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 1\n");
        let c = cfg(bin.clone());
        let out = list_sessions(&c).unwrap();
        assert!(out.is_empty());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn list_sessions_parses_matching_prefix() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\ncat <<EOF\nciu-aaa|1700000000|/home/u\nother-x|1|/tmp\nciu-bbb|2.5|/var\nEOF\n");
        let c = cfg(bin.clone());
        let out = list_sessions(&c).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].our_sid, "aaa");
        assert_eq!(out[0].name, "ciu-aaa");
        assert_eq!(out[0].created_at, 1700000000.0);
        assert_eq!(out[0].cwd, "/home/u");
        assert_eq!(out[1].our_sid, "bbb");
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn list_sessions_skips_malformed_lines() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\necho 'broken|only-two-parts'\necho 'ciu-x|1|/p'\n");
        let c = cfg(bin.clone());
        let out = list_sessions(&c).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].our_sid, "x");
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn list_sessions_invalid_created_at_defaults_to_zero() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\necho 'ciu-x|not-a-num|/p'\n");
        let c = cfg(bin.clone());
        let out = list_sessions(&c).unwrap();
        assert_eq!(out[0].created_at, 0.0);
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn session_exists_true_when_stub_exits_zero() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let c = cfg(bin.clone());
        assert!(session_exists(&c, "any"));
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn session_exists_false_when_stub_exits_nonzero() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 1\n");
        let c = cfg(bin.clone());
        assert!(!session_exists(&c, "any"));
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn session_exists_false_when_binary_missing() {
        let c = cfg("/nope/never/tmux-zzz".into());
        assert!(!session_exists(&c, "any"));
    }

    #[test]
    fn kill_session_always_returns_ok_even_on_stub_failure() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 1\n");
        let c = cfg(bin.clone());
        assert!(kill_session(&c, "any").is_ok());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn send_enter_succeeds_with_stub() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let c = cfg(bin.clone());
        assert!(send_enter(&c, "sid").is_ok());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn send_enter_propagates_error_from_stub() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\necho oops >&2\nexit 1\n");
        let c = cfg(bin.clone());
        assert!(send_enter(&c, "sid").is_err());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn send_shift_enter_succeeds_with_stub() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let c = cfg(bin.clone());
        assert!(send_shift_enter(&c, "sid").is_ok());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn send_shift_enter_propagates_error_from_stub() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\necho boom 1>&2\nexit 1\n");
        let c = cfg(bin.clone());
        assert!(send_shift_enter(&c, "sid").is_err());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn resize_window_succeeds_with_stub() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let c = cfg(bin.clone());
        assert!(resize_window(&c, "sid", 80, 24).is_ok());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn resize_window_error_propagates() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 1\n");
        let c = cfg(bin.clone());
        assert!(resize_window(&c, "sid", 80, 24).is_err());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn capture_pane_returns_stub_stdout_bytes() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nprintf 'pane-body'\n");
        let c = cfg(bin.clone());
        let bytes = capture_pane(&c, "sid").unwrap();
        assert_eq!(&bytes, b"pane-body");
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn capture_pane_error_propagates() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 1\n");
        let c = cfg(bin.clone());
        assert!(capture_pane(&c, "sid").is_err());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn send_bytes_handles_empty_payload() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let c = cfg(bin.clone());
        assert!(send_bytes(&c, "sid", &[]).is_ok());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn send_bytes_short_payload_single_chunk() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let c = cfg(bin.clone());
        assert!(send_bytes(&c, "sid", b"hi").is_ok());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn send_bytes_long_payload_multi_chunk() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let c = cfg(bin.clone());
        let data = vec![0xAAu8; 1300];
        assert!(send_bytes(&c, "sid", &data).is_ok());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn send_bytes_failing_stub_returns_error() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 1\n");
        let c = cfg(bin.clone());
        assert!(send_bytes(&c, "sid", b"xy").is_err());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn send_signal_invalid_signal_returns_other() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nprintf '99999'\n");
        let c = cfg(bin.clone());
        let err = send_signal(&c, "sid", "BOGUS").unwrap_err();
        assert!(matches!(err, CiuError::Other(_)));
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn send_signal_invalid_pane_pid_returns_other() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nprintf 'not-a-number'\n");
        let c = cfg(bin.clone());
        let err = send_signal(&c, "sid", "TERM").unwrap_err();
        assert!(
            matches!(&err, CiuError::Other(msg) if msg.contains("invalid pane_pid")),
            "unexpected err: {err:?}"
        );
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn send_signal_display_message_failure_propagates() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 1\n");
        let c = cfg(bin.clone());
        assert!(send_signal(&c, "sid", "TERM").is_err());
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn foreground_pid_from_ps_skips_short_and_nonnumeric_rows() {
        let text = "\
too few
abc 100 +\n\
200 xyz +\n\
300 100 R+\n\
400 100 I\n\
";
        assert_eq!(foreground_pid_from_ps(text, 100), Some(300));
    }

    #[test]
    fn foreground_pid_from_ps_returns_none_when_no_fg_process() {
        let text = "100 1 S\n200 1 I\n";
        assert_eq!(foreground_pid_from_ps(text, 999), None);
    }

    #[test]
    fn send_signal_happy_path_with_own_pid_sends_sigcont() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let my_pid = std::process::id();
        let bin = write_stub_tmux(&h, &format!("#!/bin/sh\necho {my_pid}\n"));
        let c = cfg(bin.clone());
        send_signal(&c, "sid", "CONT").expect("SIGCONT to self should succeed");
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn ensure_global_keys_calls_once_noop_when_run_fails() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let c = cfg(bin.clone());
        ensure_global_keys(&c);
        ensure_global_keys(&c);
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn spawn_missing_binary_returns_tmux_not_found() {
        let c = cfg("/nope/never/tmux-zzz".into());
        let err = spawn(&c, "/tmp", &["claude".to_string()], vec![]).unwrap_err();
        assert!(matches!(err, CiuError::TmuxNotFound(_)));
    }

    #[test]
    fn spawn_stub_success_returns_our_sid() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let c = cfg(bin.clone());
        let sid = spawn(&c, "/tmp", &["cmd".to_string()], vec![]).unwrap();
        assert_eq!(sid.len(), 12);
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn spawn_stub_failure_propagates_error() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\n[ \"$1\" = \"-L\" ] && [ \"$3\" = \"new-session\" ] && { echo bad >&2; exit 1; }\nexit 0\n");
        let c = cfg(bin.clone());
        let err = spawn(&c, "/tmp", &["cmd".to_string()], vec![]).unwrap_err();
        assert!(matches!(err, CiuError::TmuxCommand { .. }));
        let _ = std::fs::remove_file(&bin);
    }
}
