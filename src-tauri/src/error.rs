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
