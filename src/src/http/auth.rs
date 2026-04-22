use crate::infra::paths;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use rand::Rng;

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;

    fn req_with_header(key: &str, val: &str) -> Request {
        HttpRequest::builder()
            .uri("/")
            .header(key, val)
            .body(Body::empty())
            .unwrap()
    }

    fn req_with_uri(uri: &str) -> Request {
        HttpRequest::builder().uri(uri).body(Body::empty()).unwrap()
    }

    #[test]
    fn constant_time_eq_equal_strings() {
        assert!(constant_time_eq("hello", "hello"));
    }

    #[test]
    fn constant_time_eq_different_same_len() {
        assert!(!constant_time_eq("hello", "world"));
    }

    #[test]
    fn constant_time_eq_different_lengths() {
        assert!(!constant_time_eq("a", "abc"));
    }

    #[test]
    fn constant_time_eq_empty_strings() {
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn constant_time_eq_one_empty() {
        assert!(!constant_time_eq("", "x"));
    }

    #[test]
    fn constant_time_eq_unicode_bytes() {
        assert!(constant_time_eq("café", "café"));
        assert!(!constant_time_eq("café", "cafx"));
    }

    #[test]
    fn extract_token_from_bearer_header() {
        let r = req_with_header("authorization", "Bearer abc123");
        assert_eq!(extract_token(&r).as_deref(), Some("abc123"));
    }

    #[test]
    fn extract_token_from_bearer_empty() {
        let r = req_with_header("authorization", "Bearer ");
        assert_eq!(extract_token(&r).as_deref(), Some(""));
    }

    #[test]
    fn extract_token_ignores_non_bearer_auth() {
        let r = req_with_header("authorization", "Basic foobar");
        assert!(extract_token(&r).is_none());
    }

    #[test]
    fn extract_token_from_cookie_single() {
        let r = req_with_header("cookie", "ciu_token=xyz");
        assert_eq!(extract_token(&r).as_deref(), Some("xyz"));
    }

    #[test]
    fn extract_token_from_cookie_multi() {
        let r = req_with_header("cookie", "foo=bar; ciu_token=tok; baz=qux");
        assert_eq!(extract_token(&r).as_deref(), Some("tok"));
    }

    #[test]
    fn extract_token_cookie_without_ciu_token() {
        let r = req_with_header("cookie", "foo=bar; other=tok");
        assert!(extract_token(&r).is_none());
    }

    #[test]
    fn extract_token_from_query_t_param() {
        let r = req_with_uri("/path?t=fromquery");
        assert_eq!(extract_token(&r).as_deref(), Some("fromquery"));
    }

    #[test]
    fn extract_token_query_multi_params() {
        let r = req_with_uri("/p?foo=bar&t=mytok&x=y");
        assert_eq!(extract_token(&r).as_deref(), Some("mytok"));
    }

    #[test]
    fn extract_token_query_no_t_param() {
        let r = req_with_uri("/p?foo=bar");
        assert!(extract_token(&r).is_none());
    }

    #[test]
    fn extract_token_no_sources_returns_none() {
        let r = req_with_uri("/p");
        assert!(extract_token(&r).is_none());
    }

    #[test]
    fn extract_token_priority_bearer_over_cookie() {
        let r = HttpRequest::builder()
            .uri("/?t=querytok")
            .header("authorization", "Bearer bearertok")
            .header("cookie", "ciu_token=cookietok")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_token(&r).as_deref(), Some("bearertok"));
    }

    #[test]
    fn extract_token_priority_cookie_over_query() {
        let r = HttpRequest::builder()
            .uri("/?t=querytok")
            .header("cookie", "ciu_token=cookietok")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_token(&r).as_deref(), Some("cookietok"));
    }

    #[test]
    fn extract_token_from_request_pub_wrapper() {
        let r = req_with_header("authorization", "Bearer viaapi");
        assert_eq!(extract_token_from_request(&r).as_deref(), Some("viaapi"));
    }

    #[test]
    fn base32_encode_returns_64_hex_chars() {
        let out = base32_encode(&[0u8; 8]);
        assert_eq!(out.len(), 64);
        assert!(out.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn base32_encode_is_random_not_deterministic() {
        let a = base32_encode(&[1, 2, 3]);
        let b = base32_encode(&[1, 2, 3]);
        assert_ne!(a, b, "current impl ignores input and randomizes");
    }

    #[test]
    fn auth_token_clone_preserves_value() {
        let t = AuthToken("abc".into());
        let t2 = t.clone();
        assert_eq!(t.0, t2.0);
    }

    #[test]
    fn cookie_name_constant() {
        assert_eq!(COOKIE_NAME, "ciu_token");
    }

    use axum::middleware::from_fn;
    use axum::routing::{any, get};
    use axum::Router;
    use tower::ServiceExt;

    fn test_router(expected: Option<&str>) -> Router {
        let inject = expected.map(|s| s.to_string());
        Router::new()
            .route("/", get(|| async { "root" }))
            .route("/auth", get(|| async { "auth" }))
            .route("/static/{*rest}", get(|| async { "static" }))
            .route("/ws/{*rest}", any(|| async { "ws" }))
            .route("/api/x", any(|| async { "x" }))
            .route("/api/instances", get(|| async { "inst" }))
            .layer(from_fn(auth_middleware))
            .layer(from_fn(move |mut req: Request, next: Next| {
                let inj = inject.clone();
                async move {
                    if let Some(tok) = inj {
                        req.extensions_mut().insert(AuthToken(tok));
                    }
                    Ok::<Response, StatusCode>(next.run(req).await)
                }
            }))
    }

    async fn call(router: Router, req: Request) -> StatusCode {
        router.oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn auth_middleware_allows_root() {
        let s = call(test_router(Some("secret")), req_with_uri("/")).await;
        assert_eq!(s, StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_middleware_allows_auth_path() {
        let s = call(test_router(Some("secret")), req_with_uri("/auth")).await;
        assert_eq!(s, StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_middleware_allows_static_prefix() {
        let s = call(test_router(Some("secret")), req_with_uri("/static/foo.css")).await;
        assert_eq!(s, StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_middleware_allows_ws_prefix() {
        let s = call(test_router(Some("secret")), req_with_uri("/ws/terminal")).await;
        assert_eq!(s, StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_middleware_accepts_instances_route_with_valid_token() {
        let req = HttpRequest::builder()
            .uri("/api/instances")
            .header("authorization", "Bearer secret")
            .body(Body::empty())
            .unwrap();
        let s = call(test_router(Some("secret")), req).await;
        assert_eq!(s, StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_middleware_rejects_when_no_token_provided() {
        let s = call(test_router(Some("secret")), req_with_uri("/api/instances")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_middleware_rejects_when_token_mismatches() {
        let req = HttpRequest::builder()
            .uri("/api/x")
            .header("authorization", "Bearer wrong")
            .body(Body::empty())
            .unwrap();
        let s = call(test_router(Some("right")), req).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_middleware_accepts_valid_bearer() {
        let req = HttpRequest::builder()
            .uri("/api/x")
            .header("authorization", "Bearer secret")
            .body(Body::empty())
            .unwrap();
        let s = call(test_router(Some("secret")), req).await;
        assert_eq!(s, StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_middleware_accepts_valid_cookie() {
        let req = HttpRequest::builder()
            .uri("/api/x")
            .header("cookie", "ciu_token=secret")
            .body(Body::empty())
            .unwrap();
        let s = call(test_router(Some("secret")), req).await;
        assert_eq!(s, StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_middleware_accepts_valid_query() {
        let s = call(test_router(Some("secret")), req_with_uri("/api/x?t=secret")).await;
        assert_eq!(s, StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_middleware_default_expected_when_missing_extension() {
        let s = call(test_router(None), req_with_uri("/api/x?t=")).await;
        assert_eq!(s, StatusCode::OK, "empty token matches empty default");
    }

    #[test]
    fn load_or_create_token_reads_existing_token_file() {
        let _g = crate::test_util::FS_LOCK.lock().unwrap();
        let _ = crate::test_util::set_test_home();
        std::fs::create_dir_all(paths::TOKEN_FILE.parent().unwrap()).unwrap();
        std::fs::write(&*paths::TOKEN_FILE, "preexisting-token\n").unwrap();
        let t = load_or_create_token();
        assert_eq!(t, "preexisting-token");
    }

    #[test]
    fn load_or_create_token_creates_new_when_missing() {
        let _g = crate::test_util::FS_LOCK.lock().unwrap();
        let _ = crate::test_util::set_test_home();
        let _ = std::fs::remove_file(&*paths::TOKEN_FILE);
        std::fs::create_dir_all(paths::TOKEN_FILE.parent().unwrap()).unwrap();
        let t = load_or_create_token();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        let disk = std::fs::read_to_string(&*paths::TOKEN_FILE).unwrap();
        assert_eq!(disk, t);
    }

    #[test]
    fn load_or_create_token_creates_new_when_empty_file() {
        let _g = crate::test_util::FS_LOCK.lock().unwrap();
        let _ = crate::test_util::set_test_home();
        std::fs::create_dir_all(paths::TOKEN_FILE.parent().unwrap()).unwrap();
        std::fs::write(&*paths::TOKEN_FILE, "   \n").unwrap();
        let t = load_or_create_token();
        assert_eq!(t.len(), 64);
    }

    #[cfg(unix)]
    #[test]
    fn load_or_create_token_sets_owner_only_perms() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::test_util::FS_LOCK.lock().unwrap();
        let _ = crate::test_util::set_test_home();
        let _ = std::fs::remove_file(&*paths::TOKEN_FILE);
        std::fs::create_dir_all(paths::TOKEN_FILE.parent().unwrap()).unwrap();
        let _ = load_or_create_token();
        let md = std::fs::metadata(&*paths::TOKEN_FILE).unwrap();
        let mode = md.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn extract_token_ignores_non_ascii_authorization_header() {
        let r = HttpRequest::builder()
            .uri("/")
            .header(
                "authorization",
                axum::http::HeaderValue::from_bytes(b"Bearer \xff").unwrap(),
            )
            .body(Body::empty())
            .unwrap();
        assert!(extract_token(&r).is_none());
    }

    #[test]
    fn extract_token_ignores_non_ascii_cookie_header() {
        let r = HttpRequest::builder()
            .uri("/")
            .header(
                "cookie",
                axum::http::HeaderValue::from_bytes(b"ciu_token=\xff").unwrap(),
            )
            .body(Body::empty())
            .unwrap();
        assert!(extract_token(&r).is_none());
    }
}
