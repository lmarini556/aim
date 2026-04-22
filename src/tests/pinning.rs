// Pinning / characterization tests.
// Run via `cargo test --test pinning`.
// These tests capture current observable behavior of pure-logic functions
// prior to the restructure/refactor. They must continue to pass unchanged
// after any subsequent refactor — that is their sole purpose.
//
// Intentionally restricted to the PUBLIC API. Private helpers are pinned
// indirectly or via later in-module unit tests (which count toward coverage).
// The tests here do NOT exercise filesystem, network, tmux, or PTY.

use aim_lib::auth::{constant_time_eq, extract_token_from_request};
use aim_lib::error::CiuError;
use aim_lib::services::transcript::{iso_to_epoch, summarize_tool_arg};
use aim_lib::http::dto::{
    AckBody, ConfigFileBody, ConfigWriteBody, GroupBody, InputBody, McpListBody, NewInstanceBody,
    OkResponse, OpenDashboardBody, RenameBody, SettingsBody, SignalBody, SkillCreateBody,
    SkillDeleteBody,
};
use aim_lib::tmux::session::{new_our_sid, session_name};
use aim_lib::tmux::TmuxConfig;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::json;
use std::path::PathBuf;

fn mk_config() -> TmuxConfig {
    TmuxConfig {
        tmux_bin: PathBuf::from("/nonexistent/tmux"),
        socket_name: "ciu".into(),
        name_prefix: "ciu-".into(),
    }
}

// ----- error.rs -----

#[test]
fn error_status_mapping_session_not_found() {
    let s: StatusCode = CiuError::SessionNotFound("x".into()).into();
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[test]
fn error_status_mapping_dir_not_found() {
    let s: StatusCode = CiuError::DirNotFound("x".into()).into();
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[test]
fn error_status_mapping_not_managed() {
    let s: StatusCode = CiuError::NotManaged.into();
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[test]
fn error_status_mapping_tmux_not_found() {
    let s: StatusCode = CiuError::TmuxNotFound(PathBuf::from("/x")).into();
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn error_status_mapping_tmux_command_is_500() {
    let s: StatusCode = CiuError::TmuxCommand { cmd: "c".into(), stderr: "e".into() }.into();
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn error_status_mapping_json_is_500() {
    let je = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
    let s: StatusCode = CiuError::Json(je).into();
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn error_status_mapping_io_is_500() {
    let io = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
    let s: StatusCode = CiuError::Io(io).into();
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn error_status_mapping_other_is_500() {
    let s: StatusCode = CiuError::Other("x".into()).into();
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn error_display_includes_stderr() {
    let e = CiuError::TmuxCommand { cmd: "ls".into(), stderr: "boom".into() };
    let msg = format!("{e}");
    assert!(msg.contains("ls"));
    assert!(msg.contains("boom"));
}

// ----- auth.rs -----

#[test]
fn const_time_eq_equal_strings() {
    assert!(constant_time_eq("abc", "abc"));
}

#[test]
fn const_time_eq_different_same_length() {
    assert!(!constant_time_eq("abc", "abd"));
}

#[test]
fn const_time_eq_different_length() {
    assert!(!constant_time_eq("abc", "abcd"));
    assert!(!constant_time_eq("abcd", "abc"));
}

#[test]
fn const_time_eq_empty() {
    assert!(constant_time_eq("", ""));
}

#[test]
fn extract_token_from_bearer_header() {
    let req = Request::builder()
        .uri("/api/foo")
        .header(header::AUTHORIZATION, "Bearer secret123")
        .body(Body::empty())
        .unwrap();
    assert_eq!(extract_token_from_request(&req), Some("secret123".into()));
}

#[test]
fn extract_token_ignores_non_bearer_auth() {
    let req = Request::builder()
        .uri("/api/foo")
        .header(header::AUTHORIZATION, "Basic abcd")
        .body(Body::empty())
        .unwrap();
    assert_eq!(extract_token_from_request(&req), None);
}

#[test]
fn extract_token_from_cookie() {
    let req = Request::builder()
        .uri("/api/foo")
        .header(header::COOKIE, "ciu_token=abc; other=x")
        .body(Body::empty())
        .unwrap();
    assert_eq!(extract_token_from_request(&req), Some("abc".into()));
}

#[test]
fn extract_token_from_query_param() {
    let req = Request::builder()
        .uri("/api/foo?t=from-query&z=1")
        .body(Body::empty())
        .unwrap();
    assert_eq!(extract_token_from_request(&req), Some("from-query".into()));
}

#[test]
fn extract_token_prefers_bearer_over_cookie_over_query() {
    let req = Request::builder()
        .uri("/?t=q")
        .header(header::AUTHORIZATION, "Bearer hdr")
        .header(header::COOKIE, "ciu_token=ck")
        .body(Body::empty())
        .unwrap();
    assert_eq!(extract_token_from_request(&req), Some("hdr".into()));
}

#[test]
fn extract_token_cookie_used_when_no_bearer() {
    let req = Request::builder()
        .uri("/?t=q")
        .header(header::COOKIE, "ciu_token=ck")
        .body(Body::empty())
        .unwrap();
    assert_eq!(extract_token_from_request(&req), Some("ck".into()));
}

#[test]
fn extract_token_none_when_absent() {
    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    assert_eq!(extract_token_from_request(&req), None);
}

// ----- tmux/session.rs (pure helpers) -----

#[test]
fn session_name_concatenates_prefix() {
    let cfg = mk_config();
    assert_eq!(session_name(&cfg, "abc123"), "ciu-abc123");
}

#[test]
fn session_name_custom_prefix() {
    let cfg = TmuxConfig {
        tmux_bin: PathBuf::from("/x"),
        socket_name: "s".into(),
        name_prefix: "pre_".into(),
    };
    assert_eq!(session_name(&cfg, "xyz"), "pre_xyz");
}

#[test]
fn new_our_sid_is_12_hex_chars() {
    let s = new_our_sid();
    assert_eq!(s.len(), 12);
    assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn new_our_sid_non_deterministic() {
    let a = new_our_sid();
    let b = new_our_sid();
    assert_ne!(a, b);
}

// ----- transcript: iso_to_epoch -----

#[test]
fn iso_to_epoch_none_input() {
    assert_eq!(iso_to_epoch(None), None);
}

#[test]
fn iso_to_epoch_zulu() {
    let v = iso_to_epoch(Some("2024-01-01T00:00:00Z"));
    assert!(v.is_some());
    let v = v.unwrap();
    assert!((v - 1_704_067_200.0).abs() < 1.0);
}

#[test]
fn iso_to_epoch_with_offset() {
    let v = iso_to_epoch(Some("2024-01-01T00:00:00+00:00")).unwrap();
    assert!((v - 1_704_067_200.0).abs() < 1.0);
}

#[test]
fn iso_to_epoch_with_millis() {
    let v = iso_to_epoch(Some("2024-01-01T00:00:00.500Z")).unwrap();
    assert!((v - 1_704_067_200.5).abs() < 0.01);
}

#[test]
fn iso_to_epoch_invalid_returns_none() {
    assert_eq!(iso_to_epoch(Some("not-a-date")), None);
    assert_eq!(iso_to_epoch(Some("")), None);
}

// ----- transcript: summarize_tool_arg -----

#[test]
fn summarize_tool_arg_none_tool_returns_empty() {
    assert_eq!(summarize_tool_arg(None, &json!({"command": "ls"})), "");
}

#[test]
fn summarize_tool_arg_non_object_input_returns_empty() {
    assert_eq!(summarize_tool_arg(Some("Bash"), &json!("string")), "");
    assert_eq!(summarize_tool_arg(Some("Bash"), &json!([])), "");
    assert_eq!(summarize_tool_arg(Some("Bash"), &json!(null)), "");
}

#[test]
fn summarize_tool_arg_bash_short() {
    assert_eq!(
        summarize_tool_arg(Some("Bash"), &json!({"command": "ls -la"})),
        "ls -la"
    );
}

#[test]
fn summarize_tool_arg_bash_newlines_collapsed_to_spaces() {
    assert_eq!(
        summarize_tool_arg(Some("Bash"), &json!({"command": "a\nb\nc"})),
        "a b c"
    );
}

#[test]
fn summarize_tool_arg_bash_truncates_over_120_chars_with_ellipsis() {
    let long = "x".repeat(150);
    let out = summarize_tool_arg(Some("Bash"), &json!({"command": long}));
    assert_eq!(out.chars().count(), 121); // 120 + ellipsis
    assert!(out.ends_with('\u{2026}'));
}

#[test]
fn summarize_tool_arg_read_returns_basename() {
    assert_eq!(
        summarize_tool_arg(Some("Read"), &json!({"file_path": "/a/b/c/file.rs"})),
        "file.rs"
    );
}

#[test]
fn summarize_tool_arg_edit_write_notebook_same_basename_rule() {
    for tool in ["Edit", "Write", "NotebookEdit"] {
        assert_eq!(
            summarize_tool_arg(Some(tool), &json!({"file_path": "/x/y.txt"})),
            "y.txt"
        );
    }
}

#[test]
fn summarize_tool_arg_read_no_slash_returns_as_is() {
    assert_eq!(
        summarize_tool_arg(Some("Read"), &json!({"file_path": "bare.txt"})),
        "bare.txt"
    );
}

#[test]
fn summarize_tool_arg_grep_with_path() {
    assert_eq!(
        summarize_tool_arg(
            Some("Grep"),
            &json!({"pattern": "fn main", "path": "src/"})
        ),
        "\"fn main\" in src/"
    );
}

#[test]
fn summarize_tool_arg_grep_with_glob_as_fallback_path() {
    assert_eq!(
        summarize_tool_arg(
            Some("Grep"),
            &json!({"pattern": "fn", "glob": "*.rs"})
        ),
        "\"fn\" in *.rs"
    );
}

#[test]
fn summarize_tool_arg_grep_no_path() {
    assert_eq!(
        summarize_tool_arg(Some("Grep"), &json!({"pattern": "fn"})),
        "\"fn\""
    );
}

#[test]
fn summarize_tool_arg_glob() {
    assert_eq!(
        summarize_tool_arg(Some("Glob"), &json!({"pattern": "**/*.rs"})),
        "**/*.rs"
    );
}

#[test]
fn summarize_tool_arg_webfetch() {
    assert_eq!(
        summarize_tool_arg(Some("WebFetch"), &json!({"url": "https://x.y"})),
        "https://x.y"
    );
}

#[test]
fn summarize_tool_arg_websearch() {
    assert_eq!(
        summarize_tool_arg(Some("WebSearch"), &json!({"query": "rust"})),
        "\"rust\""
    );
}

#[test]
fn summarize_tool_arg_task_description_truncated_to_80_chars() {
    let long = "a".repeat(100);
    let out = summarize_tool_arg(Some("Task"), &json!({"description": long}));
    assert_eq!(out.chars().count(), 80);
}

#[test]
fn summarize_tool_arg_task_falls_back_to_subagent_type() {
    assert_eq!(
        summarize_tool_arg(Some("Task"), &json!({"subagent_type": "planner"})),
        "planner"
    );
    // Agent is an alias for Task
    assert_eq!(
        summarize_tool_arg(Some("Agent"), &json!({"description": "foo"})),
        "foo"
    );
}

#[test]
fn summarize_tool_arg_todowrite_counts_todos() {
    assert_eq!(
        summarize_tool_arg(
            Some("TodoWrite"),
            &json!({"todos": [1, 2, 3]})
        ),
        "3 todos"
    );
    assert_eq!(
        summarize_tool_arg(Some("TodoWrite"), &json!({})),
        "0 todos"
    );
}

#[test]
fn summarize_tool_arg_unknown_tool_returns_empty() {
    assert_eq!(
        summarize_tool_arg(Some("UnknownTool"), &json!({"x": 1})),
        ""
    );
}

// ----- types.rs serde round-trips -----

#[test]
fn rename_body_round_trip() {
    let b = RenameBody { name: "foo".into() };
    let s = serde_json::to_string(&b).unwrap();
    let back: RenameBody = serde_json::from_str(&s).unwrap();
    assert_eq!(back.name, "foo");
}

#[test]
fn group_body_optional_deserializes() {
    let b: GroupBody = serde_json::from_str(r#"{"group": null}"#).unwrap();
    assert!(b.group.is_none());
    let b: GroupBody = serde_json::from_str(r#"{"group": "a"}"#).unwrap();
    assert_eq!(b.group.as_deref(), Some("a"));
}

#[test]
fn signal_body_default_is_term() {
    let b: SignalBody = serde_json::from_str("{}").unwrap();
    assert_eq!(b.signal.as_deref(), Some("TERM"));
}

#[test]
fn new_instance_body_minimal() {
    let b: NewInstanceBody = serde_json::from_str(r#"{"cwd": "/tmp"}"#).unwrap();
    assert_eq!(b.cwd, "/tmp");
    assert!(b.command.is_none());
    assert!(b.mcps.is_none());
    assert!(b.mcp_source.is_none());
    assert!(b.name.is_none());
    assert!(b.group.is_none());
}

#[test]
fn input_body_default_submit_is_true() {
    let b: InputBody = serde_json::from_str(r#"{"text": "hi"}"#).unwrap();
    assert_eq!(b.text, "hi");
    assert_eq!(b.submit, Some(true));
}

#[test]
fn ack_body_parses_float_timestamp() {
    let b: AckBody = serde_json::from_str(r#"{"timestamp": 1700000000.5}"#).unwrap();
    assert!((b.timestamp - 1_700_000_000.5).abs() < 0.001);
}

#[test]
fn config_file_body_round_trip() {
    let b: ConfigFileBody = serde_json::from_str(r#"{"path": "~/x"}"#).unwrap();
    assert_eq!(b.path, "~/x");
}

#[test]
fn config_write_body_round_trip() {
    let b: ConfigWriteBody =
        serde_json::from_str(r#"{"path": "p", "content": "c"}"#).unwrap();
    assert_eq!(b.path, "p");
    assert_eq!(b.content, "c");
}

#[test]
fn skill_create_body_round_trip() {
    let b: SkillCreateBody =
        serde_json::from_str(r#"{"scope": "global", "name": "foo"}"#).unwrap();
    assert_eq!(b.scope, "global");
    assert_eq!(b.name, "foo");
}

#[test]
fn skill_delete_body_round_trip() {
    let b: SkillDeleteBody = serde_json::from_str(r#"{"path": "p"}"#).unwrap();
    assert_eq!(b.path, "p");
}

#[test]
fn mcp_list_body_round_trip() {
    let b: McpListBody = serde_json::from_str(r#"{"path": "p"}"#).unwrap();
    assert_eq!(b.path, "p");
}

#[test]
fn open_dashboard_body_optional_sid() {
    let b: OpenDashboardBody = serde_json::from_str("{}").unwrap();
    assert!(b.sid.is_none());
    let b: OpenDashboardBody = serde_json::from_str(r#"{"sid": "x"}"#).unwrap();
    assert_eq!(b.sid.as_deref(), Some("x"));
}

#[test]
fn settings_body_all_optional() {
    let b: SettingsBody = serde_json::from_str("{}").unwrap();
    assert!(b.sound.is_none());
    assert!(b.banner_ttl.is_none());
    assert!(b.poll_interval.is_none());

    let b: SettingsBody = serde_json::from_str(
        r#"{"sound": true, "banner_ttl": 30, "poll_interval": 2}"#,
    )
    .unwrap();
    assert_eq!(b.sound, Some(true));
    assert_eq!(b.banner_ttl, Some(30));
    assert_eq!(b.poll_interval, Some(2));
}

#[test]
fn ok_response_default_ok_true() {
    let r = OkResponse::new();
    assert!(r.ok);
    let s = serde_json::to_string(&r).unwrap();
    assert_eq!(s, r#"{"ok":true}"#);
}

#[test]
fn ok_response_default_impl() {
    let r: OkResponse = OkResponse::default();
    assert!(!r.ok);
}
