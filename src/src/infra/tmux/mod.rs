pub mod command;
pub mod pty;
pub mod session;

#[derive(Debug, Clone)]
pub struct TmuxConfig {
    pub tmux_bin: std::path::PathBuf,
    pub socket_name: String,
    pub name_prefix: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_config_clone_preserves_fields() {
        let c = TmuxConfig {
            tmux_bin: "/bin/tmux".into(),
            socket_name: "sock".into(),
            name_prefix: "ciu-".into(),
        };
        let c2 = c.clone();
        assert_eq!(c2.tmux_bin, c.tmux_bin);
        assert_eq!(c2.socket_name, c.socket_name);
        assert_eq!(c2.name_prefix, c.name_prefix);
    }

    #[test]
    fn tmux_config_debug_has_name() {
        let c = TmuxConfig {
            tmux_bin: "/bin/tmux".into(),
            socket_name: "sock".into(),
            name_prefix: "ciu-".into(),
        };
        assert!(format!("{c:?}").contains("TmuxConfig"));
    }
}
