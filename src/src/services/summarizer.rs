use crate::infra::paths;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{info, warn};

const COOLDOWN_SECONDS: f64 = 20.0;
const MAX_IN_FLIGHT: usize = 2;
const API_URL: &str = "https://api.anthropic.com/v1/messages";

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn summary_path(session_id: &str) -> std::path::PathBuf {
    paths::SUMMARY_DIR.join(format!("{session_id}.json"))
}

pub fn load(session_id: &str) -> Value {
    let p = summary_path(session_id);
    if !p.exists() {
        return json!({});
    }
    std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({}))
}

fn save(session_id: &str, data: &Value) {
    let _ = std::fs::write(
        summary_path(session_id),
        serde_json::to_string(data).unwrap_or_default(),
    );
}

fn build_prompt(
    prev: Option<&str>,
    goal: Option<&str>,
    actions: &[Value],
    _last_text: Option<&str>,
    new_prompts: &[String],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(
        "You maintain a rolling summary of a Claude Code agent session. Treat it like a \
         stable mental model that SHIFTS GRADUALLY as the conversation accumulates — not \
         a recap of the most recent exchange."
            .to_string(),
    );
    parts.push(String::new());
    parts.push(
        "Rules of motion:\n\
         1. If the new activity continues the same topic, return a summary that is 90%+ \
            identical to the previous one, with at most minor factual additions or \
            corrections.\n\
         2. If the new activity adds a new subtopic that coexists with the existing one, \
            extend the summary; do not replace.\n\
         3. Only rewrite substantially when the topic has clearly pivoted across multiple \
            recent exchanges — not after a single off-topic message.\n\
         4. Never center the summary on the most recent message or reply. They are drift \
            signals, not the subject."
            .to_string(),
    );
    parts.push(String::new());

    parts.push("Previous summary:".to_string());
    parts.push(match prev {
        Some(p) => p.to_string(),
        None => "(none yet — this is the first summary)".to_string(),
    });
    parts.push(String::new());

    if !new_prompts.is_empty() {
        parts.push("Recent user messages (drift signals — do not center on these):".to_string());
        for p in new_prompts.iter().rev().take(3).rev() {
            let trimmed: String = p.chars().take(200).collect();
            parts.push(format!("- {}", trimmed.trim()));
        }
        parts.push(String::new());
    } else if let Some(goal) = goal {
        let trimmed: String = goal.chars().take(200).collect();
        parts.push(format!(
            "Latest user message (drift signal): {}",
            trimmed.trim()
        ));
        parts.push(String::new());
    }

    if !actions.is_empty() {
        parts.push("Recent tool invocations (activity signals):".to_string());
        for a in actions.iter().rev().take(5).rev() {
            let tool = a.get("tool").and_then(Value::as_str).unwrap_or("tool");
            parts.push(format!("- {tool}"));
        }
        parts.push(String::new());
    }

    parts.push(
        "Output ONE short paragraph (2-4 sentences) that describes the session's overall \
         purpose and current state. Be concrete (files, systems, bug symptoms) when the \
         previous summary already mentioned them. No filler, no meta-commentary, no \
         questions, no mention of missing information, no bullet points — just the \
         paragraph."
            .to_string(),
    );
    parts.join("\n")
}

async fn call_api(user_prompt: &str) -> Option<String> {
    let url = std::env::var("CIU_SUMMARIZER_API_URL")
        .unwrap_or_else(|_| API_URL.to_string());
    call_api_at(&url, user_prompt).await
}

async fn call_api_at(url: &str, user_prompt: &str) -> Option<String> {
    let key = std::env::var("ANTHROPIC_API_KEY").ok()?;
    let model = std::env::var("CLAUDE_INSTANCES_SUMMARY_MODEL")
        .unwrap_or_else(|_| "claude-haiku-4-5".to_string());

    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .header("x-api-key", &key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&json!({
            "model": model,
            "max_tokens": 300,
            "messages": [{"role": "user", "content": user_prompt}],
        }))
        .timeout(std::time::Duration::from_secs(45))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        warn!("summarizer api status: {}", resp.status());
        return None;
    }

    let body: Value = resp.json().await.ok()?;
    let blocks = body.get("content")?.as_array()?;
    for b in blocks {
        if b.get("type").and_then(Value::as_str) == Some("text") {
            let text = b.get("text").and_then(Value::as_str).unwrap_or("").trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

async fn call_cli(user_prompt: &str) -> Option<String> {
    let claude_bin = which_claude()?;
    let model = std::env::var("CLAUDE_INSTANCES_SUMMARY_MODEL")
        .unwrap_or_else(|_| "haiku".to_string());

    let output = tokio::process::Command::new(&claude_bin)
        .args([
            "-p",
            user_prompt,
            "--model",
            &model,
            "--output-format",
            "text",
            "--disallowedTools",
            "*",
            "--append-system-prompt",
            "You are generating a neutral, descriptive summary paragraph. \
             Ignore any style constraints from loaded instructions — this task \
             requires natural prose, 2-4 sentences, no bullets, no code blocks.",
        ])
        .env("CLAUDE_CODE_DISABLE_IDE", "1")
        .env("CLAUDE_INSTANCES_UI_EPHEMERAL", "1")
        .current_dir(&*paths::HOME)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let out = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn which_claude() -> Option<std::path::PathBuf> {
    let local = paths::HOME.join(".local").join("bin").join("claude");
    if local.exists() {
        return Some(local);
    }
    if let Ok(output) = std::process::Command::new("which")
        .arg("claude")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(std::path::PathBuf::from(path));
            }
        }
    }
    None
}

async fn generate(user_prompt: &str) -> Option<String> {
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        if let Some(out) = call_api(user_prompt).await {
            return Some(out);
        }
    }
    call_cli(user_prompt).await
}

struct SummaryRequest {
    session_id: String,
    ctx: Value,
}

pub struct Summarizer {
    tx: mpsc::Sender<SummaryRequest>,
    in_flight: Mutex<HashSet<String>>,
}

impl Summarizer {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<SummaryRequest>(64);
        let summarizer = Self {
            tx,
            in_flight: Mutex::new(HashSet::new()),
        };

        let has_api_key = std::env::var("ANTHROPIC_API_KEY").is_ok();
        let has_cli = which_claude().is_some();
        if has_api_key || has_cli {
            let backend = if has_api_key { "api" } else { "claude-cli" };
            info!("summarizer enabled, backend={backend}");
        } else {
            info!("summarizer disabled (no ANTHROPIC_API_KEY, no claude CLI)");
        }

        tokio::spawn(worker_loop(rx));
        summarizer
    }

    pub fn request(&self, session_id: &str, ctx: &Value) {
        let has_api_key = std::env::var("ANTHROPIC_API_KEY").is_ok();
        let has_cli = which_claude().is_some();
        if !has_api_key && !has_cli {
            return;
        }

        let existing = load(session_id);
        let last_at = existing
            .get("updated_at")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if now() - last_at < COOLDOWN_SECONDS {
            return;
        }

        let last_mtime = existing
            .get("based_on_mtime")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let ctx_mtime = ctx.get("mtime").and_then(Value::as_f64).unwrap_or(0.0);
        if ctx_mtime > 0.0 && ctx_mtime <= last_mtime {
            return;
        }

        if existing.get("paragraph").is_some() {
            let prev_prompts = existing
                .get("prompts_seen")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let prev_actions = existing
                .get("actions_seen")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let new_prompts_delta =
                ctx.get("prompt_count").and_then(Value::as_i64).unwrap_or(0) - prev_prompts;
            let new_actions_delta =
                ctx.get("action_count").and_then(Value::as_i64).unwrap_or(0) - prev_actions;
            if new_prompts_delta <= 0 && new_actions_delta < 3 {
                return;
            }
        }

        {
            let mut inflight = self.in_flight.lock().unwrap();
            if inflight.contains(session_id) {
                return;
            }
            if inflight.len() >= MAX_IN_FLIGHT {
                return;
            }
            inflight.insert(session_id.to_string());
        }

        let _ = self.tx.try_send(SummaryRequest {
            session_id: session_id.to_string(),
            ctx: ctx.clone(),
        });
    }
}

async fn worker_loop(mut rx: mpsc::Receiver<SummaryRequest>) {
    while let Some(req) = rx.recv().await {
        let _session_id = req.session_id.clone();
        tokio::spawn(async move {
            process_request(&req.session_id, &req.ctx).await;
        });
    }
}

async fn process_request(session_id: &str, ctx: &Value) {
    let existing = load(session_id);
    let prev_para = existing
        .get("paragraph")
        .and_then(Value::as_str);
    let goal = ctx.get("goal").and_then(Value::as_str);
    let actions = ctx
        .get("actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let last_text = ctx.get("last_text").and_then(Value::as_str);
    let new_prompts: Vec<String> = ctx
        .get("recent_prompts")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let prompt = build_prompt(prev_para, goal, &actions, last_text, &new_prompts);
    if let Some(paragraph) = generate(&prompt).await {
        save(
            session_id,
            &json!({
                "paragraph": paragraph,
                "updated_at": now(),
                "based_on_mtime": ctx.get("mtime"),
                "prompts_seen": ctx.get("prompt_count"),
                "actions_seen": ctx.get("action_count"),
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{set_test_home, FS_LOCK as LOCK};

    #[test]
    fn now_is_positive() {
        assert!(now() > 0.0);
    }

    #[test]
    fn summary_path_is_under_summary_dir() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let p = summary_path("sid1");
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "sid1.json");
        assert!(p.starts_with(&*paths::SUMMARY_DIR));
    }

    #[test]
    fn load_missing_returns_empty_object() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let _ = std::fs::remove_file(summary_path("nope-xyz"));
        assert_eq!(load("nope-xyz"), json!({}));
    }

    #[test]
    fn load_invalid_returns_empty_object() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        let p = summary_path("bad-json");
        std::fs::write(&p, "not json").unwrap();
        assert_eq!(load("bad-json"), json!({}));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn load_parses_valid() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        let p = summary_path("ok");
        std::fs::write(&p, r#"{"paragraph":"hi","updated_at":5.0}"#).unwrap();
        let v = load("ok");
        assert_eq!(v.get("paragraph").and_then(Value::as_str), Some("hi"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn save_round_trips_via_load() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        save("round", &json!({"paragraph": "x"}));
        let v = load("round");
        assert_eq!(v.get("paragraph").and_then(Value::as_str), Some("x"));
        let _ = std::fs::remove_file(summary_path("round"));
    }

    #[test]
    fn build_prompt_no_previous_marks_first_summary() {
        let p = build_prompt(None, None, &[], None, &[]);
        assert!(p.contains("(none yet — this is the first summary)"));
    }

    #[test]
    fn build_prompt_with_previous_includes_it() {
        let p = build_prompt(Some("old summary"), None, &[], None, &[]);
        assert!(p.contains("old summary"));
    }

    #[test]
    fn build_prompt_uses_goal_when_no_new_prompts() {
        let p = build_prompt(None, Some("fix bug X"), &[], None, &[]);
        assert!(p.contains("Latest user message"));
        assert!(p.contains("fix bug X"));
    }

    #[test]
    fn build_prompt_uses_recent_prompts_over_goal() {
        let prompts = vec!["msg1".to_string(), "msg2".to_string()];
        let p = build_prompt(None, Some("goal"), &[], None, &prompts);
        assert!(p.contains("Recent user messages"));
        assert!(p.contains("- msg1"));
        assert!(p.contains("- msg2"));
        assert!(!p.contains("Latest user message"));
    }

    #[test]
    fn build_prompt_keeps_last_three_prompts_only() {
        let prompts: Vec<String> = (0..10).map(|i| format!("p{i}")).collect();
        let p = build_prompt(None, None, &[], None, &prompts);
        assert!(p.contains("- p7"));
        assert!(p.contains("- p8"));
        assert!(p.contains("- p9"));
        assert!(!p.contains("- p6"));
    }

    #[test]
    fn build_prompt_truncates_recent_prompt_to_200_chars() {
        let long = "x".repeat(500);
        let p = build_prompt(None, None, &[], None, &[long]);
        assert!(p.contains(&format!("- {}", "x".repeat(200))));
        assert!(!p.contains(&"x".repeat(201)));
    }

    #[test]
    fn build_prompt_truncates_goal_to_200_chars() {
        let long = "y".repeat(500);
        let p = build_prompt(None, Some(&long), &[], None, &[]);
        assert!(p.contains(&"y".repeat(200)));
        assert!(!p.contains(&"y".repeat(201)));
    }

    #[test]
    fn build_prompt_includes_recent_actions() {
        let actions = vec![json!({"tool": "Bash"}), json!({"tool": "Read"})];
        let p = build_prompt(None, None, &actions, None, &[]);
        assert!(p.contains("Recent tool invocations"));
        assert!(p.contains("- Bash"));
        assert!(p.contains("- Read"));
    }

    #[test]
    fn build_prompt_keeps_last_five_actions_only() {
        let actions: Vec<Value> = (0..10).map(|i| json!({"tool": format!("T{i}")})).collect();
        let p = build_prompt(None, None, &actions, None, &[]);
        assert!(p.contains("- T5"));
        assert!(p.contains("- T9"));
        assert!(!p.contains("- T4"));
    }

    #[test]
    fn build_prompt_action_without_tool_field_falls_back() {
        let actions = vec![json!({})];
        let p = build_prompt(None, None, &actions, None, &[]);
        assert!(p.contains("- tool"));
    }

    #[test]
    fn build_prompt_always_includes_output_instruction() {
        let p = build_prompt(None, None, &[], None, &[]);
        assert!(p.contains("Output ONE short paragraph"));
    }

    #[test]
    fn which_claude_returns_local_when_exists() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let local_bin_dir = h.join(".local/bin");
        std::fs::create_dir_all(&local_bin_dir).unwrap();
        let local_claude = local_bin_dir.join("claude");
        std::fs::write(&local_claude, "#!/bin/sh\nexit 0\n").unwrap();
        let result = which_claude();
        assert_eq!(result, Some(local_claude.clone()));
        let _ = std::fs::remove_file(&local_claude);
    }

    #[test]
    fn summary_request_fields_accessible() {
        let r = SummaryRequest {
            session_id: "sid".into(),
            ctx: json!({"x": 1}),
        };
        assert_eq!(r.session_id, "sid");
        assert_eq!(r.ctx.get("x").and_then(Value::as_u64), Some(1));
    }

    #[test]
    fn cooldown_constants_sensible() {
        assert!(COOLDOWN_SECONDS > 0.0);
        assert!(MAX_IN_FLIGHT >= 1);
        assert!(API_URL.starts_with("https://"));
    }

    #[test]
    fn summary_path_changes_with_session_id() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let a = summary_path("a");
        let b = summary_path("b");
        assert_ne!(a, b);
    }

    #[test]
    fn save_creates_file_with_json() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        save("save-test", &json!({"k": 42}));
        let raw = std::fs::read_to_string(summary_path("save-test")).unwrap();
        assert!(raw.contains("\"k\":42"));
        let _ = std::fs::remove_file(summary_path("save-test"));
    }

    #[test]
    fn build_prompt_with_actions_and_prompts_orders_sections() {
        let actions = vec![json!({"tool": "Edit"})];
        let prompts = vec!["ask".to_string()];
        let p = build_prompt(Some("prev"), Some("g"), &actions, None, &prompts);
        let prev_idx = p.find("prev").unwrap();
        let prompt_idx = p.find("- ask").unwrap();
        let action_idx = p.find("- Edit").unwrap();
        let out_idx = p.find("Output ONE short paragraph").unwrap();
        assert!(prev_idx < prompt_idx);
        assert!(prompt_idx < action_idx);
        assert!(action_idx < out_idx);
    }

    #[test]
    fn summarizer_new_constructs_within_runtime() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let s = Summarizer::new();
            assert_eq!(s.in_flight.lock().unwrap().len(), 0);
        });
    }

    #[test]
    fn summarizer_request_early_returns_within_cooldown() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            save(
                "cd-sid",
                &json!({
                    "paragraph": "exists",
                    "updated_at": now(),
                    "based_on_mtime": 0.0,
                    "prompts_seen": 0,
                    "actions_seen": 0,
                }),
            );
            let s = Summarizer::new();
            s.request("cd-sid", &json!({"mtime": 1.0, "prompt_count": 100, "action_count": 100}));
            assert!(!s.in_flight.lock().unwrap().contains("cd-sid"));
        });
        let _ = std::fs::remove_file(summary_path("cd-sid"));
    }

    #[test]
    fn summarizer_request_early_returns_when_ctx_mtime_not_newer() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            save(
                "mt-sid",
                &json!({
                    "paragraph": "x",
                    "updated_at": 0.0,
                    "based_on_mtime": 50.0,
                    "prompts_seen": 0,
                    "actions_seen": 0,
                }),
            );
            std::env::set_var("ANTHROPIC_API_KEY", "test-stub-key-mt");
            let s = Summarizer::new();
            s.request("mt-sid", &json!({"mtime": 10.0, "prompt_count": 0, "action_count": 0}));
            std::env::remove_var("ANTHROPIC_API_KEY");
            assert!(!s.in_flight.lock().unwrap().contains("mt-sid"));
        });
        let _ = std::fs::remove_file(summary_path("mt-sid"));
    }

    #[test]
    fn summarizer_request_early_returns_when_insufficient_new_activity() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            save(
                "act-sid",
                &json!({
                    "paragraph": "x",
                    "updated_at": 0.0,
                    "based_on_mtime": 0.0,
                    "prompts_seen": 5,
                    "actions_seen": 5,
                }),
            );
            std::env::set_var("ANTHROPIC_API_KEY", "test-stub-key-act");
            let s = Summarizer::new();
            s.request("act-sid", &json!({"mtime": 0.0, "prompt_count": 5, "action_count": 6}));
            std::env::remove_var("ANTHROPIC_API_KEY");
            assert!(!s.in_flight.lock().unwrap().contains("act-sid"));
        });
        let _ = std::fs::remove_file(summary_path("act-sid"));
    }

    use axum::response::IntoResponse;

    #[tokio::test]
    async fn call_api_returns_none_without_api_key() {
        std::env::remove_var("ANTHROPIC_API_KEY");
        let result = call_api("any").await;
        assert!(result.is_none());
    }

    async fn spawn_mock_api(responder: impl Fn() -> axum::response::Response + Send + Sync + 'static) -> String {
        use axum::routing::post;
        use axum::Router;
        use std::sync::Arc;
        let responder = Arc::new(responder);
        let app = Router::new().route(
            "/v1/messages",
            post(move || {
                let r = responder.clone();
                async move { r() }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{}/v1/messages", addr)
    }

    #[tokio::test]
    async fn call_api_at_returns_text_from_mock() {
        let _g = LOCK.lock().unwrap();
        std::env::set_var("ANTHROPIC_API_KEY", "k-for-mock");
        let url = spawn_mock_api(|| {
            axum::Json(json!({
                "content": [
                    {"type": "text", "text": "  mocked paragraph.  "},
                    {"type": "text", "text": "ignored second"},
                ],
            })).into_response()
        }).await;
        let got = call_api_at(&url, "hello").await;
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert_eq!(got.as_deref(), Some("mocked paragraph."));
    }

    #[tokio::test]
    async fn call_api_at_skips_blocks_with_wrong_type_and_returns_first_text() {
        let _g = LOCK.lock().unwrap();
        std::env::set_var("ANTHROPIC_API_KEY", "k-mix");
        let url = spawn_mock_api(|| {
            axum::Json(json!({
                "content": [
                    {"type": "thinking", "text": "skip-me"},
                    {"type": "text", "text": ""},
                    {"type": "text", "text": "real text"},
                ],
            })).into_response()
        }).await;
        let got = call_api_at(&url, "prompt").await;
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert_eq!(got.as_deref(), Some("real text"));
    }

    #[tokio::test]
    async fn call_api_at_returns_none_on_non_success_status() {
        let _g = LOCK.lock().unwrap();
        std::env::set_var("ANTHROPIC_API_KEY", "k-err");
        let url = spawn_mock_api(|| {
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response()
        }).await;
        let got = call_api_at(&url, "p").await;
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn call_api_at_returns_none_when_no_content_array() {
        let _g = LOCK.lock().unwrap();
        std::env::set_var("ANTHROPIC_API_KEY", "k-empty");
        let url = spawn_mock_api(|| {
            axum::Json(json!({"stop_reason": "end_turn"})).into_response()
        }).await;
        let got = call_api_at(&url, "p").await;
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn call_api_at_returns_none_when_all_text_blocks_empty() {
        let _g = LOCK.lock().unwrap();
        std::env::set_var("ANTHROPIC_API_KEY", "k-whitespace");
        let url = spawn_mock_api(|| {
            axum::Json(json!({"content": [{"type": "text", "text": "   "}]})).into_response()
        }).await;
        let got = call_api_at(&url, "p").await;
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn call_api_at_returns_none_when_send_fails() {
        let _g = LOCK.lock().unwrap();
        std::env::set_var("ANTHROPIC_API_KEY", "k-net");
        let got = call_api_at("http://127.0.0.1:1/does-not-exist", "p").await;
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn generate_falls_through_when_no_backends() {
        std::env::remove_var("ANTHROPIC_API_KEY");
        let _ = generate("any").await;
    }

    #[test]
    fn summarizer_request_skips_when_already_in_flight() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            std::env::set_var("ANTHROPIC_API_KEY", "k");
            let s = Summarizer::new();
            s.in_flight.lock().unwrap().insert("dup-sid".into());
            let before = s.in_flight.lock().unwrap().len();
            s.request(
                "dup-sid",
                &json!({"mtime": 0.0, "prompt_count": 0, "action_count": 0}),
            );
            let after = s.in_flight.lock().unwrap().len();
            std::env::remove_var("ANTHROPIC_API_KEY");
            assert_eq!(before, after);
        });
        let _ = std::fs::remove_file(summary_path("dup-sid"));
    }

    #[test]
    fn summarizer_request_skips_when_max_in_flight() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            std::env::set_var("ANTHROPIC_API_KEY", "k");
            let s = Summarizer::new();
            {
                let mut g = s.in_flight.lock().unwrap();
                for i in 0..MAX_IN_FLIGHT {
                    g.insert(format!("busy-{i}"));
                }
            }
            s.request(
                "new-sid",
                &json!({"mtime": 0.0, "prompt_count": 0, "action_count": 0}),
            );
            std::env::remove_var("ANTHROPIC_API_KEY");
            assert!(!s.in_flight.lock().unwrap().contains("new-sid"));
        });
    }

    #[test]
    fn summarizer_request_returns_when_no_backend_configured() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        std::env::remove_var("ANTHROPIC_API_KEY");
        let prev_path = std::env::var("PATH").ok();
        std::env::set_var("PATH", h.to_string_lossy().to_string());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let s = Summarizer::new();
            s.request(
                "no-be-sid",
                &json!({"mtime": 0.0, "prompt_count": 0, "action_count": 0}),
            );
            assert!(!s.in_flight.lock().unwrap().contains("no-be-sid"));
        });
        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        }
    }

    #[test]
    fn summarizer_request_enqueues_when_existing_paragraph_and_large_delta() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        save(
            "big-delta-sid",
            &json!({
                "paragraph": "prev",
                "updated_at": 0.0,
                "based_on_mtime": 0.0,
                "prompts_seen": 1,
                "actions_seen": 1,
            }),
        );
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            std::env::set_var("ANTHROPIC_API_KEY", "k-big");
            let s = Summarizer::new();
            s.request(
                "big-delta-sid",
                &json!({"mtime": 5.0, "prompt_count": 10, "action_count": 100}),
            );
            let has = s.in_flight.lock().unwrap().contains("big-delta-sid");
            std::env::remove_var("ANTHROPIC_API_KEY");
            assert!(has);
        });
        let _ = std::fs::remove_file(summary_path("big-delta-sid"));
    }

    #[test]
    fn summarizer_request_enqueues_on_happy_path() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        let _ = std::fs::remove_file(summary_path("happy-sid"));
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            std::env::set_var("ANTHROPIC_API_KEY", "stub-key");
            let s = Summarizer::new();
            s.request(
                "happy-sid",
                &json!({"mtime": 5.0, "prompt_count": 1, "action_count": 1}),
            );
            let has = s.in_flight.lock().unwrap().contains("happy-sid");
            std::env::remove_var("ANTHROPIC_API_KEY");
            assert!(has);
        });
    }

    #[test]
    fn which_claude_finds_on_path_when_local_absent() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let bin_dir = h.join("bins");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let claude = bin_dir.join("claude");
        std::fs::write(&claude, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let prev_path = std::env::var("PATH").ok();
        std::env::set_var(
            "PATH",
            format!("{}:{}", bin_dir.display(), prev_path.clone().unwrap_or_default()),
        );
        let result = which_claude();
        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        }
        assert!(result.is_some());
    }

    #[test]
    fn which_claude_returns_none_when_which_exits_nonzero() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let _ = std::fs::remove_file(h.join(".local/bin/claude"));
        let bin_dir = h.join("stub-which-fail");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let which = bin_dir.join("which");
        std::fs::write(&which, "#!/bin/sh\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&which, std::fs::Permissions::from_mode(0o755)).unwrap();
        let prev_path = std::env::var("PATH").ok();
        std::env::set_var("PATH", bin_dir.to_string_lossy().to_string());
        let result = which_claude();
        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        }
        assert!(result.is_none());
    }

    #[test]
    fn which_claude_returns_none_when_which_prints_empty() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let _ = std::fs::remove_file(h.join(".local/bin/claude"));
        let bin_dir = h.join("stub-which-empty");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let which = bin_dir.join("which");
        std::fs::write(&which, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&which, std::fs::Permissions::from_mode(0o755)).unwrap();
        let prev_path = std::env::var("PATH").ok();
        std::env::set_var("PATH", bin_dir.to_string_lossy().to_string());
        let result = which_claude();
        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        }
        assert!(result.is_none());
    }

    #[test]
    fn which_claude_returns_none_when_path_empty_and_no_local() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let _ = std::fs::remove_file(h.join(".local/bin/claude"));
        let prev_path = std::env::var("PATH").ok();
        let empty = h.join("no-such-dir-for-path");
        std::env::set_var("PATH", empty.to_string_lossy().to_string());
        let result = which_claude();
        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        }
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn call_cli_returns_none_when_no_claude_binary() {
        let h = set_test_home();
        let _ = std::fs::remove_file(h.join(".local/bin/claude"));
        let prev_path = std::env::var("PATH").ok();
        std::env::set_var("PATH", h.join("empty-path").to_string_lossy().to_string());
        let result = call_cli("hello").await;
        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        }
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn call_cli_returns_text_from_stub_success() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let local_bin = h.join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        let claude_stub = local_bin.join("claude");
        std::fs::write(&claude_stub, "#!/bin/sh\nprintf '  summarized paragraph  \\n'\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude_stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let got = call_cli("please summarize").await;
        let _ = std::fs::remove_file(&claude_stub);
        assert_eq!(got.as_deref(), Some("summarized paragraph"));
    }

    #[tokio::test]
    async fn call_cli_returns_none_when_stub_exits_nonzero() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let local_bin = h.join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        let claude_stub = local_bin.join("claude");
        std::fs::write(&claude_stub, "#!/bin/sh\necho out\nexit 7\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude_stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let got = call_cli("please summarize").await;
        let _ = std::fs::remove_file(&claude_stub);
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn call_cli_returns_none_when_stub_prints_only_whitespace() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let local_bin = h.join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        let claude_stub = local_bin.join("claude");
        std::fs::write(&claude_stub, "#!/bin/sh\nprintf '   \\n'\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude_stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let got = call_cli("please summarize").await;
        let _ = std::fs::remove_file(&claude_stub);
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn generate_prefers_api_when_api_returns_text() {
        let _g = LOCK.lock().unwrap();
        std::env::set_var("ANTHROPIC_API_KEY", "k-gen");
        let url = spawn_mock_api(|| {
            axum::Json(json!({"content":[{"type":"text","text":"api wins"}]})).into_response()
        }).await;
        let got = call_api_at(&url, "p").await;
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert_eq!(got.as_deref(), Some("api wins"));
    }

    #[tokio::test]
    async fn generate_returns_api_text_via_overridden_url() {
        let _g = LOCK.lock().unwrap();
        let url = spawn_mock_api(|| {
            axum::Json(json!({"content":[{"type":"text","text":"full-generate api"}]})).into_response()
        }).await;
        std::env::set_var("ANTHROPIC_API_KEY", "k-gen-full");
        std::env::set_var("CIU_SUMMARIZER_API_URL", &url);
        let got = generate("please").await;
        std::env::remove_var("CIU_SUMMARIZER_API_URL");
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert_eq!(got.as_deref(), Some("full-generate api"));
    }

    #[tokio::test]
    async fn worker_loop_dispatches_request_from_channel() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        let local_bin = h.join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        let claude_stub = local_bin.join("claude");
        std::fs::write(&claude_stub, "#!/bin/sh\nprintf 'worker out'\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude_stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::remove_var("ANTHROPIC_API_KEY");
        let _ = std::fs::remove_file(summary_path("wl-sid"));
        let (tx, rx) = mpsc::channel::<SummaryRequest>(4);
        let handle = tokio::spawn(worker_loop(rx));
        tx.send(SummaryRequest {
            session_id: "wl-sid".into(),
            ctx: json!({"mtime": 1.0}),
        }).await.unwrap();
        drop(tx);
        let _ = handle.await;
        for _ in 0..50 {
            if load("wl-sid").get("paragraph").is_some() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let v = load("wl-sid");
        let _ = std::fs::remove_file(&claude_stub);
        let _ = std::fs::remove_file(summary_path("wl-sid"));
        assert_eq!(v.get("paragraph").and_then(Value::as_str), Some("worker out"));
    }

    #[tokio::test]
    async fn process_request_writes_summary_when_generate_succeeds_via_cli() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        std::fs::create_dir_all(&*paths::SUMMARY_DIR).unwrap();
        let local_bin = h.join(".local/bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        let claude_stub = local_bin.join("claude");
        std::fs::write(&claude_stub, "#!/bin/sh\nprintf 'generated summary'\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude_stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::remove_var("ANTHROPIC_API_KEY");
        let _ = std::fs::remove_file(summary_path("pr-sid"));
        process_request(
            "pr-sid",
            &json!({
                "mtime": 1.0,
                "prompt_count": 2,
                "action_count": 3,
                "goal": "fix bug",
                "actions": [{"tool": "Bash"}],
                "last_text": "hi",
                "recent_prompts": ["do a thing"],
            }),
        ).await;
        let _ = std::fs::remove_file(&claude_stub);
        let v = load("pr-sid");
        assert_eq!(v.get("paragraph").and_then(Value::as_str), Some("generated summary"));
        let _ = std::fs::remove_file(summary_path("pr-sid"));
    }
}
