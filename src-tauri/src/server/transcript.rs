use crate::paths::{PROJECTS_DIR, SUMMARY_DIR};
use crate::server::instances::AppState;
use crate::server::types::{TranscriptEntry, TranscriptResponse};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::DateTime;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

pub fn transcript_path_for(session_id: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let cwd = cwd?;
    let slug = format!("-{}", cwd.replace('/', "-").trim_start_matches('-'));
    let p = PROJECTS_DIR.join(&slug).join(format!("{session_id}.jsonl"));
    if p.exists() { Some(p) } else { None }
}

pub fn title_cached(path: &std::path::Path) -> (Option<String>, Option<String>) {
    let mut title: Option<String> = None;
    let mut first_user: Option<String> = None;
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return (None, None),
    };
    for (idx, line) in content.lines().enumerate() {
        if idx > 1000 && title.is_some() && first_user.is_some() {
            break;
        }
        let d: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let t = d.get("type").and_then(Value::as_str).unwrap_or_default();
        if t == "custom-title" {
            if let Some(ct) = d.get("customTitle").and_then(Value::as_str) {
                if !ct.is_empty() {
                    title = Some(ct.to_string());
                }
            }
        } else if t == "agent-name" && title.is_none() {
            if let Some(an) = d.get("agentName").and_then(Value::as_str) {
                if !an.is_empty() {
                    title = Some(an.to_string());
                }
            }
        } else if t == "user" && first_user.is_none() {
            let msg = d.get("message").unwrap_or(&Value::Null);
            if let Some(obj) = msg.as_object() {
                let content = obj.get("content");
                if let Some(s) = content.and_then(Value::as_str) {
                    first_user = Some(s.to_string());
                } else if let Some(arr) = content.and_then(Value::as_array) {
                    for item in arr {
                        if item.get("type").and_then(Value::as_str) == Some("text") {
                            if let Some(txt) = item.get("text").and_then(Value::as_str) {
                                first_user = Some(txt.to_string());
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(ref mut fu) = first_user {
        *fu = fu.trim().replace('\n', " ");
        if fu.len() > 80 {
            let truncated: String = fu.chars().take(77).collect();
            *fu = format!("{truncated}\u{2026}");
        }
    }
    (title, first_user)
}

pub fn session_title(session_id: &str, cwd: Option<&str>) -> (Option<String>, Option<String>) {
    match transcript_path_for(session_id, cwd) {
        Some(path) => title_cached(&path),
        None => (None, None),
    }
}

pub fn iso_to_epoch(iso: Option<&str>) -> Option<f64> {
    let iso = iso?;
    let dt = DateTime::parse_from_rfc3339(&iso.replace('Z', "+00:00")).ok()?;
    Some(dt.timestamp_millis() as f64 / 1000.0)
}

fn read_tail(path: &std::path::Path, max_bytes: u64) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let size = f.seek(SeekFrom::End(0)).ok()?;
    let block = size.min(max_bytes);
    f.seek(SeekFrom::Start(size - block)).ok()?;
    let mut buf = vec![0u8; block as usize];
    f.read_exact(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub fn jsonl_tail(session_id: &str, cwd: Option<&str>) -> Value {
    let path = match transcript_path_for(session_id, cwd) {
        Some(p) => p,
        None => return json!({}),
    };
    let data = match read_tail(&path, 262144) {
        Some(d) => d,
        None => return json!({}),
    };

    let mut pending: HashMap<String, Option<String>> = HashMap::new();
    let mut pending_started_epoch: Option<f64> = None;
    let mut last_ts_iso: Option<String> = None;
    let mut last_ts_epoch: Option<f64> = None;
    let mut last_type: Option<String> = None;
    let mut last_assistant_preview: Option<String> = None;

    for line in data.trim().lines() {
        let d: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if d.get("isSidechain").and_then(Value::as_bool).unwrap_or(false)
            || d.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
        {
            continue;
        }
        let t = match d.get("type").and_then(Value::as_str) {
            Some(t) if t == "assistant" || t == "user" => t,
            _ => continue,
        };
        let ts = d.get("timestamp").and_then(Value::as_str);
        let ts_epoch = ts.and_then(|s| iso_to_epoch(Some(s)));
        if ts.is_some() {
            last_ts_iso = ts.map(String::from);
            last_ts_epoch = ts_epoch;
            last_type = Some(t.to_string());
        }
        let msg = d.get("message").unwrap_or(&Value::Null);
        let content = msg.as_object().and_then(|o| o.get("content"));

        if t == "user" {
            let mut has_tool_result = false;
            let mut has_text = false;
            if let Some(s) = content.and_then(Value::as_str) {
                if !s.trim().is_empty() {
                    has_text = true;
                }
            } else if let Some(arr) = content.and_then(Value::as_array) {
                for item in arr {
                    let obj = match item.as_object() {
                        Some(o) => o,
                        None => continue,
                    };
                    let ik = obj.get("type").and_then(Value::as_str).unwrap_or_default();
                    if ik == "tool_result" {
                        has_tool_result = true;
                        if let Some(tid) = obj.get("tool_use_id").and_then(Value::as_str) {
                            pending.remove(tid);
                        }
                    } else if ik == "text" {
                        let txt = obj.get("text").and_then(Value::as_str).unwrap_or_default();
                        if !txt.trim().is_empty() {
                            has_text = true;
                        }
                    }
                }
            }
            if has_text && !has_tool_result {
                pending.clear();
                pending_started_epoch = None;
            } else if pending.is_empty() {
                pending_started_epoch = None;
            }
        } else if t == "assistant" {
            if let Some(arr) = content.and_then(Value::as_array) {
                for item in arr {
                    let obj = match item.as_object() {
                        Some(o) => o,
                        None => continue,
                    };
                    let ik = obj.get("type").and_then(Value::as_str).unwrap_or_default();
                    if ik == "tool_use" {
                        if let Some(tid) = obj.get("id").and_then(Value::as_str) {
                            if pending.is_empty() {
                                pending_started_epoch = ts_epoch;
                            }
                            pending.insert(
                                tid.to_string(),
                                obj.get("name").and_then(Value::as_str).map(String::from),
                            );
                        }
                    } else if ik == "text" {
                        let txt = obj
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .trim()
                            .replace('\n', " ");
                        if !txt.is_empty() {
                            let preview = if txt.len() > 140 {
                                let truncated: String = txt.chars().take(140).collect();
                                format!("{truncated}\u{2026}")
                            } else {
                                txt
                            };
                            last_assistant_preview = Some(preview);
                        }
                    }
                }
            }
        }
    }

    let pending_tool = pending.values().last().and_then(|v| v.clone());

    json!({
        "last_type": last_type,
        "last_timestamp": last_ts_iso,
        "last_timestamp_epoch": last_ts_epoch,
        "last_assistant_preview": last_assistant_preview,
        "pending": !pending.is_empty(),
        "pending_tool": pending_tool,
        "pending_started_epoch": pending_started_epoch,
    })
}

pub fn summarize_tool_arg(tool: Option<&str>, input: &Value) -> String {
    let obj = match input.as_object() {
        Some(o) => o,
        None => return String::new(),
    };
    let tool = match tool {
        Some(t) => t,
        None => return String::new(),
    };
    match tool {
        "Bash" => {
            let cmd = obj
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .replace('\n', " ");
            if cmd.len() > 120 {
                let truncated: String = cmd.chars().take(120).collect();
                format!("{truncated}\u{2026}")
            } else {
                cmd
            }
        }
        "Read" | "Edit" | "Write" | "NotebookEdit" => {
            let fp = obj
                .get("file_path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            std::path::Path::new(fp)
                .file_name()
                .and_then(|n| n.to_str())
                .map(String::from)
                .unwrap_or_else(|| fp.to_string())
        }
        "Grep" => {
            let pat = obj.get("pattern").and_then(Value::as_str).unwrap_or_default();
            let wh = obj
                .get("path")
                .or_else(|| obj.get("glob"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if wh.is_empty() {
                format!("\"{pat}\"")
            } else {
                format!("\"{pat}\" in {wh}")
            }
        }
        "Glob" => obj
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        "WebFetch" => obj
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        "WebSearch" => {
            let q = obj.get("query").and_then(Value::as_str).unwrap_or_default();
            format!("\"{q}\"")
        }
        "Task" | "Agent" => {
            let desc = obj
                .get("description")
                .or_else(|| obj.get("subagent_type"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            desc.chars().take(80).collect()
        }
        "TodoWrite" => {
            let count = obj
                .get("todos")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            format!("{count} todos")
        }
        _ => String::new(),
    }
}

fn summary_from_data(data: &str) -> Value {
    let mut goal: Option<String> = None;
    let mut actions: Vec<Value> = Vec::new();
    let mut all_actions_count: usize = 0;
    let mut last_text: Option<String> = None;
    let mut last_assistant_ts: Option<String> = None;
    let mut user_prompts: Vec<String> = Vec::new();

    for line in data.trim().lines() {
        let d: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if d.get("isSidechain").and_then(Value::as_bool).unwrap_or(false)
            || d.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
        {
            continue;
        }
        let t = match d.get("type").and_then(Value::as_str) {
            Some(t) if t == "user" || t == "assistant" => t,
            _ => continue,
        };
        let msg = d.get("message").unwrap_or(&Value::Null);
        let content = msg.as_object().and_then(|o| o.get("content"));
        let ts = d.get("timestamp").and_then(Value::as_str).map(String::from);

        if t == "user" {
            let mut text_parts: Vec<String> = Vec::new();
            let mut has_tool_result = false;
            if let Some(s) = content.and_then(Value::as_str) {
                if !s.trim().is_empty() {
                    text_parts.push(s.to_string());
                }
            } else if let Some(arr) = content.and_then(Value::as_array) {
                for item in arr {
                    let obj = match item.as_object() {
                        Some(o) => o,
                        None => continue,
                    };
                    let ik = obj.get("type").and_then(Value::as_str).unwrap_or_default();
                    if ik == "tool_result" {
                        has_tool_result = true;
                    } else if ik == "text" {
                        let txt = obj.get("text").and_then(Value::as_str).unwrap_or_default();
                        if !txt.trim().is_empty() {
                            text_parts.push(txt.to_string());
                        }
                    }
                }
            }
            if !text_parts.is_empty() && !has_tool_result {
                let joined = text_parts.join(" ").trim().replace('\n', " ");
                let head: String = joined.trim_start().chars().take(40).collect::<String>().to_lowercase();
                let is_meta = head.starts_with("<command-")
                    || head.starts_with("<local-command")
                    || head.starts_with("[request interrupted")
                    || head.starts_with("caveat:");
                if !is_meta {
                    let g: String = joined.chars().take(180).collect();
                    goal = Some(if joined.len() > 180 {
                        format!("{g}\u{2026}")
                    } else {
                        g
                    });
                    let prompt: String = joined.chars().take(500).collect();
                    user_prompts.push(prompt);
                    actions.clear();
                    last_text = None;
                }
            }
        } else if t == "assistant" {
            if let Some(arr) = content.and_then(Value::as_array) {
                for item in arr {
                    let obj = match item.as_object() {
                        Some(o) => o,
                        None => continue,
                    };
                    let ik = obj.get("type").and_then(Value::as_str).unwrap_or_default();
                    if ik == "tool_use" {
                        all_actions_count += 1;
                        let tool = obj.get("name").and_then(Value::as_str);
                        let input_val = obj.get("input").unwrap_or(&Value::Null);
                        let arg = summarize_tool_arg(tool, input_val);
                        let tool_str = tool.unwrap_or_default();
                        let dup = actions.last().map_or(false, |a| {
                            a.get("tool").and_then(Value::as_str).unwrap_or_default() == tool_str
                                && a.get("arg").and_then(Value::as_str).unwrap_or_default() == arg
                        });
                        if !dup {
                            actions.push(json!({"tool": tool_str, "arg": arg, "ts": ts}));
                            if actions.len() > 14 {
                                actions.drain(..actions.len() - 14);
                            }
                        }
                    } else if ik == "text" {
                        let txt = obj
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .trim();
                        if !txt.is_empty() {
                            let lt: String = txt.chars().take(400).collect();
                            last_text = Some(if txt.len() > 400 {
                                format!("{lt}\u{2026}")
                            } else {
                                lt
                            });
                            last_assistant_ts = ts.clone();
                        }
                    }
                }
            }
        }
    }

    let tail_actions: Vec<Value> = if actions.len() > 8 {
        actions[actions.len() - 8..].to_vec()
    } else {
        actions
    };
    let recent: Vec<String> = if user_prompts.len() > 5 {
        user_prompts[user_prompts.len() - 5..].to_vec()
    } else {
        user_prompts.clone()
    };

    json!({
        "goal": goal,
        "actions": tail_actions,
        "last_text": last_text,
        "last_assistant_timestamp": last_assistant_ts,
        "prompt_count": user_prompts.len(),
        "action_count": all_actions_count,
        "recent_prompts": recent,
    })
}

pub fn jsonl_summary(session_id: &str, cwd: Option<&str>) -> Value {
    let empty = json!({
        "goal": null,
        "actions": [],
        "last_text": null,
        "last_assistant_timestamp": null,
        "paragraph": null,
        "paragraph_updated_at": null,
    });
    let path = match transcript_path_for(session_id, cwd) {
        Some(p) => p,
        None => return empty,
    };
    let data = match read_tail(&path, 524288) {
        Some(d) => d,
        None => return empty,
    };
    let mut base = summary_from_data(&data);

    let summary_path = SUMMARY_DIR.join(format!("{session_id}.json"));
    if let Ok(contents) = std::fs::read_to_string(&summary_path) {
        if let Ok(cached) = serde_json::from_str::<Value>(&contents) {
            base["paragraph"] = cached.get("paragraph").cloned().unwrap_or(Value::Null);
            base["paragraph_updated_at"] = cached.get("updated_at").cloned().unwrap_or(Value::Null);
        }
    }
    if base.get("paragraph").is_none() || base["paragraph"].is_null() {
        base["paragraph"] = Value::Null;
        base["paragraph_updated_at"] = Value::Null;
    }

    base
}

#[derive(Debug, Deserialize)]
pub struct TranscriptQuery {
    #[serde(default = "default_limit")]
    pub limit: Option<usize>,
}

fn default_limit() -> Option<usize> {
    Some(60)
}

pub async fn api_transcript(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Query(query): Query<TranscriptQuery>,
) -> Result<Json<TranscriptResponse>, StatusCode> {
    let limit = query.limit.unwrap_or(60);
    let instances = state.cached_instances();
    let inst = instances
        .iter()
        .find(|i| i.session_id == session_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let cwd = inst.cwd.as_deref();
    let path = transcript_path_for(&session_id, cwd);

    let mut entries: Vec<TranscriptEntry> = Vec::new();
    if let Some(path) = path {
        if let Some(data) = read_tail(&path, 524288) {
            let lines: Vec<&str> = data.lines().collect();
            let skip = if lines.len() > limit * 4 {
                lines.len() - limit * 4
            } else {
                0
            };
            for line in &lines[skip..] {
                let d: Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let t = match d.get("type").and_then(Value::as_str) {
                    Some(t) if t == "user" || t == "assistant" => t,
                    _ => continue,
                };
                if d.get("isSidechain").and_then(Value::as_bool).unwrap_or(false) {
                    continue;
                }
                if d.get("isMeta").and_then(Value::as_bool).unwrap_or(false) {
                    continue;
                }
                let uuid = d.get("uuid").and_then(Value::as_str).map(String::from);
                let msg = d.get("message").unwrap_or(&Value::Null);
                let content = msg.as_object().and_then(|o| o.get("content"));
                let mut parts: Vec<Value> = Vec::new();

                if let Some(s) = content.and_then(Value::as_str) {
                    parts.push(json!({"kind": "text", "text": s}));
                } else if let Some(arr) = content.and_then(Value::as_array) {
                    for item in arr {
                        let obj = match item.as_object() {
                            Some(o) => o,
                            None => continue,
                        };
                        let ik = obj.get("type").and_then(Value::as_str).unwrap_or_default();
                        match ik {
                            "text" => {
                                let txt = obj
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                parts.push(json!({"kind": "text", "text": txt}));
                            }
                            "tool_use" => {
                                let inp = obj.get("input").cloned().unwrap_or(json!({}));
                                parts.push(json!({
                                    "kind": "tool_use",
                                    "tool": obj.get("name").and_then(Value::as_str).unwrap_or_default(),
                                    "input": inp,
                                }));
                            }
                            "tool_result" => {
                                let res = obj.get("content");
                                let txt = if let Some(s) = res.and_then(Value::as_str) {
                                    s.to_string()
                                } else if let Some(arr) = res.and_then(Value::as_array) {
                                    arr.iter()
                                        .filter_map(|r| {
                                            let ro = r.as_object()?;
                                            if ro.get("type").and_then(Value::as_str) == Some("text") {
                                                ro.get("text").and_then(Value::as_str).map(String::from)
                                            } else {
                                                None
                                            }
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                } else {
                                    String::new()
                                };
                                parts.push(json!({
                                    "kind": "tool_result",
                                    "text": txt,
                                    "is_error": obj.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                                }));
                            }
                            "thinking" => {
                                let txt = obj
                                    .get("thinking")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                parts.push(json!({"kind": "thinking", "text": txt}));
                            }
                            _ => {}
                        }
                    }
                }

                if parts.is_empty() {
                    continue;
                }
                entries.push(TranscriptEntry {
                    uuid,
                    entry_type: t.to_string(),
                    timestamp: d.get("timestamp").and_then(Value::as_str).map(String::from),
                    parts,
                });
            }
            if entries.len() > limit {
                entries = entries.split_off(entries.len() - limit);
            }
        }
    }

    let inst = inst.clone();
    let session = json!({
        "session_id": inst.session_id,
        "name": inst.name,
        "title": inst.title,
        "custom_name": inst.custom_name,
        "status": inst.status,
        "cwd": inst.cwd,
        "pid": inst.pid,
        "group": inst.group,
        "mcps": inst.mcps,
        "notification_message": inst.notification_message,
        "last_tool": inst.last_tool,
        "hook_timestamp": inst.hook_timestamp,
        "started_at": inst.started_at,
        "alive": inst.alive,
        "summary": inst.summary,
        "subagents": inst.subagents,
        "our_sid": inst.our_sid,
        "tmux_session": inst.tmux_session,
    });

    Ok(Json(TranscriptResponse { session, entries }))
}
