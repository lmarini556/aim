use crate::infra::paths;
use crate::services::instances::AppState;
use crate::http::dto::*;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{set_test_home, FS_LOCK as LOCK};

    #[test]
    fn extract_mcp_config_path_absent() {
        assert!(extract_mcp_config_path("claude --foo").is_none());
    }

    #[test]
    fn extract_mcp_config_path_with_value() {
        let r = extract_mcp_config_path("claude --mcp-config /a/b.json --other");
        assert_eq!(r.as_deref(), Some("/a/b.json"));
    }

    #[test]
    fn extract_mcp_config_path_value_at_end() {
        let r = extract_mcp_config_path("claude --mcp-config /x.json");
        assert_eq!(r.as_deref(), Some("/x.json"));
    }

    #[test]
    fn extract_mcp_config_path_empty_after_flag() {
        assert!(extract_mcp_config_path("claude --mcp-config ").is_none());
    }

    #[test]
    fn extract_mcp_config_path_tight_spacing() {
        let r = extract_mcp_config_path("--mcp-config/x.json");
        assert_eq!(r.as_deref(), Some("/x.json"));
    }

    #[test]
    fn servers_from_value_empty_object() {
        assert_eq!(servers_from_value(&json!({})), Vec::<String>::new());
    }

    #[test]
    fn servers_from_value_missing_key() {
        assert_eq!(servers_from_value(&json!({"other": 1})), Vec::<String>::new());
    }

    #[test]
    fn servers_from_value_extracts_keys() {
        let v = json!({"mcpServers": {"a": 1, "b": 2}});
        let mut r = servers_from_value(&v);
        r.sort();
        assert_eq!(r, vec!["a", "b"]);
    }

    #[test]
    fn servers_from_value_non_object_mcp_field() {
        assert_eq!(servers_from_value(&json!({"mcpServers": "oops"})), Vec::<String>::new());
    }

    #[test]
    fn expand_tilde_plain_path_unchanged() {
        let _ = set_test_home();
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn expand_tilde_alone_returns_home() {
        let h = set_test_home();
        assert_eq!(expand_tilde("~"), h);
    }

    #[test]
    fn expand_tilde_with_trailing_path() {
        let h = set_test_home();
        assert_eq!(expand_tilde("~/rest/of/path"), h.join("rest/of/path"));
    }

    #[test]
    fn expand_tilde_tilde_no_slash_not_expanded() {
        let _ = set_test_home();
        assert_eq!(expand_tilde("~user"), PathBuf::from("~user"));
    }

    #[test]
    fn read_json_safe_missing_returns_empty_object() {
        let v = read_json_safe(std::path::Path::new("/nope/x.json"));
        assert_eq!(v, json!({}));
    }

    #[test]
    fn read_json_safe_invalid_returns_empty_object() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("bad-cfg.json");
        std::fs::write(&p, "not json").unwrap();
        assert_eq!(read_json_safe(&p), json!({}));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_json_safe_valid_returns_parsed() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("good-cfg.json");
        std::fs::write(&p, r#"{"a": 1}"#).unwrap();
        assert_eq!(read_json_safe(&p), json!({"a": 1}));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn write_json_pretty_round_trip() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("wp-cfg.json");
        write_json_pretty(&p, &json!({"k": "v"}));
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains('\n'));
        assert!(content.ends_with('\n'));
        assert_eq!(read_json_safe(&p), json!({"k": "v"}));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn write_json_pretty_creates_parent_dir() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("nested").join("sub").join("out.json");
        write_json_pretty(&p, &json!({"x": 1}));
        assert!(p.is_file());
        let _ = std::fs::remove_dir_all(h.join("nested"));
    }

    #[test]
    fn collect_md_files_ignores_non_md() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let d = h.join("md-test");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.md"), "x").unwrap();
        std::fs::write(d.join("b.txt"), "y").unwrap();
        let mut out = Vec::new();
        collect_md_files(&d, &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("a.md"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn collect_md_files_recurses() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let d = h.join("md-rec");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("top.md"), "x").unwrap();
        std::fs::write(d.join("sub").join("nested.md"), "y").unwrap();
        let mut out = Vec::new();
        collect_md_files(&d, &mut out);
        assert_eq!(out.len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn collect_md_files_missing_dir_noop() {
        let mut out = Vec::new();
        collect_md_files(std::path::Path::new("/nope/missing"), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn scan_skill_dir_missing_dir_noop() {
        let mut out = Vec::new();
        scan_skill_dir(std::path::Path::new("/nope"), "L", "S", &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn scan_skill_dir_produces_entries_with_name() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let d = h.join("skill-scan");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("foo.md"), "x").unwrap();
        let mut out = Vec::new();
        scan_skill_dir(&d, "MyLabel", "myscope", &mut out);
        assert_eq!(out.len(), 1);
        let e = &out[0];
        assert_eq!(e.get("name").and_then(Value::as_str), Some("foo"));
        assert_eq!(e.get("label").and_then(Value::as_str), Some("MyLabel"));
        assert_eq!(e.get("scope").and_then(Value::as_str), Some("myscope"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn api_config_mcp_read_missing_file() {
        let _ = set_test_home();
        let r = api_config_mcp_read(Json(ConfigFileBody { path: "/nope.json".into() })).await;
        assert_eq!(r.get("exists").and_then(Value::as_bool), Some(false));
        assert_eq!(r.get("content").and_then(Value::as_str), Some("{}"));
    }

    #[tokio::test]
    async fn api_config_mcp_read_tilde_expansion() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("mcp-tilde.json");
        std::fs::write(&p, "{}").unwrap();
        let r = api_config_mcp_read(Json(ConfigFileBody { path: "~/mcp-tilde.json".into() })).await;
        assert_eq!(r.get("exists").and_then(Value::as_bool), Some(true));
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn api_config_mcp_write_rejects_invalid_json() {
        let _ = set_test_home();
        let r = api_config_mcp_write(Json(ConfigWriteBody {
            path: "/tmp/aim-mcp-badwrite.json".into(),
            content: "not json".into(),
        })).await;
        let (status, _) = r.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn api_config_mcp_write_accepts_valid_json() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("mcp-write.json");
        let r = api_config_mcp_write(Json(ConfigWriteBody {
            path: p.to_string_lossy().into(),
            content: r#"{"mcpServers": {}}"#.into(),
        })).await.unwrap();
        assert_eq!(r.get("ok").and_then(Value::as_bool), Some(true));
        assert!(p.is_file());
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn api_config_skill_read_missing() {
        let _ = set_test_home();
        let r = api_config_skill_read(Json(ConfigFileBody { path: "/nope.md".into() })).await;
        assert_eq!(r.get("exists").and_then(Value::as_bool), Some(false));
        assert_eq!(r.get("content").and_then(Value::as_str), Some(""));
    }

    #[tokio::test]
    async fn api_config_skill_read_existing() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("skill-r.md");
        std::fs::write(&p, "body").unwrap();
        let r = api_config_skill_read(Json(ConfigFileBody { path: p.to_string_lossy().into() })).await;
        assert_eq!(r.get("content").and_then(Value::as_str), Some("body"));
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn api_config_skill_write_creates_file() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("skill-w").join("a.md");
        let r = api_config_skill_write(Json(ConfigWriteBody {
            path: p.to_string_lossy().into(),
            content: "hello".into(),
        })).await;
        assert_eq!(r.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello");
        let _ = std::fs::remove_dir_all(h.join("skill-w"));
    }

    #[tokio::test]
    async fn api_config_skill_create_global_scope_writes_template() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let r = api_config_skill_create(Json(SkillCreateBody {
            scope: "global".into(),
            name: "newtool".into(),
        })).await.unwrap();
        let path_str = r.get("path").and_then(Value::as_str).unwrap().to_string();
        let p = PathBuf::from(&path_str);
        assert!(p.is_file());
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains("description: newtool"));
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn api_config_skill_create_conflict_if_exists() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let _first = api_config_skill_create(Json(SkillCreateBody {
            scope: "global".into(),
            name: "dupe".into(),
        })).await.unwrap();
        let second = api_config_skill_create(Json(SkillCreateBody {
            scope: "global".into(),
            name: "dupe".into(),
        })).await;
        let (status, _) = second.unwrap_err();
        assert_eq!(status, StatusCode::CONFLICT);
        let _ = std::fs::remove_file(paths::GLOBAL_COMMANDS_DIR.join("dupe.md"));
    }

    #[tokio::test]
    async fn api_config_skill_create_project_scope() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let proj = h.join("projskill");
        std::fs::create_dir_all(&proj).unwrap();
        let r = api_config_skill_create(Json(SkillCreateBody {
            scope: proj.to_string_lossy().into(),
            name: "px".into(),
        })).await.unwrap();
        let path_str = r.get("path").and_then(Value::as_str).unwrap();
        assert!(path_str.ends_with("px.md"));
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[tokio::test]
    async fn api_config_skill_delete_missing() {
        let _ = set_test_home();
        let r = api_config_skill_delete(Json(SkillDeleteBody {
            path: "/nope/x.md".into(),
        })).await;
        let (s, _) = r.unwrap_err();
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_config_skill_delete_existing() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("skill-del.md");
        std::fs::write(&p, "x").unwrap();
        let r = api_config_skill_delete(Json(SkillDeleteBody {
            path: p.to_string_lossy().into(),
        })).await.unwrap();
        assert_eq!(r.get("ok").and_then(Value::as_bool), Some(true));
        assert!(!p.exists());
    }

    #[tokio::test]
    async fn api_config_claudemd_read_missing() {
        let _ = set_test_home();
        let r = api_config_claudemd_read(Json(ConfigFileBody { path: "/nope.md".into() })).await;
        assert_eq!(r.get("exists").and_then(Value::as_bool), Some(false));
    }

    #[tokio::test]
    async fn api_config_claudemd_write_creates_file() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("CLAUDE.md");
        let r = api_config_claudemd_write(Json(ConfigWriteBody {
            path: p.to_string_lossy().into(),
            content: "# proj".into(),
        })).await;
        assert_eq!(r.get("ok").and_then(Value::as_bool), Some(true));
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn api_mcp_list_missing_file() {
        let _ = set_test_home();
        let r = api_mcp_list(Json(McpListBody { path: "/nope.json".into() })).await;
        assert_eq!(r.get("exists").and_then(Value::as_bool), Some(false));
        assert_eq!(r.get("mcps").and_then(Value::as_array).unwrap().len(), 0);
    }

    #[tokio::test]
    async fn api_mcp_list_existing_returns_sorted_names() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("mcp-list.json");
        std::fs::write(
            &p,
            r#"{"mcpServers": {"zeta": {}, "alpha": {}}}"#,
        ).unwrap();
        let r = api_mcp_list(Json(McpListBody { path: p.to_string_lossy().into() })).await;
        assert_eq!(r.get("exists").and_then(Value::as_bool), Some(true));
        let mcps: Vec<&str> = r.get("mcps").and_then(Value::as_array).unwrap()
            .iter().filter_map(Value::as_str).collect();
        assert_eq!(mcps, vec!["alpha", "zeta"]);
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn api_mcp_sources_returns_two_entries() {
        let _ = set_test_home();
        let r = api_mcp_sources().await;
        let sources = r.get("sources").and_then(Value::as_array).unwrap();
        assert_eq!(sources.len(), 2);
    }

    fn app_state_with_instances(instances: Vec<InstanceData>) -> Arc<AppState> {
        use crate::services::instances::{InstancesCache, PendingFocus, PsCache};
        use crate::infra::tmux::TmuxConfig;
        use std::collections::HashMap;
        use std::sync::Mutex;
        let s = Arc::new(AppState {
            tmux_config: TmuxConfig {
                tmux_bin: "/nope".into(),
                socket_name: "ciu-test".into(),
                name_prefix: "ciu-".into(),
            },
            auth_token: "t".into(),
            server_start: 1.0,
            instances_cache: Mutex::new(InstancesCache { at: f64::MAX, data: instances }),
            pending_focus: Mutex::new(PendingFocus { sid: None, ts: 0.0 }),
            ps_cache: Mutex::new(PsCache { at: 0.0, map: HashMap::new() }),
        });
        s
    }

    fn inst_with_cwd(session_id: &str, cwd: Option<&str>, command: &str) -> InstanceData {
        InstanceData {
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
            command: command.into(),
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
    async fn api_config_mcp_list_returns_globals_even_when_empty() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let s = app_state_with_instances(vec![]);
        let r = api_config_mcp_list(State(s)).await;
        let configs = r.get("configs").and_then(Value::as_array).unwrap();
        assert_eq!(configs.len(), 2);
        for c in configs {
            assert!(c.get("label").and_then(Value::as_str).unwrap().contains("Global")
                || c.get("label").and_then(Value::as_str).unwrap().contains("MCP"));
        }
    }

    #[tokio::test]
    async fn api_config_mcp_list_includes_instance_with_mcp_config_flag() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let cfg = h.join("inst-mcp.json");
        std::fs::write(&cfg, r#"{"mcpServers": {"svc": {}}}"#).unwrap();
        let inst = inst_with_cwd(
            "sess-1",
            Some("/tmp"),
            &format!("claude --mcp-config {}", cfg.to_string_lossy()),
        );
        let s = app_state_with_instances(vec![inst]);
        let r = api_config_mcp_list(State(s)).await;
        let configs = r.get("configs").and_then(Value::as_array).unwrap();
        let instance_cfg = configs
            .iter()
            .find(|c| c.get("label").and_then(Value::as_str).unwrap_or("").starts_with("Instance"));
        assert!(instance_cfg.is_some());
        let servers = instance_cfg
            .unwrap()
            .get("servers")
            .and_then(Value::as_array)
            .unwrap();
        assert!(servers.iter().any(|v| v.as_str() == Some("svc")));
        let _ = std::fs::remove_file(&cfg);
    }

    #[tokio::test]
    async fn api_config_mcp_list_skips_instance_when_cfg_file_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let inst = inst_with_cwd(
            "sess-x",
            Some("/tmp"),
            "claude --mcp-config /absolutely/not/there.json",
        );
        let s = app_state_with_instances(vec![inst]);
        let r = api_config_mcp_list(State(s)).await;
        let configs = r.get("configs").and_then(Value::as_array).unwrap();
        assert!(!configs.iter().any(|c| {
            c.get("label").and_then(Value::as_str).unwrap_or("").starts_with("Instance")
        }));
    }

    #[tokio::test]
    async fn api_config_mcp_list_dedupes_project_cwd() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let proj = h.join("projmcp");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join(".mcp.json"),
            r#"{"mcpServers": {"p1": {}}}"#,
        )
        .unwrap();
        let cwd_s = proj.to_string_lossy().to_string();
        let s = app_state_with_instances(vec![
            inst_with_cwd("a", Some(&cwd_s), "claude"),
            inst_with_cwd("b", Some(&cwd_s), "claude"),
        ]);
        let r = api_config_mcp_list(State(s)).await;
        let configs = r.get("configs").and_then(Value::as_array).unwrap();
        let proj_entries: Vec<_> = configs
            .iter()
            .filter(|c| c.get("label").and_then(Value::as_str).unwrap_or("").starts_with("Project"))
            .collect();
        assert_eq!(proj_entries.len(), 1);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[tokio::test]
    async fn api_config_skills_list_returns_only_globals_when_no_projects() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        std::fs::create_dir_all(&*paths::GLOBAL_COMMANDS_DIR).unwrap();
        std::fs::write(paths::GLOBAL_COMMANDS_DIR.join("g1.md"), "x").unwrap();
        let s = app_state_with_instances(vec![]);
        let r = api_config_skills_list(State(s)).await;
        let skills = r.get("skills").and_then(Value::as_array).unwrap();
        assert!(skills.iter().any(|sk| sk.get("name").and_then(Value::as_str) == Some("g1")));
        let _ = std::fs::remove_file(paths::GLOBAL_COMMANDS_DIR.join("g1.md"));
        let _ = std::fs::remove_dir_all(h.join(".claude"));
    }

    #[tokio::test]
    async fn api_config_skills_list_includes_project_commands() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let proj = h.join("projskills");
        let cmds = proj.join(".claude").join("commands");
        std::fs::create_dir_all(&cmds).unwrap();
        std::fs::write(cmds.join("proj.md"), "x").unwrap();
        let cwd_s = proj.to_string_lossy().to_string();
        let s = app_state_with_instances(vec![inst_with_cwd("s", Some(&cwd_s), "claude")]);
        let r = api_config_skills_list(State(s)).await;
        let skills = r.get("skills").and_then(Value::as_array).unwrap();
        let proj_skill = skills
            .iter()
            .find(|sk| sk.get("name").and_then(Value::as_str) == Some("proj"));
        assert!(proj_skill.is_some());
        assert!(proj_skill
            .unwrap()
            .get("label")
            .and_then(Value::as_str)
            .unwrap()
            .contains("Project"));
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[tokio::test]
    async fn api_config_claudemd_list_emits_global_entry() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let s = app_state_with_instances(vec![]);
        let r = api_config_claudemd_list(State(s)).await;
        let files = r.get("files").and_then(Value::as_array).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].get("scope").and_then(Value::as_str),
            Some("global")
        );
    }

    #[tokio::test]
    async fn api_config_claudemd_list_adds_project_entry() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let proj = h.join("projmd");
        std::fs::create_dir_all(&proj).unwrap();
        let cwd_s = proj.to_string_lossy().to_string();
        let s = app_state_with_instances(vec![inst_with_cwd("s", Some(&cwd_s), "claude")]);
        let r = api_config_claudemd_list(State(s)).await;
        let files = r.get("files").and_then(Value::as_array).unwrap();
        assert!(files
            .iter()
            .any(|f| f.get("scope").and_then(Value::as_str) == Some(&*cwd_s)));
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[tokio::test]
    async fn api_config_claudemd_list_adds_nested_claude_dir_entry() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let proj = h.join("projnested");
        let nested = proj.join(".claude");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("CLAUDE.md"), "x").unwrap();
        let cwd_s = proj.to_string_lossy().to_string();
        let s = app_state_with_instances(vec![inst_with_cwd("s", Some(&cwd_s), "claude")]);
        let r = api_config_claudemd_list(State(s)).await;
        let files = r.get("files").and_then(Value::as_array).unwrap();
        let nested_entry = files
            .iter()
            .find(|f| f.get("label").and_then(Value::as_str).unwrap_or("").contains(".claude/"));
        assert!(nested_entry.is_some());
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[tokio::test]
    async fn api_config_mcp_list_dedupes_skips_duplicate_cwd() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let proj = h.join("mcpdup");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(".mcp.json"), r#"{"mcpServers": {}}"#).unwrap();
        let cwd_s = proj.to_string_lossy().to_string();
        let s = app_state_with_instances(vec![
            inst_with_cwd("a", Some(&cwd_s), "claude"),
            inst_with_cwd("b", Some(&cwd_s), "claude"),
            inst_with_cwd("c", None, "claude"),
        ]);
        let r = api_config_mcp_list(State(s)).await;
        let configs = r.get("configs").and_then(Value::as_array).unwrap();
        let proj_entries: Vec<_> = configs
            .iter()
            .filter(|c| c.get("label").and_then(Value::as_str).unwrap_or("").starts_with("Project"))
            .collect();
        assert_eq!(proj_entries.len(), 1);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[tokio::test]
    async fn api_config_skills_list_dedupes_duplicate_cwd_and_skips_none_cwd() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let proj = h.join("skillsdup");
        let cmds = proj.join(".claude").join("commands");
        std::fs::create_dir_all(&cmds).unwrap();
        std::fs::write(cmds.join("k.md"), "x").unwrap();
        let cwd_s = proj.to_string_lossy().to_string();
        let s = app_state_with_instances(vec![
            inst_with_cwd("a", Some(&cwd_s), "claude"),
            inst_with_cwd("b", Some(&cwd_s), "claude"),
            inst_with_cwd("c", None, "claude"),
        ]);
        let r = api_config_skills_list(State(s)).await;
        let skills = r.get("skills").and_then(Value::as_array).unwrap();
        let k_entries: Vec<_> = skills
            .iter()
            .filter(|sk| sk.get("name").and_then(Value::as_str) == Some("k"))
            .collect();
        assert_eq!(k_entries.len(), 1);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[tokio::test]
    async fn api_config_claudemd_list_dedupes_duplicate_cwd_and_skips_none() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let proj = h.join("mddup");
        std::fs::create_dir_all(&proj).unwrap();
        let cwd_s = proj.to_string_lossy().to_string();
        let s = app_state_with_instances(vec![
            inst_with_cwd("a", Some(&cwd_s), "claude"),
            inst_with_cwd("b", Some(&cwd_s), "claude"),
            inst_with_cwd("c", None, "claude"),
        ]);
        let r = api_config_claudemd_list(State(s)).await;
        let files = r.get("files").and_then(Value::as_array).unwrap();
        let proj_entries: Vec<_> = files
            .iter()
            .filter(|f| f.get("scope").and_then(Value::as_str) == Some(&*cwd_s))
            .collect();
        assert_eq!(proj_entries.len(), 1);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[tokio::test]
    async fn api_config_claudemd_read_returns_content_when_file_exists() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("CLAUDE_read.md");
        std::fs::write(&p, "hello-md").unwrap();
        let body = ConfigFileBody { path: p.to_string_lossy().to_string() };
        let r = api_config_claudemd_read(Json(body)).await;
        assert_eq!(r.get("exists").and_then(Value::as_bool), Some(true));
        assert_eq!(r.get("content").and_then(Value::as_str), Some("hello-md"));
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn api_mcp_sources_counts_servers_when_file_present() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        std::fs::create_dir_all(&*paths::CLAUDE_DIR).unwrap();
        let global_json = h.join(".claude.json");
        std::fs::write(&global_json, r#"{"mcpServers": {"a": {}, "b": {}}}"#).unwrap();
        let r = api_mcp_sources().await;
        let sources = r.get("sources").and_then(Value::as_array).unwrap();
        let with_count = sources
            .iter()
            .find(|s| s.get("exists").and_then(Value::as_bool) == Some(true))
            .expect("at least one source should exist");
        assert_eq!(with_count.get("count").and_then(Value::as_u64), Some(2));
        let _ = std::fs::remove_file(&global_json);
    }
}
