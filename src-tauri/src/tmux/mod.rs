pub mod command;
pub mod pty;
pub mod session;

#[derive(Debug, Clone)]
pub struct TmuxConfig {
    pub tmux_bin: std::path::PathBuf,
    pub socket_name: String,
    pub name_prefix: String,
}
