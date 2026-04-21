use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};

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
) -> crate::error::Result<PtyHandle> {
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

    let slave_raw = pty.slave.as_raw_fd();
    let target = format!("{}{}", config.name_prefix, our_sid);

    let child = unsafe {
        Command::new(&config.tmux_bin)
            .arg("-L")
            .arg(&config.socket_name)
            .arg("attach-session")
            .arg("-t")
            .arg(&target)
            .stdin(Stdio::from_raw_fd(slave_raw))
            .stdout(Stdio::from_raw_fd(slave_raw))
            .stderr(Stdio::from_raw_fd(slave_raw))
            .env("TERM", "xterm-256color")
            .pre_exec(|| {
                nix::unistd::setsid().map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
                Ok(())
            })
            .spawn()?
    };

    drop(pty.slave);

    let flags = nix::fcntl::fcntl(master_raw, nix::fcntl::FcntlArg::F_GETFL)?;
    let mut oflags = nix::fcntl::OFlag::from_bits_truncate(flags);
    oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(master_raw, nix::fcntl::FcntlArg::F_SETFL(oflags))?;

    Ok(PtyHandle {
        master: pty.master,
        child,
    })
}
