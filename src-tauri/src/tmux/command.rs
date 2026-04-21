use std::process::Command;

use crate::error::{CiuError, Result};

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
