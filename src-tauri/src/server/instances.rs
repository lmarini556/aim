use crate::paths;
use crate::server::transcript::{
    jsonl_summary, jsonl_tail, session_title,
};
use crate::server::types::*;
use crate::tmux::{self, TmuxConfig};
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub struct AppState {
    pub tmux_config: TmuxConfig,
    pub auth_token: String,
    pub server_start: f64,
    pub instances_cache: Mutex<InstancesCache>,
    pub pending_focus: Mutex<PendingFocus>,
    pub ps_cache: Mutex<PsCache>,
}

pub struct InstancesCache {
    pub at: f64,
    pub data: Vec<InstanceData>,
}

pub struct PendingFocus {
    pub sid: Option<String>,
    pub ts: f64,
}

pub struct PsCache {
    pub at: f64,
    pub map: HashMap<u32, PsRow>,
}

#[derive(Clone)]
pub struct PsRow {
    pub ppid: u32,
    pub tty: String,
    pub command: String,
}

const INSTANCES_TTL: f64 = 1.5;
const PS_TTL: f64 = 1.5;
const PENDING_FOCUS_TTL: f64 = 10.0;
const STOP_FRESH: f64 = 10.0;
const NOTIFICATION_TTL: f64 = 1800.0;
const RUNNING_STALENESS: f64 = 30.0;
const APPROVAL_KEYWORDS: &[&str] = &["permission", "approval", "approve", "confirm", "allow"];

impl AppState {
    pub fn cached_instances(&self) -> Vec<InstanceData> {
        let n = now();
        let mut cache = self.instances_cache.lock().unwrap();
        if n - cache.at < INSTANCES_TTL {
            return cache.data.clone();
        }
        let data = gather_instances(&self.tmux_config, &self.ps_cache);
        cache.at = n;
        cache.data = data.clone();
        data
    }

    pub fn invalidate_cache(&self) {
        self.instances_cache.lock().unwrap().at = 0.0;
    }
}

pub fn read_json(path: &std::path::Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({}))
}

pub fn write_json(path: &std::path::Path, data: &Value) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, serde_json::to_string_pretty(data).unwrap_or_default());
}

fn pid_alive(pid: u32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
}

fn refresh_ps_cache(cache: &Mutex<PsCache>) -> HashMap<u32, PsRow> {
    let n = now();
    {
        let c = cache.lock().unwrap();
        if n - c.at < PS_TTL {
            return c.map.clone();
        }
    }
    let mut map = HashMap::new();
    if let Ok(out) = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,ppid=,tty=,command="])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let parts: Vec<&str> = line.splitn(4, char::is_whitespace).collect();
            if parts.len() < 3 {
                continue;
            }
            let pid: u32 = match parts[0].trim().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let ppid: u32 = parts[1].trim().parse().unwrap_or(0);
            let tty = parts.get(2).unwrap_or(&"").trim().to_string();
            let command = parts.get(3).unwrap_or(&"").to_string();
            map.insert(pid, PsRow { ppid, tty, command });
        }
    }
    let mut c = cache.lock().unwrap();
    c.at = n;
    c.map = map.clone();
    map
}

fn parse_mcp_arg(command: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let parts: Vec<&str> = command.split_whitespace().collect();
    let mut i = 0;
    while i < parts.len() {
        let t = parts[i];
        if t == "--mcp-config" && i + 1 < parts.len() {
            paths.push(parts[i + 1].to_string());
            i += 2;
            continue;
        }
        if let Some(val) = t.strip_prefix("--mcp-config=") {
            paths.push(val.to_string());
        }
        i += 1;
    }
    paths
}

fn mcp_servers_at(path: &std::path::Path) -> HashMap<String, Value> {
    if !path.exists() {
        return HashMap::new();
    }
    let data = read_json(path);
    match data.get("mcpServers") {
        Some(Value::Object(m)) => m
            .iter()
            .filter(|(_, v)| v.is_object())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        _ => HashMap::new(),
    }
}

fn available_mcp_servers() -> HashMap<String, Value> {
    let mut servers = HashMap::new();
    for path in [
        paths::HOME.join(".claude.json"),
        paths::GLOBAL_MCP.clone(),
    ] {
        for (name, spec) in mcp_servers_at(&path) {
            servers.entry(name).or_insert(spec);
        }
    }
    servers
}

fn load_mcps(cwd: Option<&str>, command: &str) -> Value {
    let global_defs = available_mcp_servers();
    let mut global: Vec<String> = global_defs.keys().cloned().collect();
    global.sort();

    let mut project = Vec::new();
    if let Some(cwd) = cwd {
        let proj = PathBuf::from(cwd).join(".mcp.json");
        if proj.exists() {
            let data = read_json(&proj);
            if let Some(Value::Object(m)) = data.get("mcpServers") {
                let mut names: Vec<String> = m.keys().cloned().collect();
                names.sort();
                project = names;
            }
        }
    }

    let mut explicit = Vec::new();
    for p in parse_mcp_arg(command) {
        let expanded = PathBuf::from(p.replace('~', &paths::HOME.to_string_lossy()));
        if expanded.exists() {
            let data = read_json(&expanded);
            if let Some(Value::Object(m)) = data.get("mcpServers") {
                explicit.extend(m.keys().cloned());
            }
        }
    }
    explicit.sort();
    explicit.dedup();

    json!({ "global": global, "project": project, "explicit": explicit })
}

fn display_name(
    session_id: &str,
    cwd: Option<&str>,
    jsonl_title: Option<&str>,
    names: &Value,
) -> String {
    if let Some(Value::String(n)) = names.get(session_id) {
        if !n.is_empty() {
            return n.clone();
        }
    }
    if let Some(t) = jsonl_title {
        return t.to_string();
    }
    let base = cwd
        .map(|c| {
            std::path::Path::new(c)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    format!("{base} · {}", &session_id[..session_id.len().min(8)])
}

fn is_approval_message(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    if lower.contains("waiting for your input") {
        return false;
    }
    APPROVAL_KEYWORDS.iter().any(|k| lower.contains(k))
}

fn resolve_status(hook_state: &Value, alive: bool, jsonl: &Value) -> (String, Option<String>) {
    if !alive {
        return ("ended".into(), None);
    }
    let n = now();
    let hook_ts = hook_state
        .get("timestamp")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let hook_age = if hook_ts > 0.0 { n - hook_ts } else { f64::INFINITY };
    let hook_event = hook_state
        .get("last_event")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let hook_msg = hook_state
        .get("notification_message")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let jsonl_epoch = jsonl
        .get("last_timestamp_epoch")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let jsonl_age = if jsonl_epoch > 0.0 {
        n - jsonl_epoch
    } else {
        f64::INFINITY
    };
    let pending = jsonl.get("pending").and_then(|v| v.as_bool()).unwrap_or(false);
    let pending_tool = jsonl
        .get("pending_tool")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let last_type = jsonl
        .get("last_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let freshest = hook_age.min(jsonl_age);

    if hook_event == "Notification" && hook_age < NOTIFICATION_TTL && is_approval_message(hook_msg)
    {
        let label = if !hook_msg.is_empty() {
            hook_msg.to_string()
        } else if !pending_tool.is_empty() {
            format!("Awaiting approval: {pending_tool}")
        } else {
            "Needs input".to_string()
        };
        return ("needs_input".into(), Some(label));
    }

    if hook_event == "Notification" {
        return ("idle".into(), None);
    }

    if hook_event == "Stop" && hook_age < STOP_FRESH {
        return ("idle".into(), None);
    }

    if !hook_event.is_empty() && hook_event != "Stop" && freshest < RUNNING_STALENESS {
        return ("running".into(), None);
    }

    if last_type == "user" && jsonl_age < RUNNING_STALENESS {
        return ("running".into(), None);
    }
    if pending && jsonl_age < RUNNING_STALENESS {
        return ("running".into(), None);
    }

    ("idle".into(), None)
}

fn is_headless(command: &str) -> bool {
    command.contains(" -p ") || command.contains(" --print ") || command.ends_with(" -p") || command.ends_with(" --print")
}

fn subagent_info(session_id: &str, cwd: Option<&str>) -> Vec<Value> {
    let cwd = match cwd {
        Some(c) => c,
        None => return vec![],
    };
    let slug = format!("-{}", cwd.replace('/', "-").trim_start_matches('-'));
    let sub_dir = paths::PROJECTS_DIR.join(&slug).join(session_id).join("subagents");
    if !sub_dir.exists() {
        return vec![];
    }
    let mut entries: Vec<(PathBuf, f64)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&sub_dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().map(|e| e == "jsonl").unwrap_or(false)
                && p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("agent-"))
                    .unwrap_or(false)
            {
                let mtime = p
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                entries.push((p, mtime));
            }
        }
    }
    entries.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    entries
        .into_iter()
        .map(|(p, mtime)| {
            let agent_id = p
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .replace("agent-", "");
            let label = read_first_user_text(&p).unwrap_or_else(|| agent_id[..agent_id.len().min(12)].to_string());
            json!({ "agent_id": agent_id, "label": label, "mtime": mtime })
        })
        .collect()
}

fn read_first_user_text(path: &std::path::Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    use std::io::BufRead;
    for line in reader.lines().take(200) {
        let line = line.ok()?;
        let d: Value = serde_json::from_str(&line).ok()?;
        if d.get("type").and_then(|v| v.as_str()) != Some("user") {
            continue;
        }
        let content = d.pointer("/message/content")?;
        if let Some(s) = content.as_str() {
            let s = s.trim().replace('\n', " ");
            if !s.is_empty() {
                return Some(s[..s.len().min(100)].to_string());
            }
        }
        if let Some(arr) = content.as_array() {
            for item in arr {
                if item.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                        let t = t.trim().replace('\n', " ");
                        if !t.is_empty() {
                            return Some(t[..t.len().min(100)].to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

fn stash_pending_name(our_sid: &str, name: &str) {
    let mut data = read_json(&paths::PENDING_NAMES_FILE);
    if let Value::Object(ref mut m) = data {
        m.insert(our_sid.to_string(), Value::String(name.to_string()));
    }
    write_json(&paths::PENDING_NAMES_FILE, &data);
}

fn stash_pending_group(our_sid: &str, group: &str) {
    let mut data = read_json(&paths::PENDING_GROUPS_FILE);
    if let Value::Object(ref mut m) = data {
        m.insert(our_sid.to_string(), Value::String(group.to_string()));
    }
    write_json(&paths::PENDING_GROUPS_FILE, &data);
}

fn promote_pending_names(instances: &mut [InstanceData]) {
    let mut pending = read_json(&paths::PENDING_NAMES_FILE);
    let mut names = read_json(&paths::NAMES_FILE);
    let mut names_changed = false;
    let mut pending_changed = false;

    if let Value::Object(ref mut pm) = pending {
        if let Value::Object(ref mut nm) = names {
            for inst in instances.iter() {
                if let (Some(osid), sid) = (&inst.our_sid, &inst.session_id) {
                    if let Some(Value::String(pname)) = pm.remove(osid.as_str()) {
                        if !nm.contains_key(sid.as_str()) {
                            nm.insert(sid.to_string(), Value::String(pname));
                            names_changed = true;
                            pending_changed = true;
                        }
                    }
                }
            }
        }
    }
    if names_changed {
        write_json(&paths::NAMES_FILE, &names);
    }
    if pending_changed {
        write_json(&paths::PENDING_NAMES_FILE, &pending);
    }

    let mut pg = read_json(&paths::PENDING_GROUPS_FILE);
    let mut groups = read_json(&paths::GROUPS_FILE);
    let mut g_changed = false;
    let mut pg_changed = false;

    if let Value::Object(ref mut pgm) = pg {
        for inst in instances.iter() {
            if let (Some(osid), sid) = (&inst.our_sid, &inst.session_id) {
                if let Some(Value::String(grp)) = pgm.remove(osid.as_str()) {
                    pg_changed = true;
                    let arr = groups
                        .as_object_mut()
                        .unwrap()
                        .entry(&grp)
                        .or_insert_with(|| json!([]));
                    if let Some(a) = arr.as_array_mut() {
                        if !a.iter().any(|v| v.as_str() == Some(sid.as_str())) {
                            a.push(Value::String(sid.to_string()));
                            g_changed = true;
                        }
                    }
                }
            }
        }
    }
    if g_changed {
        write_json(&paths::GROUPS_FILE, &groups);
    }
    if pg_changed {
        write_json(&paths::PENDING_GROUPS_FILE, &pg);
    }

    let fresh_names = read_json(&paths::NAMES_FILE);
    for inst in instances.iter_mut() {
        if let Some(Value::String(n)) = fresh_names.get(&inst.session_id) {
            if !n.is_empty() {
                inst.custom_name = Some(n.clone());
                inst.name = n.clone();
            }
        }
    }
}

fn gather_instances(tmux_config: &TmuxConfig, ps_cache: &Mutex<PsCache>) -> Vec<InstanceData> {
    let names = read_json(&paths::NAMES_FILE);
    let groups = read_json(&paths::GROUPS_FILE);
    let acks = read_json(&paths::ACKS_FILE);
    let tmux_sessions = tmux::session::list_sessions(tmux_config).unwrap_or_default();
    let tmux_by_sid: HashMap<String, String> = tmux_sessions
        .iter()
        .map(|s| (s.our_sid.clone(), s.name.clone()))
        .collect();

    let ps_map = refresh_ps_cache(ps_cache);
    let mut by_session: HashMap<String, InstanceData> = HashMap::new();
    let n = now();

    if paths::SESSIONS_DIR.exists() {
        if let Ok(rd) = std::fs::read_dir(&*paths::SESSIONS_DIR) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().map(|e| e == "json").unwrap_or(false) {
                    let data = read_json(&p);
                    let pid = data.get("pid").and_then(|v| v.as_u64()).map(|v| v as u32);
                    let sid = data.get("sessionId").and_then(|v| v.as_str());
                    let (pid, sid) = match (pid, sid) {
                        (Some(p), Some(s)) => (p, s.to_string()),
                        _ => continue,
                    };
                    let alive = pid_alive(pid);
                    let command = if alive {
                        ps_map
                            .get(&pid)
                            .map(|r| r.command.clone())
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    if alive && is_headless(&command) {
                        continue;
                    }
                    let hook_state = read_json(&paths::STATE_DIR.join(format!("{sid}.json")));
                    let our_sid = match hook_state
                        .get("our_sid")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                    {
                        Some(s) => s,
                        None => continue,
                    };
                    let tmux_name = tmux_by_sid.get(&our_sid).cloned();
                    if tmux_name.is_none() {
                        continue;
                    }
                    let cwd = data.get("cwd").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let cwd_ref = cwd.as_deref();
                    let (jsonl_title, first_user) = session_title(&sid, cwd_ref);
                    let tail = jsonl_tail(&sid, cwd_ref);
                    let summary = jsonl_summary(&sid, cwd_ref);
                    let mcps = load_mcps(cwd_ref, &command);
                    let (status, notif) = resolve_status(&hook_state, alive, &tail);
                    let subagents = if alive { subagent_info(&sid, cwd_ref) } else { vec![] };

                    let mut status = status;
                    if status == "idle" && alive && !subagents.is_empty() {
                        let newest_sub = subagents
                            .iter()
                            .filter_map(|a| a.get("mtime").and_then(|v| v.as_f64()))
                            .fold(0.0f64, f64::max);
                        if n - newest_sub < RUNNING_STALENESS {
                            status = "running".to_string();
                        }
                    }

                    let group = groups
                        .as_object()
                        .and_then(|m| {
                            m.iter().find_map(|(g, ids)| {
                                ids.as_array().and_then(|a| {
                                    if a.iter().any(|v| v.as_str() == Some(&sid)) {
                                        Some(g.clone())
                                    } else {
                                        None
                                    }
                                })
                            })
                        });

                    let inst = InstanceData {
                        session_id: sid.clone(),
                        pid: Some(pid),
                        alive,
                        name: display_name(&sid, cwd_ref, jsonl_title.as_deref(), &names),
                        title: jsonl_title,
                        custom_name: names.get(&sid).and_then(|v| v.as_str()).map(|s| s.to_string()),
                        first_user,
                        cwd,
                        kind: data.get("kind").and_then(|v| v.as_str()).map(|s| s.to_string()),
                        started_at: data.get("startedAt").and_then(|v| v.as_f64()),
                        command,
                        status,
                        last_event: hook_state.get("last_event").and_then(|v| v.as_str()).map(|s| s.to_string()),
                        last_tool: hook_state.get("last_tool").and_then(|v| v.as_str()).map(|s| s.to_string()),
                        notification_message: notif,
                        hook_timestamp: hook_state.get("timestamp").and_then(|v| v.as_f64()),
                        transcript: tail,
                        summary,
                        mcps,
                        subagents,
                        group,
                        ack_timestamp: acks.get(&sid).and_then(|v| v.as_f64()).unwrap_or(0.0),
                        our_sid: Some(our_sid),
                        tmux_session: tmux_name,
                    };
                    by_session.insert(sid, inst);
                }
            }
        }
    }

    let mut orphan_count = 0u32;
    if let Ok(rd) = std::fs::read_dir(&*paths::STATE_DIR) {
        for entry in rd.flatten() {
            if orphan_count >= 200 {
                break;
            }
            let p = entry.path();
            let sid = match p.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if by_session.contains_key(&sid) {
                continue;
            }
            let data = read_json(&p);
            let ts = data.get("timestamp").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if n - ts > 86400.0 {
                continue;
            }
            let tp = data.get("transcript_path").and_then(|v| v.as_str()).unwrap_or("");
            if tp.contains("/subagents/") {
                continue;
            }
            let our_sid = match data
                .get("our_sid")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
            {
                Some(s) => s,
                None => continue,
            };
            orphan_count += 1;
            let cwd = data.get("cwd").and_then(|v| v.as_str()).map(|s| s.to_string());
            let cwd_ref = cwd.as_deref();
            let (jsonl_title, first_user) = session_title(&sid, cwd_ref);
            let tmux_name = tmux_by_sid.get(&our_sid).cloned();

            let group = groups.as_object().and_then(|m| {
                m.iter().find_map(|(g, ids)| {
                    ids.as_array().and_then(|a| {
                        if a.iter().any(|v| v.as_str() == Some(&sid)) {
                            Some(g.clone())
                        } else {
                            None
                        }
                    })
                })
            });

            let inst = InstanceData {
                session_id: sid.clone(),
                pid: None,
                alive: false,
                name: display_name(&sid, cwd_ref, jsonl_title.as_deref(), &names),
                title: jsonl_title,
                custom_name: names.get(&sid).and_then(|v| v.as_str()).map(|s| s.to_string()),
                first_user,
                cwd: cwd.clone(),
                kind: None,
                started_at: None,
                command: String::new(),
                status: "ended".to_string(),
                last_event: data.get("last_event").and_then(|v| v.as_str()).map(|s| s.to_string()),
                last_tool: None,
                notification_message: None,
                hook_timestamp: data.get("timestamp").and_then(|v| v.as_f64()),
                transcript: json!({}),
                summary: jsonl_summary(&sid, cwd_ref),
                mcps: load_mcps(cwd_ref, ""),
                subagents: vec![],
                group,
                ack_timestamp: acks.get(&sid).and_then(|v| v.as_f64()).unwrap_or(0.0),
                our_sid: Some(our_sid),
                tmux_session: tmux_name,
            };
            by_session.insert(sid, inst);
        }
    }

    let mut items: Vec<InstanceData> = by_session.into_values().collect();
    items.sort_by(|a, b| {
        let a_alive = a.alive;
        let b_alive = b.alive;
        let a_ts = a.hook_timestamp.unwrap_or(0.0).max(
            a.transcript
                .get("last_timestamp_epoch")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
        );
        let b_ts = b.hook_timestamp.unwrap_or(0.0).max(
            b.transcript
                .get("last_timestamp_epoch")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
        );
        (!b_alive)
            .cmp(&(!a_alive))
            .then(b_ts.partial_cmp(&a_ts).unwrap_or(std::cmp::Ordering::Equal))
    });

    promote_pending_names(&mut items);
    items
}

// ---- Axum handlers ----

pub async fn api_instances(
    State(state): State<Arc<AppState>>,
) -> Json<Value> {
    let n = now();
    let instances = state.cached_instances();
    let pf = state.pending_focus.lock().unwrap();
    let focus_sid = if pf.sid.is_some() && n - pf.ts < PENDING_FOCUS_TTL {
        pf.sid.clone()
    } else {
        None
    };
    Json(json!({
        "instances": instances,
        "served_at": n,
        "server_start": state.server_start,
        "pending_focus": focus_sid,
    }))
}

pub async fn api_clear_pending_focus(
    State(state): State<Arc<AppState>>,
) -> Json<OkResponse> {
    state.pending_focus.lock().unwrap().sid = None;
    Json(OkResponse::new())
}

pub async fn api_groups() -> Json<Value> {
    Json(read_json(&paths::GROUPS_FILE))
}

pub async fn api_set_groups(Json(data): Json<Value>) -> Json<OkResponse> {
    write_json(&paths::GROUPS_FILE, &data);
    Json(OkResponse::new())
}

pub async fn api_rename(
    Path(session_id): Path<String>,
    Json(body): Json<RenameBody>,
) -> Json<OkResponse> {
    let mut names = read_json(&paths::NAMES_FILE);
    if let Value::Object(ref mut m) = names {
        let trimmed = body.name.trim();
        if trimmed.is_empty() {
            m.remove(&session_id);
        } else {
            m.insert(session_id, Value::String(trimmed.to_string()));
        }
    }
    write_json(&paths::NAMES_FILE, &names);
    Json(OkResponse::new())
}

pub async fn api_set_group(
    Path(session_id): Path<String>,
    Json(body): Json<GroupBody>,
) -> Json<OkResponse> {
    let mut groups = read_json(&paths::GROUPS_FILE);
    if let Value::Object(ref mut m) = groups {
        for (_, ids) in m.iter_mut() {
            if let Value::Array(ref mut a) = ids {
                a.retain(|v| v.as_str() != Some(&session_id));
            }
        }
        let empty_keys: Vec<String> = m
            .iter()
            .filter(|(_, v)| v.as_array().map(|a| a.is_empty()).unwrap_or(true))
            .map(|(k, _)| k.clone())
            .collect();
        for k in empty_keys {
            m.remove(&k);
        }
        if let Some(g) = body.group {
            m.entry(&g)
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .map(|a| a.push(Value::String(session_id)));
        }
    }
    write_json(&paths::GROUPS_FILE, &groups);
    Json(OkResponse::new())
}

pub async fn api_signal(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<SignalBody>,
) -> std::result::Result<Json<OkResponse>, (StatusCode, Json<Value>)> {
    let instances = state.cached_instances();
    let target = instances
        .iter()
        .find(|i| i.session_id == session_id)
        .ok_or((StatusCode::NOT_FOUND, Json(json!({"detail": "session not found"}))))?;
    let our_sid = target
        .our_sid
        .as_ref()
        .ok_or((StatusCode::BAD_REQUEST, Json(json!({"detail": "not a managed session"}))))?;
    let sig = body.signal.as_deref().unwrap_or("INT");
    let config = state.tmux_config.clone();
    let sid = our_sid.clone();
    let sig = sig.to_string();
    tokio::task::spawn_blocking(move || {
        let _ = tmux::session::send_signal(&config, &sid, &sig);
    })
    .await
    .ok();
    Ok(Json(OkResponse::new()))
}

pub async fn api_forget(Path(session_id): Path<String>) -> Json<OkResponse> {
    let sf = paths::STATE_DIR.join(format!("{session_id}.json"));
    let _ = std::fs::remove_file(sf);
    Json(OkResponse::new())
}

pub async fn api_new_instance(
    State(state): State<Arc<AppState>>,
    Json(body): Json<NewInstanceBody>,
) -> std::result::Result<Json<Value>, (StatusCode, Json<Value>)> {
    let cwd_path = PathBuf::from(
        body.cwd
            .replace('~', &paths::HOME.to_string_lossy()),
    );
    if !cwd_path.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"detail": format!("not a directory: {}", body.cwd)})),
        ));
    }
    let _ = std::fs::create_dir_all(&*paths::UPLOAD_DIR);
    let base_cmd = body.command.as_deref().unwrap_or("claude");
    let mut command = format!("{base_cmd} --add-dir {}", shell_quote(&paths::UPLOAD_DIR.to_string_lossy()));

    if let Some(ref mcps) = body.mcps {
        let mut source_defs: HashMap<String, Value> = HashMap::new();
        if let Some(ref src) = body.mcp_source {
            let p = PathBuf::from(src.replace('~', &paths::HOME.to_string_lossy()));
            source_defs = mcp_servers_at(&p);
        }
        let all_defs = if source_defs.is_empty() {
            available_mcp_servers()
        } else {
            source_defs
        };
        let subset: HashMap<String, Value> = mcps
            .iter()
            .filter_map(|n| all_defs.get(n).map(|v| (n.clone(), v.clone())))
            .collect();
        let _ = std::fs::create_dir_all(&*paths::MCP_CONFIG_DIR);
        let cfg_token = hex::encode(rand::random::<[u8; 6]>());
        let cfg_path = paths::MCP_CONFIG_DIR.join(format!("{cfg_token}.json"));
        write_json(&cfg_path, &json!({"mcpServers": subset}));
        let strict = if base_cmd.contains("--resume") {
            ""
        } else {
            " --strict-mcp-config"
        };
        command = format!(
            "{command} --mcp-config {}{}",
            shell_quote(&cfg_path.to_string_lossy()),
            strict
        );
    }

    let config = state.tmux_config.clone();
    let cwd_str = cwd_path.to_string_lossy().to_string();
    let cmd = command.clone();
    let our_sid = tokio::task::spawn_blocking(move || {
        tmux::session::spawn(&config, &cwd_str, &[cmd], vec![])
    })
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"detail": "spawn failed"}))))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"detail": e.to_string()}))))?;

    if let Some(ref name) = body.name {
        let n = name.trim();
        if !n.is_empty() {
            stash_pending_name(&our_sid, n);
        }
    }
    if let Some(ref group) = body.group {
        let g = group.trim();
        if !g.is_empty() {
            stash_pending_group(&our_sid, g);
        }
    }
    state.invalidate_cache();
    Ok(Json(json!({"ok": true, "our_sid": our_sid})))
}

pub async fn api_input(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<InputBody>,
) -> std::result::Result<Json<OkResponse>, (StatusCode, Json<Value>)> {
    let instances = state.cached_instances();
    let target = instances
        .iter()
        .find(|i| i.session_id == session_id)
        .ok_or((StatusCode::NOT_FOUND, Json(json!({"detail": "session not found"}))))?;
    let our_sid = target
        .our_sid
        .clone()
        .ok_or((StatusCode::BAD_REQUEST, Json(json!({"detail": "not a tmux-owned session"}))))?;
    let config = state.tmux_config.clone();
    let text = body.text.clone();
    let submit = body.submit.unwrap_or(true);
    tokio::task::spawn_blocking(move || {
        if !text.is_empty() {
            let _ = tmux::session::send_bytes(&config, &our_sid, text.as_bytes());
        }
        if submit {
            let settle = if text.len() > 40 { 300 } else { 100 };
            std::thread::sleep(std::time::Duration::from_millis(settle));
            let _ = tmux::session::send_enter(&config, &our_sid);
        }
    })
    .await
    .ok();
    Ok(Json(OkResponse::new()))
}

pub async fn api_upload(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    mut multipart: Multipart,
) -> std::result::Result<Json<Value>, (StatusCode, Json<Value>)> {
    let instances = state.cached_instances();
    let _target = instances
        .iter()
        .find(|i| i.session_id == session_id)
        .ok_or((StatusCode::NOT_FOUND, Json(json!({"detail": "session not found"}))))?;

    let _ = std::fs::create_dir_all(&*paths::UPLOAD_DIR);
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, Json(json!({"detail": "bad upload"}))))?
    {
        let raw_name = field.file_name().unwrap_or("upload.bin").to_string();
        let safe: String = raw_name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
            .take(80)
            .collect();
        let safe = if safe.is_empty() { "upload.bin".to_string() } else { safe };
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let tok = hex::encode(rand::random::<[u8; 3]>());
        let path = paths::UPLOAD_DIR.join(format!("{stamp}-{tok}-{safe}"));
        let data = field
            .bytes()
            .await
            .map_err(|_| (StatusCode::BAD_REQUEST, Json(json!({"detail": "read error"}))))?;
        if data.len() > 25 * 1024 * 1024 {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({"detail": "file too large (25 MB max)"})),
            ));
        }
        std::fs::write(&path, &data)
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"detail": "write failed"}))))?;
        return Ok(Json(json!({
            "ok": true,
            "path": path.to_string_lossy(),
            "name": raw_name,
            "size": data.len(),
        })));
    }
    Err((StatusCode::BAD_REQUEST, Json(json!({"detail": "no file"}))))
}

pub async fn api_kill(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> std::result::Result<Json<OkResponse>, (StatusCode, Json<Value>)> {
    let instances = state.cached_instances();
    let target = instances
        .iter()
        .find(|i| i.session_id == session_id)
        .ok_or((StatusCode::NOT_FOUND, Json(json!({"detail": "session not found"}))))?;
    let our_sid = target
        .our_sid
        .clone()
        .ok_or((StatusCode::BAD_REQUEST, Json(json!({"detail": "not tmux-owned"}))))?;
    let config = state.tmux_config.clone();
    tokio::task::spawn_blocking(move || {
        let _ = tmux::session::kill_session(&config, &our_sid);
    })
    .await
    .ok();
    state.invalidate_cache();
    Ok(Json(OkResponse::new()))
}

pub async fn api_open_terminal(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> std::result::Result<Json<OkResponse>, (StatusCode, Json<Value>)> {
    let instances = state.cached_instances();
    let target = instances
        .iter()
        .find(|i| i.session_id == session_id)
        .ok_or((StatusCode::NOT_FOUND, Json(json!({"detail": "session not found"}))))?;
    let our_sid = target
        .our_sid
        .clone()
        .ok_or((StatusCode::BAD_REQUEST, Json(json!({"detail": "not tmux-owned"}))))?;

    let config = state.tmux_config.clone();
    if !tmux::session::session_exists(&config, &our_sid) {
        return Err((StatusCode::NOT_FOUND, Json(json!({"detail": "tmux session gone"}))));
    }

    let session_name = format!("{}{}", config.name_prefix, our_sid);
    let tmux_bin = config.tmux_bin.to_string_lossy().to_string();
    let attach_cmd = format!("{tmux_bin} -L {} attach-session -t {session_name}", config.socket_name);

    let script = if std::path::Path::new("/Applications/iTerm.app").is_dir() {
        format!(
            "tell application \"iTerm\"\ncreate window with default profile command \"{attach_cmd}\"\nend tell"
        )
    } else {
        format!(
            "tell application \"Terminal\"\ndo script \"{attach_cmd}\"\nend tell"
        )
    };
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    Ok(Json(OkResponse::new()))
}

pub async fn api_ack(
    Path(session_id): Path<String>,
    Json(body): Json<AckBody>,
) -> Json<Value> {
    let mut acks = read_json(&paths::ACKS_FILE);
    let cur = acks.get(&session_id).and_then(|v| v.as_f64()).unwrap_or(0.0);
    if body.timestamp > cur {
        if let Value::Object(ref mut m) = acks {
            m.insert(session_id.clone(), json!(body.timestamp));
        }
        write_json(&paths::ACKS_FILE, &acks);
    }
    let final_ts = acks
        .get(&session_id)
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    Json(json!({"ok": true, "ack_timestamp": final_ts}))
}

pub async fn api_recent_cwds(
    State(state): State<Arc<AppState>>,
) -> Json<Value> {
    let mut seen: HashMap<String, f64> = HashMap::new();
    for inst in state.cached_instances() {
        if let Some(ref cwd) = inst.cwd {
            let ts = inst.hook_timestamp.unwrap_or(0.0);
            let e = seen.entry(cwd.clone()).or_insert(0.0);
            *e = e.max(ts);
        }
    }
    if paths::PROJECTS_DIR.exists() {
        if let Ok(rd) = std::fs::read_dir(&*paths::PROJECTS_DIR) {
            for entry in rd.flatten() {
                let p = entry.path();
                if !p.is_dir() {
                    continue;
                }
                let slug = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                if !slug.starts_with('-') {
                    continue;
                }
                let cwd = format!("/{}", slug[1..].replace('-', "/"));
                if !PathBuf::from(&cwd).is_dir() {
                    continue;
                }
                let mtime = p
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                let e = seen.entry(cwd).or_insert(0.0);
                *e = e.max(mtime);
            }
        }
    }
    let mut rows: Vec<(String, f64)> = seen.into_iter().collect();
    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let cwds: Vec<String> = rows.into_iter().take(50).map(|(c, _)| c).collect();
    Json(json!({"cwds": cwds}))
}

pub async fn api_open_dashboard(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenDashboardBody>,
) -> Json<Value> {
    if let Some(ref sid) = body.sid {
        let mut pf = state.pending_focus.lock().unwrap();
        pf.sid = Some(sid.clone());
        pf.ts = now();
    }

    let host = std::env::var("CIU_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("CIU_PORT").unwrap_or_else(|_| "7878".to_string());
    let host_filter = format!("{host}:{port}");

    if let Some(app) = try_focus_existing_window(&host_filter) {
        let _ = std::process::Command::new("open").arg("-a").arg(&app).spawn();
        return Json(json!({"ok": true, "result": "focused"}));
    }

    let public_url = std::env::var("CIU_PUBLIC_URL")
        .unwrap_or_else(|_| format!("http://{host}:{port}"));
    let mut qs = format!("t={}", state.auth_token);
    if let Some(ref sid) = body.sid {
        qs.push_str(&format!("&sid={sid}"));
    }
    let open_url = format!("{public_url}/?{qs}");
    let _ = std::process::Command::new("open").arg(&open_url).spawn();
    Json(json!({"ok": true, "result": "opened"}))
}

fn try_focus_existing_window(host_filter: &str) -> Option<String> {
    for app in ["Arc", "Google Chrome", "Brave Browser", "Microsoft Edge", "Vivaldi"] {
        let script = format!(
            r#"if application "{app}" is running then
  using terms from application "Google Chrome"
    tell application "{app}"
      repeat with w in windows
        set i to 0
        repeat with t in tabs of w
          set i to i + 1
          try
            if (URL of t as string) contains "{host_filter}" then
              set active tab index of w to i
              set index of w to 1
              activate
              return "focused"
            end if
          end try
        end repeat
      end repeat
    end tell
  end using terms from
end if
return "nf""#
        );
        if let Ok(out) = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
        {
            if String::from_utf8_lossy(&out.stdout).trim() == "focused" {
                return Some(app.to_string());
            }
        }
    }

    let safari_script = format!(
        r#"if application "Safari" is running then
  tell application "Safari"
    repeat with w in windows
      repeat with t in tabs of w
        try
          if (URL of t as string) contains "{host_filter}" then
            set current tab of w to t
            set index of w to 1
            activate
            return "focused"
          end if
        end try
      end repeat
    end repeat
  end tell
end if
return "nf""#
    );
    if let Ok(out) = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&safari_script)
        .output()
    {
        if String::from_utf8_lossy(&out.stdout).trim() == "focused" {
            return Some("Safari".to_string());
        }
    }
    None
}

fn shell_quote(s: &str) -> String {
    if s.contains(char::is_whitespace) || s.contains('\'') || s.contains('"') {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}
