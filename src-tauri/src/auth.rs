use crate::paths;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use rand::Rng;
use std::sync::Arc;

pub fn load_or_create_token() -> String {
    if let Ok(t) = std::fs::read_to_string(&*paths::TOKEN_FILE) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    let bytes: [u8; 24] = rand::rng().random();
    let token = base32_encode(&bytes);
    let _ = std::fs::write(&*paths::TOKEN_FILE, &token);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&*paths::TOKEN_FILE, std::fs::Permissions::from_mode(0o600));
    }
    token
}

fn base32_encode(_data: &[u8]) -> String {
    use rand::Rng;
    let bytes: [u8; 32] = rand::rng().random();
    hex::encode(bytes)
}

pub fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

pub fn extract_token_from_request(req: &Request) -> Option<String> {
    extract_token(req)
}

fn extract_token(req: &Request) -> Option<String> {
    if let Some(auth) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(s) = auth.to_str() {
            if let Some(token) = s.strip_prefix("Bearer ") {
                return Some(token.to_string());
            }
        }
    }
    if let Some(cookie) = req.headers().get(header::COOKIE) {
        if let Ok(s) = cookie.to_str() {
            for part in s.split(';') {
                let part = part.trim();
                if let Some(val) = part.strip_prefix("ciu_token=") {
                    return Some(val.to_string());
                }
            }
        }
    }
    let uri = req.uri();
    if let Some(q) = uri.query() {
        for pair in q.split('&') {
            if let Some(val) = pair.strip_prefix("t=") {
                return Some(val.to_string());
            }
        }
    }
    None
}

pub async fn auth_middleware(
    req: Request,
    next: Next,
) -> std::result::Result<Response, StatusCode> {
    let path = req.uri().path().to_string();

    if path == "/auth"
        || path.starts_with("/static/")
        || path.starts_with("/ws/")
    {
        return Ok(next.run(req).await);
    }

    if path == "/" {
        return Ok(next.run(req).await);
    }

    let _token = req
        .extensions()
        .get::<Arc<String>>()
        .map(|t| t.as_str().to_string());

    let expected = req
        .extensions()
        .get::<AuthToken>()
        .map(|t| t.0.clone())
        .unwrap_or_default();

    let provided = extract_token(&req);
    if let Some(ref t) = provided {
        if constant_time_eq(t, &expected) {
            return Ok(next.run(req).await);
        }
    }

    Err(StatusCode::UNAUTHORIZED)
}

#[derive(Clone)]
pub struct AuthToken(pub String);

pub const COOKIE_NAME: &str = "ciu_token";
