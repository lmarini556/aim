use crate::paths;
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
    last_text: Option<&str>,
    new_prompts: &[String],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(
        "You are maintaining a rolling summary of a Claude Code agent session.".to_string(),
    );
    parts.push(String::new());

    if let Some(prev) = prev {
        parts.push(format!("Previous summary:\n{prev}"));
        parts.push(String::new());
    }
    if !new_prompts.is_empty() {
        parts.push("New user messages since last update:".to_string());
        for p in new_prompts.iter().rev().take(5).rev() {
            parts.push(format!("- {p}"));
        }
        parts.push(String::new());
    }
    if let Some(goal) = goal {
        if new_prompts.is_empty() {
            parts.push(format!("Current task (latest user message): {goal}"));
            parts.push(String::new());
        }
    }
    if !actions.is_empty() {
        parts.push("Recent agent actions:".to_string());
        for a in actions.iter().rev().take(10).rev() {
            let tool = a
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let arg = a
                .get("arg")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if arg.is_empty() {
                parts.push(format!("- {tool}"));
            } else {
                parts.push(format!("- {tool}: {arg}"));
            }
        }
        parts.push(String::new());
    }
    if let Some(lt) = last_text {
        let preview: String = lt.chars().take(400).collect();
        let preview = preview.replace('\n', " ");
        parts.push(format!("Most recent agent reply: {}", preview.trim()));
        parts.push(String::new());
    }
    parts.push(
        "Write ONE short paragraph (2-4 sentences) describing the session's overall purpose, \
         progress, and current state. Prioritize what the user is ultimately trying to accomplish. \
         Evolve naturally from the previous summary — do not reset if the topic is unchanged; \
         do not re-describe everything from scratch. Be concrete and specific (mention files, \
         systems, or bug symptoms when relevant). No filler, no meta-commentary about the summary.\n\n\
         STRICT RULES:\n\
         - NEVER ask questions or request more context.\n\
         - NEVER say you lack information. Summarize whatever is available.\n\
         - If very little context exists, describe what you can infer from tool names and args.\n\
         - Output ONLY the summary paragraph — no preamble, no bullet points, no formatting."
            .to_string(),
    );
    parts.join("\n")
}

async fn call_api(user_prompt: &str) -> Option<String> {
    let key = std::env::var("ANTHROPIC_API_KEY").ok()?;
    let model = std::env::var("CLAUDE_INSTANCES_SUMMARY_MODEL")
        .unwrap_or_else(|_| "claude-haiku-4-5".to_string());

    let client = reqwest::Client::new();
    let resp = client
        .post(API_URL)
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
        .get("new_prompts")
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
