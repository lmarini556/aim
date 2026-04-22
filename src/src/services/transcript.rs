use crate::infra::paths::{PROJECTS_DIR, SUMMARY_DIR};
use crate::services::instances::AppState;
use crate::http::dto::{TranscriptEntry, TranscriptResponse};
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{set_test_home, FS_LOCK as LOCK};

    fn write_project_transcript(cwd: &str, session_id: &str, jsonl: &str) -> std::path::PathBuf {
        let _ = set_test_home();
        let slug = format!("-{}", cwd.replace('/', "-").trim_start_matches('-'));
        let dir = PROJECTS_DIR.join(&slug);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{session_id}.jsonl"));
        std::fs::write(&p, jsonl).unwrap();
        p
    }

    #[test]
    fn iso_to_epoch_none_input() {
        assert!(iso_to_epoch(None).is_none());
    }

    #[test]
    fn iso_to_epoch_invalid_string() {
        assert!(iso_to_epoch(Some("not-a-date")).is_none());
    }

    #[test]
    fn iso_to_epoch_with_z_suffix() {
        let e = iso_to_epoch(Some("2024-01-01T00:00:00Z")).unwrap();
        assert!((e - 1704067200.0).abs() < 1e-6);
    }

    #[test]
    fn iso_to_epoch_with_offset() {
        let e = iso_to_epoch(Some("2024-01-01T00:00:00+00:00")).unwrap();
        assert!((e - 1704067200.0).abs() < 1e-6);
    }

    #[test]
    fn iso_to_epoch_fractional_seconds() {
        let e = iso_to_epoch(Some("2024-01-01T00:00:00.500Z")).unwrap();
        assert!((e - 1704067200.5).abs() < 1e-6);
    }

    #[test]
    fn summarize_tool_arg_none_tool() {
        assert_eq!(summarize_tool_arg(None, &json!({"x": 1})), "");
    }

    #[test]
    fn summarize_tool_arg_non_object_input() {
        assert_eq!(summarize_tool_arg(Some("Bash"), &json!([1, 2, 3])), "");
    }

    #[test]
    fn summarize_tool_arg_bash_short() {
        let s = summarize_tool_arg(Some("Bash"), &json!({"command": "ls -la"}));
        assert_eq!(s, "ls -la");
    }

    #[test]
    fn summarize_tool_arg_bash_newlines_flattened() {
        let s = summarize_tool_arg(Some("Bash"), &json!({"command": "ls\n-la"}));
        assert_eq!(s, "ls -la");
    }

    #[test]
    fn summarize_tool_arg_bash_truncated_with_ellipsis() {
        let long = "x".repeat(200);
        let s = summarize_tool_arg(Some("Bash"), &json!({"command": long}));
        assert!(s.ends_with('\u{2026}'));
        assert_eq!(s.chars().count(), 121);
    }

    #[test]
    fn summarize_tool_arg_read_returns_file_name() {
        let s = summarize_tool_arg(Some("Read"), &json!({"file_path": "/a/b/c.txt"}));
        assert_eq!(s, "c.txt");
    }

    #[test]
    fn summarize_tool_arg_edit_falls_back_to_full_path_when_no_basename() {
        let s = summarize_tool_arg(Some("Edit"), &json!({"file_path": ""}));
        assert_eq!(s, "");
    }

    #[test]
    fn summarize_tool_arg_write_file_name() {
        let s = summarize_tool_arg(Some("Write"), &json!({"file_path": "/x/y.js"}));
        assert_eq!(s, "y.js");
    }

    #[test]
    fn summarize_tool_arg_notebook_edit_file_name() {
        let s = summarize_tool_arg(Some("NotebookEdit"), &json!({"file_path": "/x/nb.ipynb"}));
        assert_eq!(s, "nb.ipynb");
    }

    #[test]
    fn summarize_tool_arg_grep_with_path() {
        let s = summarize_tool_arg(Some("Grep"), &json!({"pattern": "foo", "path": "/tmp"}));
        assert_eq!(s, "\"foo\" in /tmp");
    }

    #[test]
    fn summarize_tool_arg_grep_without_path_uses_glob() {
        let s = summarize_tool_arg(Some("Grep"), &json!({"pattern": "p", "glob": "*.rs"}));
        assert_eq!(s, "\"p\" in *.rs");
    }

    #[test]
    fn summarize_tool_arg_grep_no_location() {
        let s = summarize_tool_arg(Some("Grep"), &json!({"pattern": "x"}));
        assert_eq!(s, "\"x\"");
    }

    #[test]
    fn summarize_tool_arg_glob() {
        let s = summarize_tool_arg(Some("Glob"), &json!({"pattern": "src/**/*.rs"}));
        assert_eq!(s, "src/**/*.rs");
    }

    #[test]
    fn summarize_tool_arg_webfetch() {
        let s = summarize_tool_arg(Some("WebFetch"), &json!({"url": "https://example.com"}));
        assert_eq!(s, "https://example.com");
    }

    #[test]
    fn summarize_tool_arg_websearch() {
        let s = summarize_tool_arg(Some("WebSearch"), &json!({"query": "hello"}));
        assert_eq!(s, "\"hello\"");
    }

    #[test]
    fn summarize_tool_arg_task_description() {
        let s = summarize_tool_arg(Some("Task"), &json!({"description": "run tests"}));
        assert_eq!(s, "run tests");
    }

    #[test]
    fn summarize_tool_arg_agent_subagent_type_fallback() {
        let s = summarize_tool_arg(Some("Agent"), &json!({"subagent_type": "reviewer"}));
        assert_eq!(s, "reviewer");
    }

    #[test]
    fn summarize_tool_arg_task_truncated_to_80_chars() {
        let long = "d".repeat(120);
        let s = summarize_tool_arg(Some("Task"), &json!({"description": long}));
        assert_eq!(s.chars().count(), 80);
    }

    #[test]
    fn summarize_tool_arg_todowrite_counts_items() {
        let s = summarize_tool_arg(Some("TodoWrite"), &json!({"todos": [1, 2, 3]}));
        assert_eq!(s, "3 todos");
    }

    #[test]
    fn summarize_tool_arg_todowrite_missing_array() {
        let s = summarize_tool_arg(Some("TodoWrite"), &json!({}));
        assert_eq!(s, "0 todos");
    }

    #[test]
    fn summarize_tool_arg_unknown_tool_returns_empty() {
        let s = summarize_tool_arg(Some("Unknown"), &json!({"command": "x"}));
        assert_eq!(s, "");
    }

    #[test]
    fn transcript_path_for_no_cwd_returns_none() {
        let _ = set_test_home();
        assert!(transcript_path_for("sid", None).is_none());
    }

    #[test]
    fn transcript_path_for_missing_file_returns_none() {
        let _ = set_test_home();
        assert!(transcript_path_for("nope", Some("/Users/x")).is_none());
    }

    #[test]
    fn transcript_path_for_existing_file_returns_path() {
        let _g = LOCK.lock().unwrap();
        let path = write_project_transcript("/Users/me/proj", "s1", "{}\n");
        let resolved = transcript_path_for("s1", Some("/Users/me/proj")).unwrap();
        assert_eq!(resolved, path);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn title_cached_missing_file_returns_none_none() {
        let _ = set_test_home();
        let (t, fu) = title_cached(std::path::Path::new("/nope/no.jsonl"));
        assert!(t.is_none() && fu.is_none());
    }

    #[test]
    fn title_cached_extracts_custom_title() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"custom-title","customTitle":"My Chat"}"#;
        let p = write_project_transcript("/x/ct", "s", jsonl);
        let (t, _) = title_cached(&p);
        assert_eq!(t.as_deref(), Some("My Chat"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn title_cached_ignores_empty_custom_title() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"custom-title","customTitle":""}"#;
        let p = write_project_transcript("/x/ct2", "s", jsonl);
        let (t, _) = title_cached(&p);
        assert!(t.is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn title_cached_agent_name_used_when_no_custom_title() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"agent-name","agentName":"Agent X"}"#;
        let p = write_project_transcript("/x/an", "s", jsonl);
        let (t, _) = title_cached(&p);
        assert_eq!(t.as_deref(), Some("Agent X"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn title_cached_custom_title_overrides_agent_name() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"agent-name","agentName":"A"}
{"type":"custom-title","customTitle":"C"}"#;
        let p = write_project_transcript("/x/ov", "s", jsonl);
        let (t, _) = title_cached(&p);
        assert_eq!(t.as_deref(), Some("C"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn title_cached_first_user_from_string_content() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"user","message":{"content":"hello there"}}"#;
        let p = write_project_transcript("/x/su", "s", jsonl);
        let (_, fu) = title_cached(&p);
        assert_eq!(fu.as_deref(), Some("hello there"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn title_cached_first_user_from_array_content() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"user","message":{"content":[{"type":"text","text":"hi array"}]}}"#;
        let p = write_project_transcript("/x/ac", "s", jsonl);
        let (_, fu) = title_cached(&p);
        assert_eq!(fu.as_deref(), Some("hi array"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn title_cached_first_user_truncated_over_80_chars() {
        let _g = LOCK.lock().unwrap();
        let long = "a".repeat(100);
        let jsonl = format!(r#"{{"type":"user","message":{{"content":"{long}"}}}}"#);
        let p = write_project_transcript("/x/long", "s", &jsonl);
        let (_, fu) = title_cached(&p);
        let fu = fu.unwrap();
        assert!(fu.ends_with('\u{2026}'));
        assert_eq!(fu.chars().count(), 78);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn title_cached_skips_invalid_json_lines() {
        let _g = LOCK.lock().unwrap();
        let jsonl = "not json\n{\"type\":\"custom-title\",\"customTitle\":\"Keep\"}";
        let p = write_project_transcript("/x/skip", "s", jsonl);
        let (t, _) = title_cached(&p);
        assert_eq!(t.as_deref(), Some("Keep"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn session_title_missing_transcript_returns_none_none() {
        let _ = set_test_home();
        let (t, fu) = session_title("nope", Some("/x"));
        assert!(t.is_none() && fu.is_none());
    }

    #[test]
    fn session_title_reads_from_file() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"custom-title","customTitle":"T"}"#;
        let p = write_project_transcript("/x/st", "sid", jsonl);
        let (t, _) = session_title("sid", Some("/x/st"));
        assert_eq!(t.as_deref(), Some("T"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_tail_missing_transcript_returns_empty_object() {
        let _ = set_test_home();
        let v = jsonl_tail("nope", Some("/x"));
        assert_eq!(v, json!({}));
    }

    #[test]
    fn jsonl_tail_returns_empty_when_transcript_is_directory() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/x/dir-as-file";
        let sid = "dir-sid";
        let slug = format!("-{}", cwd.replace('/', "-").trim_start_matches('-'));
        let dir = PROJECTS_DIR.join(&slug);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join(format!("{sid}.jsonl"));
        std::fs::create_dir_all(&fake).unwrap();
        let v = jsonl_tail(sid, Some(cwd));
        let _ = std::fs::remove_dir(&fake);
        assert_eq!(v, json!({}));
    }

    #[test]
    fn jsonl_tail_extracts_assistant_text() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"content":[{"type":"text","text":"reply"}]}}"#;
        let p = write_project_transcript("/x/jt", "s", jsonl);
        let v = jsonl_tail("s", Some("/x/jt"));
        assert_eq!(v.get("last_type").and_then(Value::as_str), Some("assistant"));
        assert_eq!(v.get("last_assistant_preview").and_then(Value::as_str), Some("reply"));
        assert_eq!(v.get("pending").and_then(Value::as_bool), Some(false));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_tail_tracks_pending_tool_use() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#;
        let p = write_project_transcript("/x/pt", "s", jsonl);
        let v = jsonl_tail("s", Some("/x/pt"));
        assert_eq!(v.get("pending").and_then(Value::as_bool), Some(true));
        assert_eq!(v.get("pending_tool").and_then(Value::as_str), Some("Bash"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_tail_tool_result_clears_pending() {
        let _g = LOCK.lock().unwrap();
        let jsonl = concat!(
            r#"{"type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"done"}]}}"#
        );
        let p = write_project_transcript("/x/pt2", "s", jsonl);
        let v = jsonl_tail("s", Some("/x/pt2"));
        assert_eq!(v.get("pending").and_then(Value::as_bool), Some(false));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_tail_user_text_clears_pending() {
        let _g = LOCK.lock().unwrap();
        let jsonl = concat!(
            r#"{"type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"new prompt"}}"#
        );
        let p = write_project_transcript("/x/pt3", "s", jsonl);
        let v = jsonl_tail("s", Some("/x/pt3"));
        assert_eq!(v.get("pending").and_then(Value::as_bool), Some(false));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_tail_skips_sidechain() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"assistant","isSidechain":true,"timestamp":"2024-01-01T00:00:00Z","message":{"content":[{"type":"text","text":"skip"}]}}"#;
        let p = write_project_transcript("/x/sc", "s", jsonl);
        let v = jsonl_tail("s", Some("/x/sc"));
        assert!(v.get("last_type").unwrap_or(&Value::Null).is_null());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_tail_skips_meta() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"assistant","isMeta":true,"timestamp":"2024-01-01T00:00:00Z","message":{"content":[{"type":"text","text":"skip"}]}}"#;
        let p = write_project_transcript("/x/mt", "s", jsonl);
        let v = jsonl_tail("s", Some("/x/mt"));
        assert!(v.get("last_type").unwrap_or(&Value::Null).is_null());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_tail_assistant_preview_truncated_over_140() {
        let _g = LOCK.lock().unwrap();
        let long = "a".repeat(200);
        let jsonl = format!(
            r#"{{"type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{{"content":[{{"type":"text","text":"{long}"}}]}}}}"#
        );
        let p = write_project_transcript("/x/tt", "s", &jsonl);
        let v = jsonl_tail("s", Some("/x/tt"));
        let preview = v.get("last_assistant_preview").and_then(Value::as_str).unwrap();
        assert!(preview.ends_with('\u{2026}'));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_summary_missing_transcript_returns_empty_skeleton() {
        let _ = set_test_home();
        let v = jsonl_summary("none", Some("/x"));
        assert!(v.get("goal").unwrap().is_null());
        assert_eq!(v.get("actions").and_then(Value::as_array).unwrap().len(), 0);
        assert!(v.get("paragraph").unwrap().is_null());
    }

    #[test]
    fn jsonl_summary_returns_empty_when_transcript_is_directory() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/x/sum-dir";
        let sid = "sum-dir-sid";
        let slug = format!("-{}", cwd.replace('/', "-").trim_start_matches('-'));
        let dir = PROJECTS_DIR.join(&slug);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join(format!("{sid}.jsonl"));
        std::fs::create_dir_all(&fake).unwrap();
        let v = jsonl_summary(sid, Some(cwd));
        let _ = std::fs::remove_dir(&fake);
        assert!(v.get("goal").unwrap().is_null());
        assert_eq!(v.get("actions").and_then(Value::as_array).unwrap().len(), 0);
    }

    #[test]
    fn jsonl_summary_extracts_goal_from_user_message() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"content":"Fix the bug"}}"#;
        let p = write_project_transcript("/x/jg", "s", jsonl);
        let v = jsonl_summary("s", Some("/x/jg"));
        assert_eq!(v.get("goal").and_then(Value::as_str), Some("Fix the bug"));
        assert_eq!(v.get("prompt_count").and_then(Value::as_u64), Some(1));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_summary_ignores_meta_prompts() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"content":"<command-name>/x"}}"#;
        let p = write_project_transcript("/x/jm", "s", jsonl);
        let v = jsonl_summary("s", Some("/x/jm"));
        assert!(v.get("goal").unwrap().is_null());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_summary_truncates_long_goal_with_ellipsis() {
        let _g = LOCK.lock().unwrap();
        let long = "g".repeat(250);
        let jsonl = format!(r#"{{"type":"user","message":{{"content":"{long}"}}}}"#);
        let p = write_project_transcript("/x/jl", "s", &jsonl);
        let v = jsonl_summary("s", Some("/x/jl"));
        let g = v.get("goal").and_then(Value::as_str).unwrap();
        assert!(g.ends_with('\u{2026}'));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_summary_captures_assistant_actions() {
        let _g = LOCK.lock().unwrap();
        let jsonl = concat!(
            r#"{"type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"content":"do it"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2024-01-01T00:00:01Z","message":{"content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"echo hi"}}]}}"#
        );
        let p = write_project_transcript("/x/ja", "s", jsonl);
        let v = jsonl_summary("s", Some("/x/ja"));
        let actions = v.get("actions").and_then(Value::as_array).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].get("tool").and_then(Value::as_str), Some("Bash"));
        assert_eq!(v.get("action_count").and_then(Value::as_u64), Some(1));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_summary_dedupes_consecutive_identical_actions() {
        let _g = LOCK.lock().unwrap();
        let jsonl = concat!(
            r#"{"type":"user","message":{"content":"go"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"Bash","input":{"command":"ls"}}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b","name":"Bash","input":{"command":"ls"}}]}}"#
        );
        let p = write_project_transcript("/x/jd", "s", jsonl);
        let v = jsonl_summary("s", Some("/x/jd"));
        let actions = v.get("actions").and_then(Value::as_array).unwrap();
        assert_eq!(actions.len(), 1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_summary_captures_last_assistant_text() {
        let _g = LOCK.lock().unwrap();
        let jsonl = concat!(
            r#"{"type":"user","message":{"content":"q"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2024-01-01T00:00:05Z","message":{"content":[{"type":"text","text":"reply here"}]}}"#
        );
        let p = write_project_transcript("/x/jlt", "s", jsonl);
        let v = jsonl_summary("s", Some("/x/jlt"));
        assert_eq!(v.get("last_text").and_then(Value::as_str), Some("reply here"));
        assert_eq!(
            v.get("last_assistant_timestamp").and_then(Value::as_str),
            Some("2024-01-01T00:00:05Z")
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_summary_merges_cached_paragraph() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"user","message":{"content":"ask"}}"#;
        let p = write_project_transcript("/x/jmp", "sid1", jsonl);

        std::fs::create_dir_all(&*SUMMARY_DIR).unwrap();
        let cache = SUMMARY_DIR.join("sid1.json");
        std::fs::write(
            &cache,
            serde_json::to_string(&json!({"paragraph": "summary text", "updated_at": 100.0}))
                .unwrap(),
        )
        .unwrap();

        let v = jsonl_summary("sid1", Some("/x/jmp"));
        assert_eq!(v.get("paragraph").and_then(Value::as_str), Some("summary text"));
        assert!((v.get("paragraph_updated_at").and_then(Value::as_f64).unwrap() - 100.0).abs() < 1e-6);

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&cache);
    }

    #[test]
    fn default_limit_helper() {
        assert_eq!(default_limit(), Some(60));
    }

    #[test]
    fn transcript_query_default() {
        let q: TranscriptQuery = serde_json::from_value(json!({})).unwrap();
        assert_eq!(q.limit, Some(60));
    }

    #[test]
    fn transcript_query_explicit() {
        let q: TranscriptQuery = serde_json::from_value(json!({"limit": 10})).unwrap();
        assert_eq!(q.limit, Some(10));
    }

    #[test]
    fn read_tail_missing_file_returns_none() {
        assert!(read_tail(std::path::Path::new("/nope/x.txt"), 1024).is_none());
    }

    #[test]
    fn read_tail_returns_last_n_bytes() {
        let _g = LOCK.lock().unwrap();
        let home = set_test_home();
        let p = home.join("tail-test.txt");
        std::fs::write(&p, "0123456789abcdef").unwrap();
        let s = read_tail(&p, 4).unwrap();
        assert_eq!(s, "cdef");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_tail_file_smaller_than_limit_returns_all() {
        let _g = LOCK.lock().unwrap();
        let home = set_test_home();
        let p = home.join("tail-small.txt");
        std::fs::write(&p, "abc").unwrap();
        let s = read_tail(&p, 1024).unwrap();
        assert_eq!(s, "abc");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn summary_from_data_empty_input() {
        let v = summary_from_data("");
        assert!(v.get("goal").unwrap().is_null());
        assert_eq!(v.get("prompt_count").and_then(Value::as_u64), Some(0));
        assert_eq!(v.get("action_count").and_then(Value::as_u64), Some(0));
    }

    #[test]
    fn summary_from_data_skips_sidechain_and_meta() {
        let data = concat!(
            r#"{"type":"user","isSidechain":true,"message":{"content":"hide"}}"#,
            "\n",
            r#"{"type":"user","isMeta":true,"message":{"content":"hide"}}"#
        );
        let v = summary_from_data(data);
        assert_eq!(v.get("prompt_count").and_then(Value::as_u64), Some(0));
    }

    #[test]
    fn summary_from_data_keeps_recent_prompts_up_to_5() {
        let mut data = String::new();
        for i in 0..8 {
            data.push_str(&format!(
                r#"{{"type":"user","message":{{"content":"p{i}"}}}}"#
            ));
            data.push('\n');
        }
        let v = summary_from_data(&data);
        let recent = v.get("recent_prompts").and_then(Value::as_array).unwrap();
        assert_eq!(recent.len(), 5);
        assert_eq!(recent[0].as_str(), Some("p3"));
    }

    #[test]
    fn summary_from_data_trims_actions_to_last_8() {
        let mut data = String::from(r#"{"type":"user","message":{"content":"go"}}
"#);
        for i in 0..12 {
            data.push_str(&format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t{i}","name":"Bash","input":{{"command":"echo {i}"}}}}]}}}}"#
            ));
            data.push('\n');
        }
        let v = summary_from_data(&data);
        let actions = v.get("actions").and_then(Value::as_array).unwrap();
        assert_eq!(actions.len(), 8);
        assert_eq!(v.get("action_count").and_then(Value::as_u64), Some(12));
    }

    #[test]
    fn summary_from_data_user_string_content_goal() {
        let data = r#"{"type":"user","message":{"content":"hello"}}"#;
        let v = summary_from_data(data);
        assert_eq!(v.get("goal").and_then(Value::as_str), Some("hello"));
    }

    #[test]
    fn summary_from_data_user_array_text_content_goal() {
        let data = r#"{"type":"user","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        let v = summary_from_data(data);
        assert_eq!(v.get("goal").and_then(Value::as_str), Some("hi"));
    }

    #[test]
    fn summary_from_data_long_assistant_text_truncated() {
        let long = "b".repeat(500);
        let data = format!(
            concat!(
                r#"{{"type":"user","message":{{"content":"q"}}}}"#,
                "\n",
                r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{0}"}}]}}}}"#
            ),
            long
        );
        let v = summary_from_data(&data);
        let lt = v.get("last_text").and_then(Value::as_str).unwrap();
        assert!(lt.ends_with('\u{2026}'));
    }

    fn app_state_for_transcript(instances: Vec<crate::http::dto::InstanceData>) -> Arc<AppState> {
        use crate::services::instances::{InstancesCache, PendingFocus, PsCache};
        use crate::infra::tmux::TmuxConfig;
        use std::collections::HashMap;
        use std::sync::Mutex;
        Arc::new(AppState {
            tmux_config: TmuxConfig {
                tmux_bin: "/nope".into(),
                socket_name: "ciu-test".into(),
                name_prefix: "ciu-".into(),
            },
            auth_token: "t".into(),
            server_start: 1.0,
            instances_cache: Mutex::new(InstancesCache {
                at: f64::MAX,
                data: instances,
            }),
            pending_focus: Mutex::new(PendingFocus { sid: None, ts: 0.0 }),
            ps_cache: Mutex::new(PsCache {
                at: 0.0,
                map: HashMap::new(),
            }),
        })
    }

    fn inst_t(session_id: &str, cwd: Option<&str>) -> crate::http::dto::InstanceData {
        crate::http::dto::InstanceData {
            session_id: session_id.into(),
            pid: None,
            alive: true,
            name: String::new(),
            title: None,
            custom_name: None,
            first_user: None,
            cwd: cwd.map(String::from),
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
            our_sid: Some("o1".into()),
            tmux_session: None,
        }
    }

    #[tokio::test]
    async fn api_transcript_returns_not_found_when_session_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let state = app_state_for_transcript(vec![]);
        let res = api_transcript(
            State(state),
            axum::extract::Path("missing".into()),
            axum::extract::Query(TranscriptQuery { limit: None }),
        )
        .await;
        assert_eq!(res.err(), Some(StatusCode::NOT_FOUND));
    }

    #[tokio::test]
    async fn api_transcript_empty_entries_when_no_file() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let state = app_state_for_transcript(vec![inst_t("sid1", None)]);
        let resp = api_transcript(
            State(state),
            axum::extract::Path("sid1".into()),
            axum::extract::Query(TranscriptQuery { limit: None }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.entries.len(), 0);
        assert_eq!(
            resp.0.session.get("session_id").and_then(Value::as_str),
            Some("sid1")
        );
    }

    #[tokio::test]
    async fn api_transcript_text_content_user_and_assistant() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/Users/me/px1";
        let sid = "sid-tx-1";
        let jsonl = format!(
            "{}\n{}\n",
            r#"{"type":"user","uuid":"u1","timestamp":"2024-01-01T00:00:00Z","message":{"content":"hello"}}"#,
            r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"hi"}]}}"#
        );
        write_project_transcript(cwd, sid, &jsonl);
        let state = app_state_for_transcript(vec![inst_t(sid, Some(cwd))]);
        let resp = api_transcript(
            State(state),
            axum::extract::Path(sid.into()),
            axum::extract::Query(TranscriptQuery { limit: None }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.entries.len(), 2);
        assert_eq!(resp.0.entries[0].entry_type, "user");
        assert_eq!(resp.0.entries[1].entry_type, "assistant");
    }

    #[tokio::test]
    async fn api_transcript_skips_sidechain_and_meta() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/Users/me/px2";
        let sid = "sid-tx-2";
        let jsonl = format!(
            "{}\n{}\n{}\n",
            r#"{"type":"user","isSidechain":true,"message":{"content":"skip-sc"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"skip-meta"}}"#,
            r#"{"type":"user","uuid":"u1","message":{"content":"keep"}}"#
        );
        write_project_transcript(cwd, sid, &jsonl);
        let state = app_state_for_transcript(vec![inst_t(sid, Some(cwd))]);
        let resp = api_transcript(
            State(state),
            axum::extract::Path(sid.into()),
            axum::extract::Query(TranscriptQuery { limit: None }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.entries.len(), 1);
        assert_eq!(resp.0.entries[0].uuid.as_deref(), Some("u1"));
    }

    #[tokio::test]
    async fn api_transcript_parses_tool_use_and_tool_result() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/Users/me/px3";
        let sid = "sid-tx-3";
        let jsonl = format!(
            "{}\n{}\n",
            r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","uuid":"u1","message":{"content":[{"type":"tool_result","content":"ok","is_error":false}]}}"#
        );
        write_project_transcript(cwd, sid, &jsonl);
        let state = app_state_for_transcript(vec![inst_t(sid, Some(cwd))]);
        let resp = api_transcript(
            State(state),
            axum::extract::Path(sid.into()),
            axum::extract::Query(TranscriptQuery { limit: None }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.entries.len(), 2);
        let p0 = &resp.0.entries[0].parts[0];
        assert_eq!(p0.get("kind").and_then(Value::as_str), Some("tool_use"));
        assert_eq!(p0.get("tool").and_then(Value::as_str), Some("Bash"));
        let p1 = &resp.0.entries[1].parts[0];
        assert_eq!(p1.get("kind").and_then(Value::as_str), Some("tool_result"));
        assert_eq!(p1.get("text").and_then(Value::as_str), Some("ok"));
    }

    #[tokio::test]
    async fn api_transcript_parses_tool_result_array_text() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/Users/me/px4";
        let sid = "sid-tx-4";
        let jsonl = format!(
            "{}\n",
            r#"{"type":"user","uuid":"u1","message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}]}}"#
        );
        write_project_transcript(cwd, sid, &jsonl);
        let state = app_state_for_transcript(vec![inst_t(sid, Some(cwd))]);
        let resp = api_transcript(
            State(state),
            axum::extract::Path(sid.into()),
            axum::extract::Query(TranscriptQuery { limit: None }),
        )
        .await
        .unwrap();
        assert_eq!(
            resp.0.entries[0].parts[0].get("text").and_then(Value::as_str),
            Some("a\nb")
        );
    }

    #[tokio::test]
    async fn api_transcript_parses_thinking_kind() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/Users/me/px5";
        let sid = "sid-tx-5";
        let jsonl = format!(
            "{}\n",
            r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"thinking","thinking":"pondering"}]}}"#
        );
        write_project_transcript(cwd, sid, &jsonl);
        let state = app_state_for_transcript(vec![inst_t(sid, Some(cwd))]);
        let resp = api_transcript(
            State(state),
            axum::extract::Path(sid.into()),
            axum::extract::Query(TranscriptQuery { limit: None }),
        )
        .await
        .unwrap();
        assert_eq!(
            resp.0.entries[0].parts[0].get("kind").and_then(Value::as_str),
            Some("thinking")
        );
        assert_eq!(
            resp.0.entries[0].parts[0].get("text").and_then(Value::as_str),
            Some("pondering")
        );
    }

    #[tokio::test]
    async fn api_transcript_skips_unknown_types_and_bad_json_lines() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/Users/me/px6";
        let sid = "sid-tx-6";
        let jsonl = format!(
            "{}\n{}\n{}\n",
            "not json",
            r#"{"type":"system","message":{"content":"skip"}}"#,
            r#"{"type":"user","uuid":"u1","message":{"content":"ok"}}"#
        );
        write_project_transcript(cwd, sid, &jsonl);
        let state = app_state_for_transcript(vec![inst_t(sid, Some(cwd))]);
        let resp = api_transcript(
            State(state),
            axum::extract::Path(sid.into()),
            axum::extract::Query(TranscriptQuery { limit: None }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.entries.len(), 1);
        assert_eq!(resp.0.entries[0].uuid.as_deref(), Some("u1"));
    }

    #[tokio::test]
    async fn api_transcript_applies_limit_truncating_entries() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/Users/me/px7";
        let sid = "sid-tx-7";
        let mut jsonl = String::new();
        for i in 0..10 {
            jsonl.push_str(&format!(
                r#"{{"type":"user","uuid":"u{i}","message":{{"content":"m{i}"}}}}"#
            ));
            jsonl.push('\n');
        }
        write_project_transcript(cwd, sid, &jsonl);
        let state = app_state_for_transcript(vec![inst_t(sid, Some(cwd))]);
        let resp = api_transcript(
            State(state),
            axum::extract::Path(sid.into()),
            axum::extract::Query(TranscriptQuery { limit: Some(3) }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.entries.len(), 3);
        assert_eq!(resp.0.entries[0].uuid.as_deref(), Some("u7"));
        assert_eq!(resp.0.entries[2].uuid.as_deref(), Some("u9"));
    }

    #[tokio::test]
    async fn api_transcript_skips_entries_with_empty_parts() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/Users/me/px8";
        let sid = "sid-tx-8";
        let jsonl = format!(
            "{}\n{}\n",
            r#"{"type":"user","uuid":"u1","message":{"content":[{"type":"unknown","foo":"bar"}]}}"#,
            r#"{"type":"user","uuid":"u2","message":{"content":"present"}}"#
        );
        write_project_transcript(cwd, sid, &jsonl);
        let state = app_state_for_transcript(vec![inst_t(sid, Some(cwd))]);
        let resp = api_transcript(
            State(state),
            axum::extract::Path(sid.into()),
            axum::extract::Query(TranscriptQuery { limit: None }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.entries.len(), 1);
        assert_eq!(resp.0.entries[0].uuid.as_deref(), Some("u2"));
    }

    #[test]
    fn jsonl_tail_invalid_json_line_is_skipped() {
        let _g = LOCK.lock().unwrap();
        let jsonl = "not json\n{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}";
        let p = write_project_transcript("/x/invalidjl", "s", jsonl);
        let v = jsonl_tail("s", Some("/x/invalidjl"));
        assert_eq!(
            v.get("last_assistant_preview").and_then(Value::as_str),
            Some("ok")
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_tail_unknown_type_skipped() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"system","message":{"content":"x"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"keep"}]}}"#;
        let p = write_project_transcript("/x/unkjl", "s", jsonl);
        let v = jsonl_tail("s", Some("/x/unkjl"));
        assert_eq!(
            v.get("last_assistant_preview").and_then(Value::as_str),
            Some("keep")
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_tail_user_array_with_non_object_items_and_text() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash"}]}}
{"type":"user","message":{"content":[1,"string-not-obj",{"type":"text","text":"real user text"},{"type":"tool_result","tool_use_id":"t1"}]}}"#;
        let p = write_project_transcript("/x/uaobj", "s", jsonl);
        let v = jsonl_tail("s", Some("/x/uaobj"));
        assert_eq!(v.get("pending").and_then(Value::as_bool), Some(false));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn jsonl_tail_assistant_array_with_non_object_items() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"assistant","message":{"content":[1,"str",{"type":"text","text":"kept"}]}}"#;
        let p = write_project_transcript("/x/aobj", "s", jsonl);
        let v = jsonl_tail("s", Some("/x/aobj"));
        assert_eq!(
            v.get("last_assistant_preview").and_then(Value::as_str),
            Some("kept")
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn summary_from_data_invalid_json_line_skipped() {
        let data = "not json\n{\"type\":\"user\",\"message\":{\"content\":\"hi there\"}}";
        let v = summary_from_data(data);
        assert_eq!(v.get("goal").and_then(Value::as_str), Some("hi there"));
    }

    #[test]
    fn summary_from_data_unknown_type_skipped() {
        let data = r#"{"type":"system","message":{"content":"x"}}
{"type":"user","message":{"content":"goal line"}}"#;
        let v = summary_from_data(data);
        assert_eq!(v.get("goal").and_then(Value::as_str), Some("goal line"));
    }

    #[test]
    fn summary_from_data_user_array_tool_result_is_not_a_goal() {
        let data = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1"},{"type":"text","text":"text after"}]}}"#;
        let v = summary_from_data(data);
        assert!(v.get("goal").is_none() || v.get("goal").unwrap().is_null());
    }

    #[test]
    fn summary_from_data_user_array_non_object_items_skipped() {
        let data = r#"{"type":"user","message":{"content":[1,"skip",{"type":"text","text":"real goal"}]}}"#;
        let v = summary_from_data(data);
        assert_eq!(v.get("goal").and_then(Value::as_str), Some("real goal"));
    }

    #[test]
    fn summary_from_data_assistant_array_non_object_skipped() {
        let data = r#"{"type":"user","message":{"content":"g"}}
{"type":"assistant","message":{"content":[1,"str",{"type":"text","text":"txt"}]}}"#;
        let v = summary_from_data(data);
        assert_eq!(v.get("last_text").and_then(Value::as_str), Some("txt"));
    }

    #[test]
    fn summary_from_data_drains_actions_over_14() {
        let mut lines = vec![r#"{"type":"user","message":{"content":"goal"}}"#.to_string()];
        for i in 0..20 {
            lines.push(format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Bash","input":{{"command":"cmd{i}"}}}}]}}}}"#
            ));
        }
        let data = lines.join("\n");
        let v = summary_from_data(&data);
        let count = v.get("action_count").and_then(Value::as_u64).unwrap();
        assert_eq!(count, 20);
        let actions = v.get("actions").and_then(Value::as_array).unwrap();
        assert!(actions.len() <= 8);
    }

    #[test]
    fn jsonl_summary_missing_tail_returns_empty_skeleton() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/x/emptyjsonl";
        let slug = format!("-{}", cwd.replace('/', "-").trim_start_matches('-'));
        let dir = PROJECTS_DIR.join(&slug);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.jsonl");
        std::fs::write(&p, "").unwrap();
        let v = jsonl_summary("s", Some(cwd));
        assert!(v.get("goal").unwrap().is_null());
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn api_transcript_skip_applied_when_lines_exceed_limit_times_four() {
        use crate::http::dto::InstanceData;
        use axum::extract::{Path as AxumPath, Query, State};
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/x/overlimit";
        let sid = "sover";
        let mut lines = Vec::new();
        for i in 0..50 {
            lines.push(format!(
                r#"{{"type":"user","uuid":"u{i}","message":{{"content":"msg{i}"}}}}"#
            ));
        }
        let p = write_project_transcript(cwd, sid, &lines.join("\n"));
        let inst = inst_t(sid, Some(cwd));
        let state = app_state_for_transcript(vec![InstanceData { ..inst }]);
        let resp = api_transcript(
            State(state),
            AxumPath(sid.into()),
            Query(TranscriptQuery { limit: Some(5) }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.entries.len(), 5);
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn api_transcript_tool_result_non_string_non_array_is_empty() {
        use axum::extract::{Path as AxumPath, Query, State};
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/x/trnsonull";
        let sid = "strns";
        let jsonl = r#"{"type":"user","uuid":"u1","message":{"content":[{"type":"tool_result","content":null}]}}"#;
        let p = write_project_transcript(cwd, sid, jsonl);
        let state = app_state_for_transcript(vec![inst_t(sid, Some(cwd))]);
        let resp = api_transcript(
            State(state),
            AxumPath(sid.into()),
            Query(TranscriptQuery { limit: None }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.entries.len(), 1);
        let part = &resp.0.entries[0].parts[0];
        assert_eq!(part.get("text").and_then(Value::as_str), Some(""));
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn api_transcript_user_content_array_with_non_object_items_skipped() {
        use axum::extract::{Path as AxumPath, Query, State};
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/x/userarrnon";
        let sid = "sunonobj";
        let jsonl = r#"{"type":"user","uuid":"u1","message":{"content":[1,"bare",{"type":"text","text":"hello"}]}}"#;
        let p = write_project_transcript(cwd, sid, jsonl);
        let state = app_state_for_transcript(vec![inst_t(sid, Some(cwd))]);
        let resp = api_transcript(
            State(state),
            AxumPath(sid.into()),
            Query(TranscriptQuery { limit: None }),
        )
        .await
        .unwrap();
        let parts = &resp.0.entries[0].parts;
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].get("text").and_then(Value::as_str), Some("hello"));
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn api_transcript_tool_result_array_with_non_text_items_skipped() {
        use axum::extract::{Path as AxumPath, Query, State};
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let cwd = "/x/trarrm";
        let sid = "strarr";
        let jsonl = r#"{"type":"user","uuid":"u1","message":{"content":[{"type":"tool_result","content":[{"type":"image"},{"type":"text","text":"keep me"},1]}]}}"#;
        let p = write_project_transcript(cwd, sid, jsonl);
        let state = app_state_for_transcript(vec![inst_t(sid, Some(cwd))]);
        let resp = api_transcript(
            State(state),
            AxumPath(sid.into()),
            Query(TranscriptQuery { limit: None }),
        )
        .await
        .unwrap();
        let part = &resp.0.entries[0].parts[0];
        assert_eq!(part.get("text").and_then(Value::as_str), Some("keep me"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn title_cached_agent_name_empty_is_ignored() {
        let _g = LOCK.lock().unwrap();
        let jsonl = r#"{"type":"agent-name","agentName":""}
{"type":"agent-name","agentName":"Named"}"#;
        let p = write_project_transcript("/x/emptyname", "s", jsonl);
        let (t, _) = title_cached(&p);
        assert_eq!(t.as_deref(), Some("Named"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn title_cached_early_break_after_1000_lines_with_both_found() {
        let _g = LOCK.lock().unwrap();
        let mut lines = vec![
            r#"{"type":"custom-title","customTitle":"Title"}"#.to_string(),
            r#"{"type":"user","message":{"content":"hello"}}"#.to_string(),
        ];
        for _ in 0..1100 {
            lines.push(r#"{"type":"user","message":{"content":"later"}}"#.into());
        }
        let data = lines.join("\n");
        let p = write_project_transcript("/x/bigtitle", "s", &data);
        let (t, fu) = title_cached(&p);
        assert_eq!(t.as_deref(), Some("Title"));
        assert_eq!(fu.as_deref(), Some("hello"));
        let _ = std::fs::remove_file(&p);
    }
}
