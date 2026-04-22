use std::process::Command;

use crate::domain::error::{CiuError, Result};

use super::TmuxConfig;

#[derive(Debug, Clone)]
pub struct TmuxOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

pub fn tmux_run(config: &TmuxConfig, args: &[&str]) -> Result<TmuxOutput> {
    if !config.tmux_bin.exists() {
        return Err(CiuError::TmuxNotFound(config.tmux_bin.clone()));
    }

    let output = Command::new(&config.tmux_bin)
        .arg("-L")
        .arg(&config.socket_name)
        .args(args)
        .output()?;

    Ok(TmuxOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
    })
}

pub fn tmux_run_checked(config: &TmuxConfig, args: &[&str]) -> Result<TmuxOutput> {
    let out = tmux_run(config, args)?;
    if !out.success {
        return Err(CiuError::TmuxCommand {
            cmd: args.join(" "),
            stderr: out.stderr.clone(),
        });
    }
    Ok(out)
}

pub fn tmux_spawn_session(
    config: &TmuxConfig,
    args: Vec<String>,
    env_overlay: Vec<(String, String)>,
) -> Result<TmuxOutput> {
    if !config.tmux_bin.exists() {
        return Err(CiuError::TmuxNotFound(config.tmux_bin.clone()));
    }

    let mut cmd = Command::new(&config.tmux_bin);
    cmd.arg("-L").arg(&config.socket_name);
    for arg in &args {
        cmd.arg(arg);
    }
    for (key, val) in &env_overlay {
        cmd.env(key, val);
    }

    let output = cmd.output()?;
    let out = TmuxOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
    };
    if !out.success {
        return Err(CiuError::TmuxCommand {
            cmd: args.join(" "),
            stderr: out.stderr,
        });
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod test_stub {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    pub static STUB_LOCK: Mutex<()> = Mutex::new(());

    pub fn write_stub_tmux(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(format!("tmux-stub-{}", uniq()));
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn uniq() -> String {
        format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    pub fn cfg(bin: std::path::PathBuf) -> TmuxConfig {
        TmuxConfig {
            tmux_bin: bin,
            socket_name: "ciu-test".into(),
            name_prefix: "ciu-".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_stub::*;
    use super::*;
    use crate::test_util::set_test_home;

    #[test]
    fn tmux_output_clone_and_debug() {
        let o = TmuxOutput {
            stdout: "out".into(),
            stderr: "err".into(),
            success: true,
        };
        let o2 = o.clone();
        assert_eq!(o.stdout, o2.stdout);
        assert!(format!("{o:?}").contains("TmuxOutput"));
    }

    #[test]
    fn tmux_run_missing_binary_returns_tmux_not_found() {
        let c = cfg("/nope/never/tmux-zzz".into());
        let err = tmux_run(&c, &["list-sessions"]).unwrap_err();
        assert!(matches!(err, CiuError::TmuxNotFound(_)));
    }

    #[test]
    fn tmux_spawn_session_missing_binary_returns_tmux_not_found() {
        let c = cfg("/nope/never/tmux-zzz".into());
        let err = tmux_spawn_session(&c, vec!["x".into()], vec![]).unwrap_err();
        assert!(matches!(err, CiuError::TmuxNotFound(_)));
    }

    #[test]
    fn tmux_run_success_captures_stdout() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\necho 'hello stdout'\n");
        let c = cfg(bin.clone());
        let o = tmux_run(&c, &["whatever"]).unwrap();
        assert!(o.success);
        assert!(o.stdout.contains("hello stdout"));
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn tmux_run_nonzero_exit_returns_unsuccessful_output() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\necho 'bad' >&2\nexit 1\n");
        let c = cfg(bin.clone());
        let o = tmux_run(&c, &["x"]).unwrap();
        assert!(!o.success);
        assert!(o.stderr.contains("bad"));
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn tmux_run_checked_propagates_error_on_failure() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\necho err >&2\nexit 2\n");
        let c = cfg(bin.clone());
        let err = tmux_run_checked(&c, &["a", "b"]).unwrap_err();
        assert!(
            matches!(&err, CiuError::TmuxCommand { cmd, stderr } if cmd == "a b" && stderr.contains("err")),
            "unexpected err: {err:?}"
        );
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn tmux_run_checked_returns_output_on_success() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\necho ok\n");
        let c = cfg(bin.clone());
        let o = tmux_run_checked(&c, &["x"]).unwrap();
        assert!(o.success);
        assert!(o.stdout.contains("ok"));
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn tmux_spawn_session_success_passes_env() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\necho \"$MY_ENV_VAR\"\n");
        let c = cfg(bin.clone());
        let o = tmux_spawn_session(
            &c,
            vec!["ignored".into()],
            vec![("MY_ENV_VAR".into(), "spawn-env-val".into())],
        )
        .unwrap();
        assert!(o.stdout.contains("spawn-env-val"));
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn tmux_spawn_session_failure_returns_tmux_command_err() {
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\necho oh >&2\nexit 3\n");
        let c = cfg(bin.clone());
        let err = tmux_spawn_session(&c, vec!["new-session".into(), "-d".into()], vec![]).unwrap_err();
        assert!(
            matches!(&err, CiuError::TmuxCommand { cmd, stderr } if cmd == "new-session -d" && stderr.contains("oh")),
            "unexpected err: {err:?}"
        );
        let _ = std::fs::remove_file(&bin);
    }
}
