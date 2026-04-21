use crate::paths;
use crate::server::instances::AppState;
use crate::server::types::*;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

const GLOBAL_MCP_PATHS: &[(&str, fn() -> PathBuf)] = &[
    ("Global (~/.claude.json)", || paths::HOME.join(".claude.json")),
    ("MCP (~/.claude/mcp.json)", || paths::GLOBAL_MCP.clone()),
];

fn read_json_safe(p: &std::path::Path) -> Value {
    if !p.is_file() {
        return json!({});
    }
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({}))
}

fn write_json_pretty(p: &std::path::Path, data: &Value) {
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        p,
        serde_json::to_string_pretty(data).unwrap_or_default() + "\n",
    );
}

pub async fn api_config_mcp_list(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mut results: Vec<Value> = Vec::new();

    for (label, path_fn) in GLOBAL_MCP_PATHS {
        let p = path_fn();
        let data = read_json_safe(&p);
        let servers = data
            .get("mcpServers")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        results.push(json!({
            "path": p.to_string_lossy(),
            "label": label,
            "exists": p.is_file(),
            "servers": servers,
        }));
    }

    let instances = state.cached_instances();
    for inst in &instances {
        let cmd = &inst.command;
        let cfg_path = extract_mcp_config_path(cmd);
        if let Some(ref cfg) = cfg_path {
            let p = PathBuf::from(cfg);
            if !p.is_file() {
                continue;
            }
            let data = read_json_safe(&p);
            let servers = data
                .get("mcpServers")
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            let name = inst
                .custom_name
                .as_deref()
                .unwrap_or_else(|| inst.our_sid.as_deref().unwrap_or(&inst.session_id));
            let name_short = if name.len() > 8 { &name[..8] } else { name };
            results.push(json!({
                "path": p.to_string_lossy(),
                "label": format!("Instance ({name_short})"),
                "exists": true,
                "servers": servers,
                "session_id": inst.session_id,
            }));
        }
    }

    let mut seen_cwds: HashSet<String> = HashSet::new();
    for inst in &instances {
        if let Some(ref cwd) = inst.cwd {
            if seen_cwds.contains(cwd) {
                continue;
            }
            seen_cwds.insert(cwd.clone());
            let proj_mcp = PathBuf::from(cwd).join(".mcp.json");
            if proj_mcp.is_file() {
                let data = read_json_safe(&proj_mcp);
                let servers = data
                    .get("mcpServers")
                    .and_then(Value::as_object)
                    .map(|m| m.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                let dir_name = PathBuf::from(cwd)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                results.push(json!({
                    "path": proj_mcp.to_string_lossy(),
                    "label": format!("Project ({dir_name})"),
                    "exists": true,
                    "servers": servers,
                }));
            }
        }
    }

    Json(json!({ "configs": results }))
}

pub async fn api_config_mcp_read(Json(body): Json<ConfigFileBody>) -> Json<Value> {
    let p = PathBuf::from(body.path.replace('~', &paths::HOME.to_string_lossy()));
    if !p.is_file() {
        return Json(json!({
            "path": p.to_string_lossy(),
            "exists": false,
            "content": "{}",
        }));
    }
    let content = std::fs::read_to_string(&p).unwrap_or_else(|_| "{}".to_string());
    Json(json!({
        "path": p.to_string_lossy(),
        "exists": true,
        "content": content,
    }))
}

pub async fn api_config_mcp_write(
    Json(body): Json<ConfigWriteBody>,
) -> std::result::Result<Json<Value>, (StatusCode, Json<Value>)> {
    let p = PathBuf::from(body.path.replace('~', &paths::HOME.to_string_lossy()));
    let parsed: Value = serde_json::from_str(&body.content).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"detail": format!("Invalid JSON: {e}")})),
        )
    })?;
    write_json_pretty(&p, &parsed);
    Ok(Json(json!({"ok": true, "path": p.to_string_lossy()})))
}

pub async fn api_config_skills_list(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mut results: Vec<Value> = Vec::new();

    scan_skill_dir(&paths::GLOBAL_COMMANDS_DIR, "Global", "global", &mut results);

    let mut seen: HashSet<String> = HashSet::new();
    for inst in state.cached_instances() {
        if let Some(ref cwd) = inst.cwd {
            if seen.contains(cwd) {
                continue;
            }
            seen.insert(cwd.clone());
            let proj_dir = PathBuf::from(cwd).join(".claude").join("commands");
            if proj_dir.is_dir() {
                let dir_name = PathBuf::from(cwd)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                scan_skill_dir(&proj_dir, &format!("Project ({dir_name})"), cwd, &mut results);
            }
        }
    }

    Json(json!({ "skills": results }))
}

fn scan_skill_dir(dir: &std::path::Path, label: &str, scope: &str, results: &mut Vec<Value>) {
    if !dir.is_dir() {
        return;
    }
    let mut entries: Vec<PathBuf> = Vec::new();
    collect_md_files(dir, &mut entries);
    entries.sort();
    for f in entries {
        let rel = f
            .strip_prefix(dir)
            .unwrap_or(&f)
            .to_string_lossy()
            .to_string();
        let name = rel.strip_suffix(".md").unwrap_or(&rel);
        results.push(json!({
            "path": f.to_string_lossy(),
            "name": name,
            "label": label,
            "scope": scope,
        }));
    }
}

fn collect_md_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_md_files(&p, out);
            } else if p.extension().is_some_and(|e| e == "md") {
                out.push(p);
            }
        }
    }
}

pub async fn api_config_skill_read(Json(body): Json<ConfigFileBody>) -> Json<Value> {
    let p = PathBuf::from(body.path.replace('~', &paths::HOME.to_string_lossy()));
    if !p.is_file() {
        return Json(json!({
            "path": p.to_string_lossy(),
            "exists": false,
            "content": "",
        }));
    }
    let content = std::fs::read_to_string(&p).unwrap_or_default();
    Json(json!({
        "path": p.to_string_lossy(),
        "exists": true,
        "content": content,
    }))
}

pub async fn api_config_skill_write(Json(body): Json<ConfigWriteBody>) -> Json<Value> {
    let p = PathBuf::from(body.path.replace('~', &paths::HOME.to_string_lossy()));
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&p, &body.content);
    Json(json!({"ok": true, "path": p.to_string_lossy()}))
}

pub async fn api_config_skill_create(
    Json(body): Json<SkillCreateBody>,
) -> std::result::Result<Json<Value>, (StatusCode, Json<Value>)> {
    let base = if body.scope == "global" {
        paths::GLOBAL_COMMANDS_DIR.clone()
    } else {
        PathBuf::from(&body.scope).join(".claude").join("commands")
    };
    let p = base.join(format!("{}.md", body.name));
    if p.exists() {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"detail": format!("Skill already exists: {}", p.display())})),
        ));
    }
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&p, format!("---\ndescription: {}\n---\n\n", body.name));
    Ok(Json(json!({"ok": true, "path": p.to_string_lossy()})))
}

pub async fn api_config_skill_delete(
    Json(body): Json<SkillDeleteBody>,
) -> std::result::Result<Json<Value>, (StatusCode, Json<Value>)> {
    let p = PathBuf::from(body.path.replace('~', &paths::HOME.to_string_lossy()));
    if !p.is_file() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"detail": "Skill not found"})),
        ));
    }
    let _ = std::fs::remove_file(&p);
    Ok(Json(json!({"ok": true})))
}

pub async fn api_config_claudemd_list(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mut results: Vec<Value> = Vec::new();

    let global_md = paths::CLAUDE_DIR.join("CLAUDE.md");
    results.push(json!({
        "path": global_md.to_string_lossy(),
        "label": "Global (~/.claude/CLAUDE.md)",
        "exists": global_md.is_file(),
        "scope": "global",
    }));

    let mut seen: HashSet<String> = HashSet::new();
    for inst in state.cached_instances() {
        if let Some(ref cwd) = inst.cwd {
            if seen.contains(cwd) {
                continue;
            }
            seen.insert(cwd.clone());
            let proj_md = PathBuf::from(cwd).join("CLAUDE.md");
            let dir_name = PathBuf::from(cwd)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            results.push(json!({
                "path": proj_md.to_string_lossy(),
                "label": format!("Project ({dir_name})"),
                "exists": proj_md.is_file(),
                "scope": cwd,
            }));
            let nested_md = PathBuf::from(cwd).join(".claude").join("CLAUDE.md");
            if nested_md.is_file() && nested_md != proj_md {
                results.push(json!({
                    "path": nested_md.to_string_lossy(),
                    "label": format!("Project .claude/ ({dir_name})"),
                    "exists": true,
                    "scope": cwd,
                }));
            }
        }
    }

    Json(json!({ "files": results }))
}

pub async fn api_config_claudemd_read(Json(body): Json<ConfigFileBody>) -> Json<Value> {
    let p = PathBuf::from(body.path.replace('~', &paths::HOME.to_string_lossy()));
    if !p.is_file() {
        return Json(json!({
            "path": p.to_string_lossy(),
            "exists": false,
            "content": "",
        }));
    }
    let content = std::fs::read_to_string(&p).unwrap_or_default();
    Json(json!({
        "path": p.to_string_lossy(),
        "exists": true,
        "content": content,
    }))
}

pub async fn api_config_claudemd_write(Json(body): Json<ConfigWriteBody>) -> Json<Value> {
    let p = PathBuf::from(body.path.replace('~', &paths::HOME.to_string_lossy()));
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&p, &body.content);
    Json(json!({"ok": true, "path": p.to_string_lossy()}))
}

fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        paths::HOME.join(rest)
    } else if raw == "~" {
        paths::HOME.clone()
    } else {
        PathBuf::from(raw)
    }
}

fn servers_from_value(data: &Value) -> Vec<String> {
    data.get("mcpServers")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
}

pub async fn api_mcp_list(Json(body): Json<McpListBody>) -> Json<Value> {
    let p = expand_tilde(body.path.trim());
    if !p.is_file() {
        return Json(json!({
            "path": p.to_string_lossy(),
            "exists": false,
            "mcps": Vec::<String>::new(),
        }));
    }
    let data = read_json_safe(&p);
    let mut mcps = servers_from_value(&data);
    mcps.sort();
    Json(json!({
        "path": p.to_string_lossy(),
        "exists": true,
        "mcps": mcps,
    }))
}

pub async fn api_mcp_sources() -> Json<Value> {
    let candidates: &[(&str, PathBuf)] = &[
        ("~/.claude.json", paths::HOME.join(".claude.json")),
        ("~/.claude/mcp.json", paths::GLOBAL_MCP.clone()),
    ];
    let sources: Vec<Value> = candidates
        .iter()
        .map(|(label, p)| {
            let exists = p.is_file();
            let count = if exists {
                Some(servers_from_value(&read_json_safe(p)).len())
            } else {
                None
            };
            json!({
                "path": label,
                "label": label,
                "exists": exists,
                "count": count,
            })
        })
        .collect();
    Json(json!({ "sources": sources }))
}

fn extract_mcp_config_path(cmd: &str) -> Option<String> {
    let idx = cmd.find("--mcp-config")?;
    let rest = &cmd[idx + "--mcp-config".len()..];
    let rest = rest.trim_start();
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let val = &rest[..end];
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}
