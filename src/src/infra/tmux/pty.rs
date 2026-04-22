use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus};

pub struct PtyHandle {
    master: OwnedFd,
    child: Child,
}

impl PtyHandle {
    pub fn raw_fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let ws = nix::libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            nix::libc::ioctl(self.master.as_raw_fd(), nix::libc::TIOCSWINSZ, &ws);
        }
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    #[cfg(test)]
    pub(crate) fn from_parts(master: OwnedFd, child: Child) -> Self {
        Self { master, child }
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn pty_attach(
    config: &super::TmuxConfig,
    our_sid: &str,
    cols: u16,
    rows: u16,
) -> crate::domain::error::Result<PtyHandle> {
    let pty = nix::pty::openpty(None, None)?;

    let master_raw = pty.master.as_raw_fd();
    let ws = nix::libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        nix::libc::ioctl(master_raw, nix::libc::TIOCSWINSZ, &ws);
    }

    let target = format!("{}{}", config.name_prefix, our_sid);

    let stdin_fd: OwnedFd = pty.slave.try_clone()?;
    let stdout_fd: OwnedFd = pty.slave.try_clone()?;
    let stderr_fd: OwnedFd = pty.slave;

    let child = unsafe {
        Command::new(&config.tmux_bin)
            .arg("-L")
            .arg(&config.socket_name)
            .arg("attach-session")
            .arg("-t")
            .arg(&target)
            .stdin(stdin_fd)
            .stdout(stdout_fd)
            .stderr(stderr_fd)
            .env("TERM", "xterm-256color")
            .pre_exec(|| {
                nix::unistd::setsid().map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
                Ok(())
            })
            .spawn()?
    };

    let flags = nix::fcntl::fcntl(master_raw, nix::fcntl::FcntlArg::F_GETFL)?;
    let mut oflags = nix::fcntl::OFlag::from_bits_truncate(flags);
    oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(master_raw, nix::fcntl::FcntlArg::F_SETFL(oflags))?;

    Ok(PtyHandle {
        master: pty.master,
        child,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::set_test_home;
    use crate::infra::tmux::TmuxConfig;

    fn cfg(bin: std::path::PathBuf) -> TmuxConfig {
        TmuxConfig {
            tmux_bin: bin,
            socket_name: "ciu-test".into(),
            name_prefix: "ciu-".into(),
        }
    }

    #[test]
    fn pty_handle_resize_ioctls_fresh_openpty_master() {
        let _ = cfg(std::path::PathBuf::from("/nope"));
        let _ = set_test_home();
        let pty = nix::pty::openpty(None, None).unwrap();
        let (master, slave) = (pty.master, pty.slave);
        let (child_reader, child_writer) = nix::unistd::pipe().unwrap();
        let child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .stdin(std::process::Stdio::from(child_reader))
            .stdout(std::process::Stdio::from(child_writer))
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        drop(slave);
        let mut h = PtyHandle { master, child };
        assert!(h.raw_fd() >= 0);
        h.resize(100, 30);
        let _ = h.try_wait().unwrap();
        drop(h);
    }
}
