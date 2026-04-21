pub mod config;
pub mod instances;
pub mod settings;
pub mod summarizer;
pub mod terminal;
pub mod transcript;
pub mod types;

use crate::auth::{self, COOKIE_NAME};
use crate::paths;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::Router;
use instances::AppState;
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
        crate::tmux::session::capture_pane(&config, &sid)
    })
    .await
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({"detail": "capture failed"})),
        )
    })?
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({"detail": e.to_string()})),
        )
    })?;

    let plain = String::from_utf8_lossy(&result).to_string();
    Ok(axum::Json(serde_json::json!({
        "our_sid": our_sid,
        "plain": plain,
    })))
}
