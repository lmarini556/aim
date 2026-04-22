use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CiuError {
    #[error("tmux binary not found at {0}")]
    TmuxNotFound(PathBuf),

    #[error("tmux command failed: {cmd} — {stderr}")]
    TmuxCommand { cmd: String, stderr: String },

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("not a managed session")]
    NotManaged,

    #[error("directory not found: {0}")]
    DirNotFound(String),

    #[error("pty error: {0}")]
    Pty(#[from] nix::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CiuError>;

impl From<CiuError> for axum::http::StatusCode {
    fn from(e: CiuError) -> Self {
        match e {
            CiuError::SessionNotFound(_) | CiuError::DirNotFound(_) => {
                axum::http::StatusCode::NOT_FOUND
            }
            CiuError::NotManaged => axum::http::StatusCode::BAD_REQUEST,
            CiuError::TmuxNotFound(_) => axum::http::StatusCode::SERVICE_UNAVAILABLE,
            _ => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn session_not_found_is_404() {
        let s: StatusCode = CiuError::SessionNotFound("abc".into()).into();
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[test]
    fn dir_not_found_is_404() {
        let s: StatusCode = CiuError::DirNotFound("/tmp".into()).into();
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[test]
    fn not_managed_is_400() {
        let s: StatusCode = CiuError::NotManaged.into();
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn tmux_not_found_is_503() {
        let s: StatusCode = CiuError::TmuxNotFound(PathBuf::from("/tmx")).into();
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn tmux_command_is_500() {
        let s: StatusCode =
            CiuError::TmuxCommand { cmd: "c".into(), stderr: "e".into() }.into();
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn pty_error_is_500() {
        let s: StatusCode = CiuError::Pty(nix::Error::EINVAL).into();
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn io_error_is_500() {
        let io = std::io::Error::new(std::io::ErrorKind::Other, "x");
        let s: StatusCode = CiuError::Io(io).into();
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn json_error_is_500() {
        let je = serde_json::from_str::<serde_json::Value>("nope").unwrap_err();
        let s: StatusCode = CiuError::Json(je).into();
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn other_is_500() {
        let s: StatusCode = CiuError::Other("x".into()).into();
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn from_io_impl_covers_io_conversion() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope");
        let e: CiuError = io.into();
        assert!(matches!(e, CiuError::Io(_)));
        assert!(!matches!(CiuError::NotManaged, CiuError::Io(_)));
    }

    #[test]
    fn from_json_impl_covers_json_conversion() {
        let je = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let e: CiuError = je.into();
        assert!(matches!(e, CiuError::Json(_)));
        assert!(!matches!(CiuError::NotManaged, CiuError::Json(_)));
    }

    #[test]
    fn from_nix_impl_covers_pty_conversion() {
        let e: CiuError = nix::Error::ENOENT.into();
        assert!(matches!(e, CiuError::Pty(_)));
        assert!(!matches!(CiuError::NotManaged, CiuError::Pty(_)));
    }

    #[test]
    fn display_formatting_tmux_not_found() {
        let e = CiuError::TmuxNotFound(PathBuf::from("/usr/bin/tmux"));
        assert_eq!(format!("{e}"), "tmux binary not found at /usr/bin/tmux");
    }

    #[test]
    fn display_formatting_session_not_found() {
        let e = CiuError::SessionNotFound("xyz".into());
        assert_eq!(format!("{e}"), "session not found: xyz");
    }

    #[test]
    fn display_formatting_dir_not_found() {
        let e = CiuError::DirNotFound("/tmp".into());
        assert_eq!(format!("{e}"), "directory not found: /tmp");
    }

    #[test]
    fn display_formatting_not_managed() {
        let e = CiuError::NotManaged;
        assert_eq!(format!("{e}"), "not a managed session");
    }

    #[test]
    fn display_formatting_other() {
        let e = CiuError::Other("custom".into());
        assert_eq!(format!("{e}"), "custom");
    }

    #[test]
    fn display_formatting_tmux_command() {
        let e = CiuError::TmuxCommand { cmd: "ls".into(), stderr: "fail".into() };
        assert_eq!(format!("{e}"), "tmux command failed: ls — fail");
    }

    #[test]
    fn result_type_alias_compiles() {
        let ok: Result<u32> = Ok(1);
        assert_eq!(ok.unwrap(), 1);
        let err: Result<u32> = Err(CiuError::NotManaged);
        assert!(err.is_err());
    }

    #[test]
    fn debug_impl_present() {
        let e = CiuError::NotManaged;
        let s = format!("{e:?}");
        assert!(s.contains("NotManaged"));
    }
}
