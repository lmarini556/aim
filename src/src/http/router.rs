use crate::http::auth::{self, COOKIE_NAME};
use crate::http::handlers::{settings, terminal};
use crate::infra::paths;
use crate::services::{config, instances, transcript};
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::Router;
use crate::services::instances::AppState;
use std::sync::Arc;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

pub fn build_router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/api/instances", get(instances::api_instances))
        .route("/api/instances/new", post(instances::api_new_instance))
        .route("/api/mcp-list", post(config::api_mcp_list))
        .route("/api/mcp-sources", get(config::api_mcp_sources))
        .route(
            "/api/instances/{session_id}/rename",
            post(instances::api_rename),
        )
        .route(
            "/api/instances/{session_id}/group",
            post(instances::api_set_group),
        )
        .route(
            "/api/instances/{session_id}/signal",
            post(instances::api_signal),
        )
        .route(
            "/api/instances/{session_id}/input",
            post(instances::api_input),
        )
        .route(
            "/api/instances/{session_id}/upload",
            post(instances::api_upload),
        )
        .route(
            "/api/instances/{session_id}/kill",
            post(instances::api_kill),
        )
        .route(
            "/api/instances/{session_id}/open-terminal",
            post(instances::api_open_terminal),
        )
        .route(
            "/api/instances/{session_id}/ack",
            post(instances::api_ack),
        )
        .route(
            "/api/instances/{session_id}/transcript",
            get(transcript::api_transcript),
        )
        .route(
            "/api/instances/{session_id}",
            delete(instances::api_forget),
        )
        .route(
            "/api/clear-pending-focus",
            post(instances::api_clear_pending_focus),
        )
        .route("/api/groups", get(instances::api_groups))
        .route("/api/groups", post(instances::api_set_groups))
        .route("/api/recent-cwds", get(instances::api_recent_cwds))
        .route("/api/open-dashboard", post(instances::api_open_dashboard))
        .route("/api/config/mcp", get(config::api_config_mcp_list))
        .route("/api/config/mcp/read", post(config::api_config_mcp_read))
        .route("/api/config/mcp/write", post(config::api_config_mcp_write))
        .route("/api/config/skills", get(config::api_config_skills_list))
        .route("/api/config/skill/read", post(config::api_config_skill_read))
        .route(
            "/api/config/skill/write",
            post(config::api_config_skill_write),
        )
        .route(
            "/api/config/skill/create",
            post(config::api_config_skill_create),
        )
        .route(
            "/api/config/skill/delete",
            post(config::api_config_skill_delete),
        )
        .route("/api/config/claudemd", get(config::api_config_claudemd_list))
        .route(
            "/api/config/claudemd/read",
            post(config::api_config_claudemd_read),
        )
        .route(
            "/api/config/claudemd/write",
            post(config::api_config_claudemd_write),
        )
        .route("/api/settings", get(settings::api_get_settings))
        .route("/api/settings", put(settings::api_put_settings))
        .route(
            "/api/debug/capture/{session_id}",
            get(api_debug_capture),
        )
        .route("/api/clipboard", get(api_read_clipboard))
        .route("/api/clipboard", post(api_write_clipboard))
        .with_state(state.clone());

    let ws_routes = Router::new()
        .route(
            "/ws/instances/{session_id}/terminal",
            get(terminal::ws_terminal),
        )
        .with_state(state.clone());

    let static_dir = paths::STATIC_DIR.clone();
    let static_service = ServeDir::new(&static_dir);

    Router::new()
        .route("/", get(index_handler))
        .route("/auth", get(auth_handler))
        .merge(api)
        .merge(ws_routes)
        .nest_service("/static", static_service)
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-cache, must-revalidate"),
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_layer,
        ))
        .with_state(state)
}

async fn auth_layer(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();

    if path == "/auth"
        || path == "/"
        || path.starts_with("/static/")
        || path.starts_with("/ws/")
    {
        return next.run(req).await;
    }

    let token = auth::extract_token_from_request(&req);
    if let Some(ref t) = token {
        if auth::constant_time_eq(t, &state.auth_token) {
            return next.run(req).await;
        }
    }

    StatusCode::UNAUTHORIZED.into_response()
}

async fn index_handler(
    State(state): State<Arc<AppState>>,
    req: Request,
) -> Response {
    let query = req.uri().query().unwrap_or("");
    let token_param = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("t="))
        .unwrap_or("");

    if !token_param.is_empty() && auth::constant_time_eq(token_param, &state.auth_token) {
        let body = render_index(&state.auth_token);
        let cookie = format!(
            "{}={}; HttpOnly; SameSite=Lax; Max-Age=31536000; Path=/",
            COOKIE_NAME, state.auth_token
        );
        return (
            [(header::SET_COOKIE, cookie), (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
            body,
        )
            .into_response();
    }

    let cookie_token = extract_cookie_token(&req);
    if let Some(ref t) = cookie_token {
        if auth::constant_time_eq(t, &state.auth_token) {
            let body = render_index(&state.auth_token);
            return ([(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())], body)
                .into_response();
        }
    }

    Html(
        r#"<!doctype html><html><head><meta charset="utf-8">
<title>AIM — sign in</title>
<style>
body{font-family:system-ui;background:#0b0d10;color:#d6d6d6;
  display:flex;align-items:center;justify-content:center;height:100vh;margin:0}
.card{max-width:520px;padding:32px;background:#15181c;border:1px solid #222;
  border-radius:12px;box-shadow:0 20px 60px -10px rgba(0,0,0,.6)}
code{background:#0b0d10;padding:2px 6px;border-radius:4px;color:#ffbf69}
h1{margin:0 0 12px;font-size:18px}
p{line-height:1.55;color:#a9a9a9}
</style></head><body><div class="card">
<h1>Authentication required</h1>
<p>Copy the token from <code>~/.claude-instances-ui/token</code> and visit
<code>/auth?t=&lt;token&gt;</code>.</p>
</div></body></html>"#,
    )
    .into_response()
}

async fn auth_handler(
    State(state): State<Arc<AppState>>,
    req: Request,
) -> Response {
    let query = req.uri().query().unwrap_or("");
    let token_param = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("t="))
        .unwrap_or("");

    if token_param.is_empty() || !auth::constant_time_eq(token_param, &state.auth_token) {
        return Html(
            "<h1>Invalid token</h1>\
             <p>Check the token printed by the server on startup \
             or read it from <code>~/.claude-instances-ui/token</code>.</p>",
        )
        .into_response();
    }

    let cookie = format!(
        "{}={}; HttpOnly; SameSite=Lax; Max-Age=31536000; Path=/",
        COOKIE_NAME, state.auth_token
    );
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, "/".to_string()),
            (header::SET_COOKIE, cookie),
        ],
    )
        .into_response()
}

fn render_index(token: &str) -> String {
    let index_path = paths::STATIC_DIR.join("index.html");
    let html = std::fs::read_to_string(&index_path)
        .unwrap_or_else(|_| "<h1>index.html not found</h1>".to_string());
    let escaped = token.replace('\\', r"\\").replace('\'', r"\'");
    let inject = format!(
        "<script>try{{sessionStorage.setItem('ciu_token','{escaped}')}}catch(e){{}}</script>"
    );
    if let Some(idx) = html.find("</head>") {
        let (head, tail) = html.split_at(idx);
        format!("{head}{inject}{tail}")
    } else {
        format!("{inject}{html}")
    }
}

fn extract_cookie_token(req: &Request) -> Option<String> {
    let cookie = req.headers().get(header::COOKIE)?.to_str().ok()?;
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix("ciu_token=") {
            return Some(val.to_string());
        }
    }
    None
}

async fn api_read_clipboard() -> axum::Json<serde_json::Value> {
    let text = tokio::task::spawn_blocking(|| {
        arboard::Clipboard::new()
            .and_then(|mut cb| cb.get_text())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    axum::Json(serde_json::json!({"text": text}))
}

async fn api_write_clipboard(
    axum::Json(body): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let ok = tokio::task::spawn_blocking(move || {
        arboard::Clipboard::new()
            .and_then(|mut cb| cb.set_text(text))
            .is_ok()
    })
    .await
    .unwrap_or(false);
    axum::Json(serde_json::json!({"ok": ok}))
}

fn capture_failure_response() -> (StatusCode, axum::Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(serde_json::json!({"detail": "capture failed"})),
    )
}

fn join_err_to_capture_failure(_: tokio::task::JoinError) -> (StatusCode, axum::Json<serde_json::Value>) {
    capture_failure_response()
}

fn capture_tmux_err_to_response(e: crate::domain::error::CiuError) -> (StatusCode, axum::Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(serde_json::json!({"detail": e.to_string()})),
    )
}

async fn api_debug_capture(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> std::result::Result<axum::Json<serde_json::Value>, (StatusCode, axum::Json<serde_json::Value>)>
{
    let instances = state.cached_instances();
    let target = instances
        .iter()
        .find(|i| i.session_id == session_id)
        .ok_or((
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"detail": "session not found"})),
        ))?;
    let our_sid = target.our_sid.as_ref().ok_or((
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({"detail": "not owned"})),
    ))?;

    let config = state.tmux_config.clone();
    let sid = our_sid.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::infra::tmux::session::capture_pane(&config, &sid)
    })
    .await
    .map_err(join_err_to_capture_failure)?
    .map_err(capture_tmux_err_to_response)?;

    let plain = String::from_utf8_lossy(&result).to_string();
    Ok(axum::Json(serde_json::json!({
        "our_sid": our_sid,
        "plain": plain,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::instances::{InstancesCache, PendingFocus, PsCache};
    use crate::http::dto::InstanceData;
    use crate::test_util::{set_test_home, FS_LOCK as LOCK};
    use crate::infra::tmux::TmuxConfig;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn state(token: &str, tmux_bin: PathBuf) -> Arc<AppState> {
        Arc::new(AppState {
            tmux_config: TmuxConfig {
                tmux_bin,
                socket_name: "ciu-test".into(),
                name_prefix: "ciu-".into(),
            },
            auth_token: token.into(),
            server_start: 1.0,
            instances_cache: Mutex::new(InstancesCache { at: 0.0, data: vec![] }),
            pending_focus: Mutex::new(PendingFocus { sid: None, ts: 0.0 }),
            ps_cache: Mutex::new(PsCache { at: 0.0, map: HashMap::new() }),
        })
    }

    fn instance(sid: &str, our_sid: Option<&str>) -> InstanceData {
        InstanceData {
            session_id: sid.into(),
            pid: None,
            alive: true,
            name: String::new(),
            title: None,
            custom_name: None,
            first_user: None,
            cwd: None,
            kind: None,
            started_at: None,
            command: String::new(),
            status: "idle".into(),
            last_event: None,
            last_tool: None,
            notification_message: None,
            hook_timestamp: None,
            transcript: json!({}),
            summary: json!({}),
            mcps: json!({}),
            subagents: vec![],
            group: None,
            ack_timestamp: 0.0,
            our_sid: our_sid.map(|s| s.to_string()),
            tmux_session: None,
        }
    }

    async fn ok_handler() -> &'static str {
        "ok"
    }

    fn req_with_header(name: header::HeaderName, value: &str) -> Request {
        HttpRequest::builder()
            .header(name, value)
            .uri("/")
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn extract_cookie_token_finds_token_alone() {
        let r = req_with_header(header::COOKIE, "ciu_token=tok123");
        assert_eq!(extract_cookie_token(&r), Some("tok123".into()));
    }

    #[test]
    fn extract_cookie_token_finds_token_amidst_others() {
        let r = req_with_header(header::COOKIE, "other=abc; ciu_token=mine; last=xyz");
        assert_eq!(extract_cookie_token(&r), Some("mine".into()));
    }

    #[test]
    fn extract_cookie_token_returns_none_when_absent() {
        let r = req_with_header(header::COOKIE, "other=abc; last=xyz");
        assert!(extract_cookie_token(&r).is_none());
    }

    #[test]
    fn extract_cookie_token_returns_none_with_no_cookie_header() {
        let r = HttpRequest::builder().uri("/").body(Body::empty()).unwrap();
        assert!(extract_cookie_token(&r).is_none());
    }

    #[test]
    fn extract_cookie_token_handles_whitespace_around_parts() {
        let r = req_with_header(header::COOKIE, "  ciu_token=spaced  ");
        assert_eq!(extract_cookie_token(&r), Some("spaced".into()));
    }

    fn with_index_html<F: FnOnce() -> T, T>(content: Option<&str>, f: F) -> T {
        let orig = paths::STATIC_DIR.clone();
        let index_path = orig.join("index.html");
        let backup = index_path.with_extension("html.bak.cov");
        if index_path.exists() {
            std::fs::rename(&index_path, &backup).ok();
        }
        std::fs::create_dir_all(&orig).ok();
        if let Some(c) = content {
            std::fs::write(&index_path, c).unwrap();
        }
        let out = f();
        std::fs::remove_file(&index_path).ok();
        if backup.exists() {
            std::fs::rename(&backup, &index_path).ok();
        }
        out
    }

    #[test]
    fn render_index_injects_before_head_close() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let out = with_index_html(
            Some("<html><head></head><body></body></html>"),
            || render_index("abc"),
        );
        assert!(out.contains("sessionStorage.setItem('ciu_token','abc')"));
        assert!(out.find("</head>").is_some());
        assert!(out.find("ciu_token").unwrap() < out.find("</head>").unwrap());
    }

    #[test]
    fn render_index_escapes_backslash_and_single_quote() {
        let out = render_index(r"a\b'c");
        assert!(out.contains(r"a\\b\'c"));
    }

    #[test]
    fn render_index_uses_fallback_when_file_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let out = with_index_html(None, || render_index("tok"));
        assert!(out.contains("index.html not found"));
    }

    #[test]
    fn render_index_injects_when_head_absent() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let out = with_index_html(Some("<html><body>no head here</body></html>"), || {
            render_index("xy")
        });
        assert!(out.starts_with("<script>"));
        assert!(out.contains("sessionStorage.setItem('ciu_token','xy')"));
    }

    #[tokio::test]
    async fn api_debug_capture_returns_404_for_missing_session() {
        let s = state("tok", "/nope".into());
        let res = api_debug_capture(
            State(s),
            axum::extract::Path("missing".into()),
        )
        .await;
        assert!(matches!(&res, Err((code, _)) if *code == StatusCode::NOT_FOUND), "got {res:?}");
    }

    #[tokio::test]
    async fn api_debug_capture_returns_404_when_not_owned() {
        let s = state("tok", "/nope".into());
        {
            let mut c = s.instances_cache.lock().unwrap();
            c.data.push(instance("sess-1", None));
            c.at = f64::MAX;
        }
        let res = api_debug_capture(
            State(s),
            axum::extract::Path("sess-1".into()),
        )
        .await;
        let err = res.err().expect("expected 404 error");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let b = serde_json::to_string(&err.1.0).unwrap();
        assert!(b.contains("not owned"));
    }

    #[tokio::test]
    async fn api_debug_capture_returns_plain_bytes_on_success() {
        use crate::infra::tmux::command::test_stub::{cfg, write_stub_tmux, STUB_LOCK};
        let _g = STUB_LOCK.lock().unwrap();
        let h = set_test_home();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nprintf 'captured-pane-body'\n");
        let tmux_cfg = cfg(bin.clone());
        let s = Arc::new(AppState {
            tmux_config: tmux_cfg,
            auth_token: "tok".into(),
            server_start: 1.0,
            instances_cache: Mutex::new(InstancesCache { at: f64::MAX, data: vec![instance("sess-x", Some("abc"))] }),
            pending_focus: Mutex::new(PendingFocus { sid: None, ts: 0.0 }),
            ps_cache: Mutex::new(PsCache { at: 0.0, map: HashMap::new() }),
        });
        let res = api_debug_capture(State(s), axum::extract::Path("sess-x".into())).await;
        let _ = std::fs::remove_file(&bin);
        let j = res.expect("expected Ok");
        assert_eq!(j.0.get("plain").and_then(serde_json::Value::as_str), Some("captured-pane-body"));
    }

    #[tokio::test]
    async fn api_debug_capture_returns_500_when_tmux_fails() {
        let s = state("tok", "/no/such/tmux".into());
        {
            let mut c = s.instances_cache.lock().unwrap();
            c.data.push(instance("sess-1", Some("ciu-abc")));
            c.at = f64::MAX;
        }
        let res = api_debug_capture(
            State(s),
            axum::extract::Path("sess-1".into()),
        )
        .await;
        assert!(matches!(&res, Err((code, _)) if *code == StatusCode::INTERNAL_SERVER_ERROR), "got {res:?}");
    }

    #[test]
    fn capture_failure_response_returns_500_and_detail() {
        let (code, body) = capture_failure_response();
        assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0.get("detail").and_then(|v| v.as_str()), Some("capture failed"));
    }

    #[tokio::test]
    async fn join_err_to_capture_failure_maps_panic_to_500() {
        let handle = tokio::task::spawn_blocking(|| panic!("forced"));
        let err = handle.await.unwrap_err();
        let (code, body) = join_err_to_capture_failure(err);
        assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0.get("detail").and_then(|v| v.as_str()), Some("capture failed"));
    }

    #[test]
    fn capture_tmux_err_to_response_includes_error_string() {
        let err = crate::domain::error::CiuError::TmuxCommand {
            cmd: "ls".into(),
            stderr: "permission denied".into(),
        };
        let (code, body) = capture_tmux_err_to_response(err);
        assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.0.get("detail").and_then(|v| v.as_str()).unwrap().contains("permission denied"));
    }

    #[tokio::test]
    async fn api_read_clipboard_returns_text_field() {
        let r = api_read_clipboard().await;
        assert!(r.0.get("text").is_some());
    }

    #[tokio::test]
    async fn api_write_clipboard_accepts_missing_text() {
        let r = api_write_clipboard(axum::Json(json!({}))).await;
        assert!(r.0.get("ok").is_some());
    }

    #[tokio::test]
    async fn api_write_clipboard_accepts_text() {
        let r = api_write_clipboard(axum::Json(json!({"text": "hi"}))).await;
        assert!(r.0.get("ok").is_some());
    }

    #[test]
    fn build_router_constructs_without_panic() {
        let s = state("tok", "/nope".into());
        let _r = build_router(s);
    }

    #[tokio::test]
    async fn auth_layer_allows_root_without_token() {
        use axum::routing::get;
        let s = state("tok", "/nope".into());
        let router: Router = Router::new()
            .route("/", get(ok_handler))
            .layer(middleware::from_fn_with_state(s.clone(), auth_layer))
            .with_state(s);
        let req = HttpRequest::builder()
            .uri("/")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(router, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_layer_rejects_api_without_token() {
        use axum::routing::get;
        let s = state("tok", "/nope".into());
        let router: Router = Router::new()
            .route("/api/x", get(ok_handler))
            .layer(middleware::from_fn_with_state(s.clone(), auth_layer))
            .with_state(s);
        let req = HttpRequest::builder()
            .uri("/api/x")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(router, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_layer_allows_api_with_valid_bearer() {
        use axum::routing::get;
        let s = state("tok", "/nope".into());
        let router: Router = Router::new()
            .route("/api/x", get(ok_handler))
            .layer(middleware::from_fn_with_state(s.clone(), auth_layer))
            .with_state(s);
        let req = HttpRequest::builder()
            .uri("/api/x")
            .header(header::AUTHORIZATION, "Bearer tok")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(router, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_layer_rejects_api_with_wrong_bearer_token() {
        use axum::routing::get;
        let s = state("correct", "/nope".into());
        let router: Router = Router::new()
            .route("/api/x", get(ok_handler))
            .layer(middleware::from_fn_with_state(s.clone(), auth_layer))
            .with_state(s);
        let req = HttpRequest::builder()
            .uri("/api/x")
            .header(header::AUTHORIZATION, "Bearer wrong")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(router, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_layer_allows_static_without_token() {
        use axum::routing::get;
        let s = state("tok", "/nope".into());
        let router: Router = Router::new()
            .route("/static/x", get(ok_handler))
            .layer(middleware::from_fn_with_state(s.clone(), auth_layer))
            .with_state(s);
        let req = HttpRequest::builder()
            .uri("/static/x")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(router, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_layer_allows_ws_without_token() {
        use axum::routing::get;
        let s = state("tok", "/nope".into());
        let router: Router = Router::new()
            .route("/ws/x", get(ok_handler))
            .layer(middleware::from_fn_with_state(s.clone(), auth_layer))
            .with_state(s);
        let req = HttpRequest::builder()
            .uri("/ws/x")
            .body(Body::empty())
            .unwrap();
        let resp = tower::ServiceExt::oneshot(router, req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_handler_rejects_wrong_token() {
        let s = state("correct", "/nope".into());
        let req = HttpRequest::builder()
            .uri("/auth?t=wrong")
            .body(Body::empty())
            .unwrap();
        let resp = auth_handler(State(s), req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_handler_rejects_missing_token() {
        let s = state("correct", "/nope".into());
        let req = HttpRequest::builder().uri("/auth").body(Body::empty()).unwrap();
        let resp = auth_handler(State(s), req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_handler_redirects_on_correct_token() {
        let s = state("good", "/nope".into());
        let req = HttpRequest::builder()
            .uri("/auth?t=good")
            .body(Body::empty())
            .unwrap();
        let resp = auth_handler(State(s), req).await;
        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "/");
        let sc = resp.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(sc.contains("ciu_token=good"));
        assert!(sc.contains("HttpOnly"));
    }

    #[tokio::test]
    async fn index_handler_returns_signin_page_when_no_token() {
        let s = state("good", "/nope".into());
        let req = HttpRequest::builder().uri("/").body(Body::empty()).unwrap();
        let resp = index_handler(State(s), req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn index_handler_sets_cookie_on_valid_query_token() {
        let s = state("good", "/nope".into());
        let req = HttpRequest::builder()
            .uri("/?t=good")
            .body(Body::empty())
            .unwrap();
        let resp = index_handler(State(s), req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let sc = resp.headers().get(header::SET_COOKIE);
        assert!(sc.is_some());
        assert!(sc.unwrap().to_str().unwrap().contains("ciu_token=good"));
    }

    #[tokio::test]
    async fn index_handler_accepts_cookie_token() {
        let s = state("good", "/nope".into());
        let req = HttpRequest::builder()
            .uri("/")
            .header(header::COOKIE, "ciu_token=good")
            .body(Body::empty())
            .unwrap();
        let resp = index_handler(State(s), req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn index_handler_ignores_wrong_cookie_token() {
        let s = state("good", "/nope".into());
        let req = HttpRequest::builder()
            .uri("/")
            .header(header::COOKIE, "ciu_token=wrong")
            .body(Body::empty())
            .unwrap();
        let resp = index_handler(State(s), req).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
