use crate::infra::paths;
use crate::services::transcript::{
    jsonl_summary, jsonl_tail, session_title,
};
use crate::http::dto::*;
use crate::infra::tmux::{self, TmuxConfig};
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

fn parse_ps_output(text: &str) -> HashMap<u32, PsRow> {
    text.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                return None;
            }
            let pid: u32 = parts[0].parse().ok()?;
            let ppid: u32 = parts[1].parse().unwrap_or(0);
            let tty = parts[2].to_string();
            let command = parts.get(3..).map(|rest| rest.join(" ")).unwrap_or_default();
            Some((pid, PsRow { ppid, tty, command }))
        })
        .collect()
}

fn refresh_ps_cache(cache: &Mutex<PsCache>) -> HashMap<u32, PsRow> {
    let n = now();
    {
        let c = cache.lock().unwrap();
        if n - c.at < PS_TTL {
            return c.map.clone();
        }
    }
    let map = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,ppid=,tty=,command="])
        .output()
        .ok()
        .map(|out| parse_ps_output(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or_default();
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
    let last_type = jsonl
        .get("last_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let freshest = hook_age.min(jsonl_age);

    if hook_event == "Notification" && hook_age < NOTIFICATION_TTL && is_approval_message(hook_msg)
    {
        return ("needs_input".into(), Some(hook_msg.to_string()));
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

    let script = build_terminal_script(&attach_cmd, iterm_installed());
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    Ok(Json(OkResponse::new()))
}

fn iterm_installed() -> bool {
    std::path::Path::new("/Applications/iTerm.app").is_dir()
}

fn build_terminal_script(attach_cmd: &str, use_iterm: bool) -> String {
    if use_iterm {
        format!(
            "tell application \"iTerm\"\ncreate window with default profile command \"{attach_cmd}\"\nend tell"
        )
    } else {
        format!(
            "tell application \"Terminal\"\ndo script \"{attach_cmd}\"\nend tell"
        )
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{set_test_home, FS_LOCK as LOCK};
    use crate::infra::tmux::command::test_stub::write_stub_tmux;

    fn wipe_dirs() {
        let _ = std::fs::remove_dir_all(&*paths::APP_DIR);
        let _ = std::fs::remove_dir_all(&*paths::CLAUDE_DIR);
    }

    fn fresh_state(tmux_bin: PathBuf) -> Arc<AppState> {
        Arc::new(AppState {
            tmux_config: TmuxConfig {
                tmux_bin,
                socket_name: "ciu-test".into(),
                name_prefix: "ciu-".into(),
            },
            auth_token: "tok".into(),
            server_start: 1.0,
            instances_cache: Mutex::new(InstancesCache { at: 0.0, data: vec![] }),
            pending_focus: Mutex::new(PendingFocus { sid: None, ts: 0.0 }),
            ps_cache: Mutex::new(PsCache { at: 0.0, map: HashMap::new() }),
        })
    }

    fn blank_instance(session_id: &str, our_sid: Option<&str>) -> InstanceData {
        InstanceData {
            session_id: session_id.into(),
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

    #[test]
    fn now_returns_positive_time() {
        assert!(now() > 0.0);
    }

    #[test]
    fn parse_mcp_arg_space_form() {
        assert_eq!(parse_mcp_arg("claude --mcp-config /a.json"), vec!["/a.json"]);
    }

    #[test]
    fn parse_mcp_arg_equals_form() {
        assert_eq!(parse_mcp_arg("claude --mcp-config=/b.json"), vec!["/b.json"]);
    }

    #[test]
    fn parse_mcp_arg_multiple_mixed() {
        let r = parse_mcp_arg("claude --mcp-config /a --mcp-config=/b");
        assert_eq!(r, vec!["/a", "/b"]);
    }

    #[test]
    fn parse_mcp_arg_none_returns_empty() {
        assert!(parse_mcp_arg("claude -p hi").is_empty());
    }

    #[test]
    fn parse_mcp_arg_flag_without_value_is_skipped() {
        assert!(parse_mcp_arg("claude --mcp-config").is_empty());
    }

    #[test]
    fn is_headless_dash_p_middle() {
        assert!(is_headless("claude -p hi"));
    }

    #[test]
    fn is_headless_print_middle() {
        assert!(is_headless("claude --print hello"));
    }

    #[test]
    fn is_headless_dash_p_suffix() {
        assert!(is_headless("claude -p"));
    }

    #[test]
    fn is_headless_print_suffix() {
        assert!(is_headless("claude --print"));
    }

    #[test]
    fn is_headless_false_for_plain_claude() {
        assert!(!is_headless("claude"));
        assert!(!is_headless("claude --resume"));
    }

    #[test]
    fn is_approval_message_permission_keyword() {
        assert!(is_approval_message("Claude requested permission"));
    }

    #[test]
    fn is_approval_message_approve_keyword() {
        assert!(is_approval_message("please approve"));
    }

    #[test]
    fn is_approval_message_confirm_mixed_case() {
        assert!(is_approval_message("Please Confirm"));
    }

    #[test]
    fn is_approval_message_allow_keyword() {
        assert!(is_approval_message("allow access"));
    }

    #[test]
    fn is_approval_message_approval_keyword() {
        assert!(is_approval_message("needs approval"));
    }

    #[test]
    fn is_approval_message_waiting_exception_overrides() {
        assert!(!is_approval_message("Waiting for your input to approve"));
    }

    #[test]
    fn is_approval_message_empty_is_false() {
        assert!(!is_approval_message(""));
    }

    #[test]
    fn is_approval_message_non_keyword_is_false() {
        assert!(!is_approval_message("Hello there"));
    }

    #[test]
    fn display_name_custom_override_wins() {
        let names = json!({"sid1": "my name"});
        assert_eq!(display_name("sid1", Some("/tmp"), Some("Title"), &names), "my name");
    }

    #[test]
    fn display_name_empty_override_falls_through_to_title() {
        let names = json!({"sid1": ""});
        assert_eq!(display_name("sid1", Some("/tmp"), Some("Title"), &names), "Title");
    }

    #[test]
    fn display_name_jsonl_title_when_no_override() {
        let names = json!({});
        assert_eq!(display_name("sid1", Some("/tmp"), Some("Title"), &names), "Title");
    }

    #[test]
    fn display_name_falls_back_to_cwd_and_sid_prefix() {
        let names = json!({});
        let n = display_name("abcdef1234567", Some("/home/user/proj"), None, &names);
        assert_eq!(n, "proj · abcdef12");
    }

    #[test]
    fn display_name_short_sid_not_truncated() {
        let names = json!({});
        let n = display_name("xyz", Some("/x/y"), None, &names);
        assert_eq!(n, "y · xyz");
    }

    #[test]
    fn display_name_no_cwd_uses_unknown() {
        let names = json!({});
        let n = display_name("abcdefgh12345", None, None, &names);
        assert_eq!(n, "unknown · abcdefgh");
    }

    #[test]
    fn pid_alive_current_process_true() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn pid_alive_huge_invalid_pid_false() {
        assert!(!pid_alive(4_000_000_000));
    }

    #[test]
    fn read_json_missing_returns_empty_object() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let p = paths::APP_DIR.join("nope-xyz.json");
        let _ = std::fs::remove_file(&p);
        assert_eq!(read_json(&p), json!({}));
    }

    #[test]
    fn read_json_invalid_returns_empty_object() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        let p = paths::APP_DIR.join("bad.json");
        std::fs::write(&p, "not json").unwrap();
        assert_eq!(read_json(&p), json!({}));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_json_parses_valid() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        let p = paths::APP_DIR.join("ok.json");
        std::fs::write(&p, r#"{"k": 7}"#).unwrap();
        assert_eq!(read_json(&p).get("k").and_then(|v| v.as_u64()), Some(7));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn write_json_creates_parent_dir_and_round_trips() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let p = paths::APP_DIR.join("sub/nest/new.json");
        write_json(&p, &json!({"x": 1}));
        assert!(p.exists());
        assert_eq!(read_json(&p).get("x").and_then(|v| v.as_u64()), Some(1));
    }

    #[test]
    fn mcp_servers_at_missing_file_is_empty() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let p = paths::APP_DIR.join("not-here-mcp.json");
        let _ = std::fs::remove_file(&p);
        assert!(mcp_servers_at(&p).is_empty());
    }

    #[test]
    fn mcp_servers_at_filters_non_object_entries() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        let p = paths::APP_DIR.join("mcp-mixed.json");
        std::fs::write(
            &p,
            r#"{"mcpServers": {"good": {"command": "x"}, "bad": "str"}}"#,
        )
        .unwrap();
        let m = mcp_servers_at(&p);
        assert_eq!(m.len(), 1);
        assert!(m.contains_key("good"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn mcp_servers_at_missing_mcp_servers_key_is_empty() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        let p = paths::APP_DIR.join("mcp-nokey.json");
        std::fs::write(&p, r#"{"other": {}}"#).unwrap();
        assert!(mcp_servers_at(&p).is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn available_mcp_servers_merges_home_and_global() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        std::fs::create_dir_all(h.join(".claude")).unwrap();
        std::fs::write(
            h.join(".claude.json"),
            r#"{"mcpServers": {"a": {"command": "x"}}}"#,
        )
        .unwrap();
        std::fs::write(
            &*paths::GLOBAL_MCP,
            r#"{"mcpServers": {"b": {"command": "y"}}}"#,
        )
        .unwrap();
        let all = available_mcp_servers();
        assert!(all.contains_key("a"));
        assert!(all.contains_key("b"));
    }

    #[test]
    fn available_mcp_servers_first_source_wins_on_duplicate() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        std::fs::create_dir_all(h.join(".claude")).unwrap();
        std::fs::write(
            h.join(".claude.json"),
            r#"{"mcpServers": {"dup": {"v": 1}}}"#,
        )
        .unwrap();
        std::fs::write(
            &*paths::GLOBAL_MCP,
            r#"{"mcpServers": {"dup": {"v": 2}}}"#,
        )
        .unwrap();
        let all = available_mcp_servers();
        assert_eq!(
            all.get("dup").unwrap().get("v").and_then(|v| v.as_u64()),
            Some(1)
        );
    }

    #[test]
    fn load_mcps_includes_project_mcp_sorted() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        std::fs::create_dir_all(h.join(".claude")).unwrap();
        let proj = h.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join(".mcp.json"),
            r#"{"mcpServers": {"z": {}, "a": {}}}"#,
        )
        .unwrap();
        let r = load_mcps(Some(proj.to_str().unwrap()), "claude");
        let project: Vec<String> = r
            .get("project")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(project, vec!["a", "z"]);
    }

    #[test]
    fn load_mcps_parses_explicit_arg() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        std::fs::create_dir_all(&*paths::APP_DIR).unwrap();
        let cfg_path = paths::APP_DIR.join("explicit.json");
        std::fs::write(&cfg_path, r#"{"mcpServers": {"ex1": {}}}"#).unwrap();
        let cmd = format!("claude --mcp-config {}", cfg_path.to_string_lossy());
        let r = load_mcps(None, &cmd);
        let explicit: Vec<String> = r
            .get("explicit")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(explicit, vec!["ex1"]);
    }

    #[test]
    fn load_mcps_sorts_and_dedups_explicit_across_multiple_files() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        std::fs::create_dir_all(&*paths::APP_DIR).unwrap();
        let p1 = paths::APP_DIR.join("ax.json");
        let p2 = paths::APP_DIR.join("bx.json");
        std::fs::write(&p1, r#"{"mcpServers": {"z": {}, "a": {}}}"#).unwrap();
        std::fs::write(&p2, r#"{"mcpServers": {"a": {}, "m": {}}}"#).unwrap();
        let cmd = format!(
            "claude --mcp-config={} --mcp-config {}",
            p1.to_string_lossy(),
            p2.to_string_lossy()
        );
        let r = load_mcps(None, &cmd);
        let explicit: Vec<String> = r
            .get("explicit")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(explicit, vec!["a", "m", "z"]);
    }

    #[test]
    fn load_mcps_explicit_path_without_mcp_servers_field_yields_empty() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        std::fs::create_dir_all(&*paths::APP_DIR).unwrap();
        let cfg = paths::APP_DIR.join("no-mcps.json");
        std::fs::write(&cfg, r#"{"other": 1}"#).unwrap();
        let cmd = format!("claude --mcp-config {}", cfg.to_string_lossy());
        let r = load_mcps(None, &cmd);
        let explicit: Vec<Value> = r
            .get("explicit")
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        assert!(explicit.is_empty());
    }

    #[test]
    fn load_mcps_explicit_path_missing_is_skipped() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let cmd = "claude --mcp-config /nope/nonexistent.json".to_string();
        let r = load_mcps(None, &cmd);
        let explicit: Vec<Value> = r
            .get("explicit")
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        assert!(explicit.is_empty());
    }

    #[test]
    fn load_mcps_has_empty_project_when_no_cwd() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let r = load_mcps(None, "claude");
        let project: Vec<Value> = r
            .get("project")
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        assert!(project.is_empty());
    }

    #[test]
    fn resolve_status_not_alive_returns_ended() {
        let (s, n) = resolve_status(&json!({}), false, &json!({}));
        assert_eq!(s, "ended");
        assert!(n.is_none());
    }

    #[test]
    fn resolve_status_notification_approval_needs_input() {
        let t = now() - 1.0;
        let hook = json!({
            "last_event": "Notification",
            "notification_message": "Please approve this tool",
            "timestamp": t
        });
        let (s, n) = resolve_status(&hook, true, &json!({}));
        assert_eq!(s, "needs_input");
        assert_eq!(n.as_deref(), Some("Please approve this tool"));
    }

    #[test]
    fn resolve_status_notification_non_approval_is_idle() {
        let t = now() - 1.0;
        let hook = json!({
            "last_event": "Notification",
            "notification_message": "just a heads up",
            "timestamp": t
        });
        let (s, n) = resolve_status(&hook, true, &json!({}));
        assert_eq!(s, "idle");
        assert!(n.is_none());
    }

    #[test]
    fn resolve_status_stop_fresh_is_idle() {
        let t = now() - 3.0;
        let hook = json!({"last_event": "Stop", "timestamp": t});
        let (s, _) = resolve_status(&hook, true, &json!({}));
        assert_eq!(s, "idle");
    }

    #[test]
    fn resolve_status_hook_event_fresh_is_running() {
        let t = now() - 5.0;
        let hook = json!({"last_event": "PostToolUse", "timestamp": t});
        let (s, _) = resolve_status(&hook, true, &json!({}));
        assert_eq!(s, "running");
    }

    #[test]
    fn resolve_status_hook_event_stale_falls_through_to_idle() {
        let t = now() - 60.0;
        let hook = json!({"last_event": "PostToolUse", "timestamp": t});
        let (s, _) = resolve_status(&hook, true, &json!({}));
        assert_eq!(s, "idle");
    }

    #[test]
    fn resolve_status_user_last_type_fresh_is_running() {
        let j = json!({"last_type": "user", "last_timestamp_epoch": now() - 2.0});
        let (s, _) = resolve_status(&json!({}), true, &j);
        assert_eq!(s, "running");
    }

    #[test]
    fn resolve_status_pending_fresh_is_running() {
        let j = json!({"pending": true, "last_timestamp_epoch": now() - 2.0});
        let (s, _) = resolve_status(&json!({}), true, &j);
        assert_eq!(s, "running");
    }

    #[test]
    fn resolve_status_default_is_idle() {
        let (s, _) = resolve_status(&json!({}), true, &json!({}));
        assert_eq!(s, "idle");
    }

    #[test]
    fn build_terminal_script_uses_iterm_when_flag_true() {
        let s = build_terminal_script("tmux-attach", true);
        assert!(s.contains("iTerm"));
        assert!(s.contains("tmux-attach"));
    }

    #[test]
    fn build_terminal_script_uses_terminal_when_flag_false() {
        let s = build_terminal_script("tmux-attach", false);
        assert!(s.contains("Terminal"));
        assert!(!s.contains("iTerm"));
        assert!(s.contains("tmux-attach"));
    }

    #[test]
    fn iterm_installed_returns_bool_without_panic() {
        let _ = iterm_installed();
    }

    #[test]
    fn resolve_status_stale_notification_is_idle() {
        let t = now() - 3000.0;
        let hook = json!({
            "last_event": "Notification",
            "notification_message": "approve",
            "timestamp": t
        });
        let (s, _) = resolve_status(&hook, true, &json!({}));
        assert_eq!(s, "idle");
    }

    #[test]
    fn resolve_status_stale_user_is_idle() {
        let j = json!({"last_type": "user", "last_timestamp_epoch": now() - 60.0});
        let (s, _) = resolve_status(&json!({}), true, &j);
        assert_eq!(s, "idle");
    }

    #[test]
    fn stash_and_promote_pending_name_sets_name_on_instance() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        stash_pending_name("sid-x", "Pretty Name");
        let pending = read_json(&paths::PENDING_NAMES_FILE);
        assert_eq!(
            pending.get("sid-x").and_then(|v| v.as_str()),
            Some("Pretty Name")
        );
        let mut items = vec![blank_instance("session-id", Some("sid-x"))];
        promote_pending_names(&mut items);
        assert_eq!(items[0].custom_name.as_deref(), Some("Pretty Name"));
        assert_eq!(items[0].name, "Pretty Name");
        let pending_after = read_json(&paths::PENDING_NAMES_FILE);
        assert!(pending_after.as_object().unwrap().is_empty());
        let names = read_json(&paths::NAMES_FILE);
        assert_eq!(
            names.get("session-id").and_then(|v| v.as_str()),
            Some("Pretty Name")
        );
    }

    #[test]
    fn stash_and_promote_pending_group_adds_session_to_group() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        stash_pending_group("sid-g", "Group A");
        let mut items = vec![blank_instance("session-abc", Some("sid-g"))];
        promote_pending_names(&mut items);
        let groups = read_json(&paths::GROUPS_FILE);
        let arr = groups.get("Group A").and_then(|v| v.as_array()).unwrap();
        assert!(arr.iter().any(|v| v.as_str() == Some("session-abc")));
    }

    #[test]
    fn promote_does_not_clobber_existing_named_sessions() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        std::fs::write(&*paths::NAMES_FILE, r#"{"already": "existing"}"#).unwrap();
        stash_pending_name("sid-x", "Pending");
        let mut items = vec![blank_instance("already", Some("sid-x"))];
        promote_pending_names(&mut items);
        let names = read_json(&paths::NAMES_FILE);
        assert_eq!(
            names.get("already").and_then(|v| v.as_str()),
            Some("existing")
        );
        assert_eq!(items[0].name, "existing");
    }

    #[test]
    fn promote_pending_group_skips_when_sid_already_in_group() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        std::fs::write(
            &*paths::GROUPS_FILE,
            r#"{"Group A": ["session-dup"]}"#,
        )
        .unwrap();
        stash_pending_group("sid-dup", "Group A");
        let mut items = vec![blank_instance("session-dup", Some("sid-dup"))];
        promote_pending_names(&mut items);
        let groups = read_json(&paths::GROUPS_FILE);
        let arr = groups.get("Group A").and_then(|v| v.as_array()).unwrap();
        assert_eq!(arr.len(), 1);
        let pending = read_json(&paths::PENDING_GROUPS_FILE);
        assert!(pending.get("sid-dup").is_none());
    }

    #[test]
    fn promote_pending_names_with_no_instances_is_noop() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let mut items: Vec<InstanceData> = vec![];
        promote_pending_names(&mut items);
        assert!(items.is_empty());
    }

    #[test]
    fn read_first_user_text_string_content_trimmed() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("first-a.jsonl");
        std::fs::write(
            &p,
            "{\"type\":\"user\",\"message\":{\"content\":\"  hello world  \"}}\n{\"type\":\"assistant\"}\n",
        )
        .unwrap();
        assert_eq!(read_first_user_text(&p).as_deref(), Some("hello world"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_first_user_text_array_content_joins_newlines() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("first-b.jsonl");
        let line = "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"line1\\nline2\"}]}}";
        std::fs::write(&p, line).unwrap();
        assert_eq!(read_first_user_text(&p).as_deref(), Some("line1 line2"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_first_user_text_truncates_to_100_chars() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("first-c.jsonl");
        let long = "a".repeat(250);
        let l = format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":\"{long}\"}}}}"
        );
        std::fs::write(&p, l).unwrap();
        let r = read_first_user_text(&p).unwrap();
        assert_eq!(r.len(), 100);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_first_user_text_missing_file_is_none() {
        assert!(read_first_user_text(std::path::Path::new(
            "/definitely/not/a/file.jsonl"
        ))
        .is_none());
    }

    #[test]
    fn read_first_user_text_no_user_lines_is_none() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        let p = h.join("first-d.jsonl");
        std::fs::write(
            &p,
            "{\"type\":\"assistant\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();
        assert!(read_first_user_text(&p).is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn subagent_info_no_cwd_empty() {
        assert!(subagent_info("sid", None).is_empty());
    }

    #[test]
    fn subagent_info_missing_dir_empty() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        assert!(subagent_info("sid-none", Some(h.to_str().unwrap())).is_empty());
    }

    #[test]
    fn subagent_info_reads_agent_files_and_labels() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let cwd = "/home/work/proj";
        let slug = format!("-{}", cwd.replace('/', "-").trim_start_matches('-'));
        let sub_dir = paths::PROJECTS_DIR
            .join(&slug)
            .join("sess-1")
            .join("subagents");
        std::fs::create_dir_all(&sub_dir).unwrap();
        std::fs::write(
            sub_dir.join("agent-aaa.jsonl"),
            "{\"type\":\"user\",\"message\":{\"content\":\"task one\"}}",
        )
        .unwrap();
        std::fs::write(sub_dir.join("other.jsonl"), "").unwrap();
        let agents = subagent_info("sess-1", Some(cwd));
        assert_eq!(agents.len(), 1);
        assert_eq!(
            agents[0].get("agent_id").and_then(|v| v.as_str()),
            Some("aaa")
        );
        assert_eq!(
            agents[0].get("label").and_then(|v| v.as_str()),
            Some("task one")
        );
    }

    #[tokio::test]
    async fn cached_instances_empty_when_no_data() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let r = state.cached_instances();
        assert!(r.is_empty());
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn cached_instances_returns_cached_within_ttl() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        {
            let mut c = state.instances_cache.lock().unwrap();
            c.at = now();
            c.data = vec![blank_instance("cached", None)];
        }
        let r = state.cached_instances();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].session_id, "cached");
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn invalidate_cache_resets_timestamp() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        state.instances_cache.lock().unwrap().at = now();
        state.invalidate_cache();
        assert_eq!(state.instances_cache.lock().unwrap().at, 0.0);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_instances_returns_shape() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let resp = api_instances(State(state.clone())).await;
        assert_eq!(resp.get("server_start").and_then(|v| v.as_f64()), Some(1.0));
        assert!(resp.get("served_at").and_then(|v| v.as_f64()).unwrap() > 0.0);
        assert!(resp.get("instances").and_then(|v| v.as_array()).is_some());
        assert!(resp.get("pending_focus").and_then(|v| v.as_str()).is_none());
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_instances_surfaces_fresh_pending_focus() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        {
            let mut pf = state.pending_focus.lock().unwrap();
            pf.sid = Some("abc".into());
            pf.ts = now();
        }
        let resp = api_instances(State(state.clone())).await;
        assert_eq!(
            resp.get("pending_focus").and_then(|v| v.as_str()),
            Some("abc")
        );
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_instances_drops_stale_pending_focus() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        {
            let mut pf = state.pending_focus.lock().unwrap();
            pf.sid = Some("abc".into());
            pf.ts = now() - 999.0;
        }
        let resp = api_instances(State(state.clone())).await;
        assert!(resp.get("pending_focus").and_then(|v| v.as_str()).is_none());
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_clear_pending_focus_resets_sid() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        state.pending_focus.lock().unwrap().sid = Some("x".into());
        let r = api_clear_pending_focus(State(state.clone())).await;
        assert!(r.ok);
        assert!(state.pending_focus.lock().unwrap().sid.is_none());
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_groups_returns_stored_groups() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        std::fs::write(&*paths::GROUPS_FILE, r#"{"alpha": ["s1"]}"#).unwrap();
        let resp = api_groups().await;
        assert_eq!(
            resp.get("alpha")
                .and_then(|v| v.as_array())
                .unwrap()
                .first()
                .and_then(|v| v.as_str()),
            Some("s1")
        );
    }

    #[tokio::test]
    async fn api_set_groups_writes_body_verbatim() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let data = json!({"beta": ["s2", "s3"]});
        let r = api_set_groups(Json(data.clone())).await;
        assert!(r.ok);
        let back = read_json(&*paths::GROUPS_FILE);
        assert_eq!(back, data);
    }

    #[tokio::test]
    async fn api_rename_sets_name() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let r = api_rename(
            Path("sid1".into()),
            Json(RenameBody { name: "New".into() }),
        )
        .await;
        assert!(r.ok);
        let names = read_json(&*paths::NAMES_FILE);
        assert_eq!(names.get("sid1").and_then(|v| v.as_str()), Some("New"));
    }

    #[tokio::test]
    async fn api_rename_trims_whitespace() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = api_rename(
            Path("sid1".into()),
            Json(RenameBody {
                name: "   Trim   ".into(),
            }),
        )
        .await;
        let names = read_json(&*paths::NAMES_FILE);
        assert_eq!(names.get("sid1").and_then(|v| v.as_str()), Some("Trim"));
    }

    #[tokio::test]
    async fn api_rename_empty_removes_entry() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        std::fs::write(&*paths::NAMES_FILE, r#"{"sid1": "old"}"#).unwrap();
        let _ = api_rename(
            Path("sid1".into()),
            Json(RenameBody {
                name: "   ".into(),
            }),
        )
        .await;
        let names = read_json(&*paths::NAMES_FILE);
        assert!(names.get("sid1").is_none());
    }

    #[tokio::test]
    async fn api_set_group_adds_to_group() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = api_set_group(
            Path("sid1".into()),
            Json(GroupBody {
                group: Some("g1".into()),
            }),
        )
        .await;
        let groups = read_json(&*paths::GROUPS_FILE);
        let arr = groups.get("g1").and_then(|v| v.as_array()).unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str(), Some("sid1"));
    }

    #[tokio::test]
    async fn api_set_group_removes_from_prior_and_clears_empty_buckets() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        std::fs::write(&*paths::GROUPS_FILE, r#"{"old": ["sid1"]}"#).unwrap();
        let _ = api_set_group(
            Path("sid1".into()),
            Json(GroupBody {
                group: Some("new".into()),
            }),
        )
        .await;
        let groups = read_json(&*paths::GROUPS_FILE);
        assert!(groups.get("old").is_none());
        let arr = groups.get("new").and_then(|v| v.as_array()).unwrap();
        assert_eq!(arr.len(), 1);
    }

    #[tokio::test]
    async fn api_set_group_none_just_removes() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        std::fs::write(&*paths::GROUPS_FILE, r#"{"g1": ["sid1", "sid2"]}"#).unwrap();
        let _ = api_set_group(Path("sid1".into()), Json(GroupBody { group: None })).await;
        let groups = read_json(&*paths::GROUPS_FILE);
        let arr = groups.get("g1").and_then(|v| v.as_array()).unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str(), Some("sid2"));
    }

    #[tokio::test]
    async fn api_signal_404_when_session_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let err = api_signal(
            State(state),
            Path("missing".into()),
            Json(SignalBody { signal: None }),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_signal_400_when_our_sid_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        {
            let mut c = state.instances_cache.lock().unwrap();
            c.at = now();
            c.data = vec![blank_instance("sid-x", None)];
        }
        let err = api_signal(
            State(state),
            Path("sid-x".into()),
            Json(SignalBody { signal: None }),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_forget_removes_state_file() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let p = paths::STATE_DIR.join("sidf.json");
        std::fs::write(&p, "{}").unwrap();
        let r = api_forget(Path("sidf".into())).await;
        assert!(r.ok);
        assert!(!p.exists());
    }

    #[tokio::test]
    async fn api_forget_missing_file_still_ok() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let r = api_forget(Path("nonexistent".into())).await;
        assert!(r.ok);
    }

    #[tokio::test]
    async fn api_ack_updates_when_newer() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let resp = api_ack(
            Path("sid1".into()),
            Json(AckBody { timestamp: 100.0 }),
        )
        .await;
        assert!(
            (resp.get("ack_timestamp").and_then(|v| v.as_f64()).unwrap() - 100.0).abs()
                < 0.01
        );
    }

    #[tokio::test]
    async fn api_ack_ignores_older() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        std::fs::write(&*paths::ACKS_FILE, r#"{"sid1": 500.0}"#).unwrap();
        let resp = api_ack(
            Path("sid1".into()),
            Json(AckBody { timestamp: 100.0 }),
        )
        .await;
        assert!(
            (resp.get("ack_timestamp").and_then(|v| v.as_f64()).unwrap() - 500.0).abs()
                < 0.01
        );
    }

    #[tokio::test]
    async fn api_new_instance_rejects_non_directory() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let err = api_new_instance(
            State(state),
            Json(NewInstanceBody {
                cwd: "/absolutely/not/a/real/directory/xyz".into(),
                command: None,
                mcps: None,
                mcp_source: None,
                name: None,
                group: None,
            }),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_new_instance_returns_sid_and_stashes_name_and_group() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let resp = api_new_instance(
            State(state.clone()),
            Json(NewInstanceBody {
                cwd: paths::HOME.to_string_lossy().to_string(),
                command: None,
                mcps: None,
                mcp_source: None,
                name: Some("myname".into()),
                group: Some("grp1".into()),
            }),
        )
        .await
        .unwrap();
        let osid = resp.get("our_sid").and_then(|v| v.as_str()).unwrap();
        assert_eq!(osid.len(), 12);
        let pn = read_json(&*paths::PENDING_NAMES_FILE);
        assert_eq!(pn.get(osid).and_then(|v| v.as_str()), Some("myname"));
        let pg = read_json(&*paths::PENDING_GROUPS_FILE);
        assert_eq!(pg.get(osid).and_then(|v| v.as_str()), Some("grp1"));
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_new_instance_maps_spawn_failure_to_500() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 1\n");
        let state = fresh_state(bin.clone());
        let err = api_new_instance(
            State(state),
            Json(NewInstanceBody {
                cwd: paths::HOME.to_string_lossy().to_string(),
                command: None,
                mcps: None,
                mcp_source: None,
                name: None,
                group: None,
            }),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(err.0, StatusCode::INTERNAL_SERVER_ERROR);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_new_instance_with_mcps_writes_config_file() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        std::fs::create_dir_all(h.join(".claude")).unwrap();
        std::fs::write(
            h.join(".claude.json"),
            r#"{"mcpServers": {"srvA": {"command": "x"}}}"#,
        )
        .unwrap();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let _ = api_new_instance(
            State(state),
            Json(NewInstanceBody {
                cwd: paths::HOME.to_string_lossy().to_string(),
                command: None,
                mcps: Some(vec!["srvA".into()]),
                mcp_source: None,
                name: None,
                group: None,
            }),
        )
        .await
        .unwrap();
        let mut found = false;
        if let Ok(rd) = std::fs::read_dir(&*paths::MCP_CONFIG_DIR) {
            for entry in rd.flatten() {
                let data = read_json(&entry.path());
                if data
                    .pointer("/mcpServers/srvA")
                    .is_some()
                {
                    found = true;
                }
            }
        }
        assert!(found);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_input_404_when_session_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let err = api_input(
            State(state),
            Path("missing".into()),
            Json(InputBody {
                text: "hi".into(),
                submit: Some(false),
            }),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_input_400_when_our_sid_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        {
            let mut c = state.instances_cache.lock().unwrap();
            c.at = now();
            c.data = vec![blank_instance("sid-u", None)];
        }
        let err = api_input(
            State(state),
            Path("sid-u".into()),
            Json(InputBody {
                text: String::new(),
                submit: Some(false),
            }),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_kill_404_when_session_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let err = api_kill(State(state), Path("missing".into()))
            .await
            .err()
            .unwrap();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_kill_400_when_our_sid_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        {
            let mut c = state.instances_cache.lock().unwrap();
            c.at = now();
            c.data = vec![blank_instance("sid-k", None)];
        }
        let err = api_kill(State(state), Path("sid-k".into()))
            .await
            .err()
            .unwrap();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_open_terminal_404_when_session_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let err = api_open_terminal(State(state), Path("missing".into()))
            .await
            .err()
            .unwrap();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_open_terminal_400_when_our_sid_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        {
            let mut c = state.instances_cache.lock().unwrap();
            c.at = now();
            c.data = vec![blank_instance("sid-o", None)];
        }
        let err = api_open_terminal(State(state), Path("sid-o".into()))
            .await
            .err()
            .unwrap();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_recent_cwds_empty_when_no_data() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::remove_dir_all(&*paths::PROJECTS_DIR);
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let resp = api_recent_cwds(State(state)).await;
        let cwds = resp.get("cwds").and_then(|v| v.as_array()).unwrap();
        assert_eq!(cwds.len(), 0);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_recent_cwds_includes_cached_instance_cwds() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        {
            let mut c = state.instances_cache.lock().unwrap();
            c.at = now();
            let mut inst = blank_instance("sid-c", None);
            inst.cwd = Some("/some/cached/cwd".into());
            inst.hook_timestamp = Some(100.0);
            c.data = vec![inst];
        }
        let resp = api_recent_cwds(State(state)).await;
        let cwds: Vec<String> = resp
            .get("cwds")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(cwds.contains(&"/some/cached/cwd".to_string()));
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn shell_quote_passthrough_simple() {
        assert_eq!(shell_quote("plain"), "plain");
    }

    #[test]
    fn shell_quote_whitespace_adds_quotes() {
        assert_eq!(shell_quote("has space"), "'has space'");
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("it's a test"), "'it'\\''s a test'");
    }

    #[test]
    fn shell_quote_double_quotes_trigger_quoting() {
        assert_eq!(shell_quote("has\"double"), "'has\"double'");
    }

    #[test]
    fn shell_quote_empty_string_passthrough() {
        assert_eq!(shell_quote(""), "");
    }

    #[test]
    fn ps_row_clone_preserves_fields() {
        let r = PsRow {
            ppid: 1,
            tty: "t".into(),
            command: "c".into(),
        };
        let r2 = r.clone();
        assert_eq!(r2.ppid, 1);
        assert_eq!(r2.tty, "t");
        assert_eq!(r2.command, "c");
    }

    #[test]
    fn refresh_ps_cache_returns_within_ttl() {
        let cache = Mutex::new(PsCache {
            at: now(),
            map: {
                let mut m = HashMap::new();
                m.insert(
                    42,
                    PsRow {
                        ppid: 1,
                        tty: "t".into(),
                        command: "pre-seeded".into(),
                    },
                );
                m
            },
        });
        let m = refresh_ps_cache(&cache);
        assert_eq!(m.get(&42).unwrap().command, "pre-seeded");
    }

    #[test]
    fn refresh_ps_cache_populates_when_expired() {
        let cache = Mutex::new(PsCache {
            at: 0.0,
            map: HashMap::new(),
        });
        let m = refresh_ps_cache(&cache);
        assert!(m.contains_key(&std::process::id()));
    }

    #[test]
    fn parse_ps_output_handles_malformed_rows() {
        let text = "onlyone\n\
                    notanumber 1 tty0 cmd\n\
                    200 1 tty1\n\
                    300 1 tty2 foo bar baz\n";
        let m = parse_ps_output(text);
        assert!(m.get(&0).is_none());
        let row200 = m.get(&200).expect("pid 200 should be present");
        assert_eq!(row200.tty, "tty1");
        assert_eq!(row200.command, "");
        let row300 = m.get(&300).expect("pid 300 should be present");
        assert_eq!(row300.command, "foo bar baz");
    }

    #[test]
    fn parse_ps_output_parses_ppid_zero_when_not_numeric() {
        let text = "100 notnum tty0 cmd\n";
        let m = parse_ps_output(text);
        let row = m.get(&100).expect("pid 100 should be present");
        assert_eq!(row.ppid, 0);
    }

    #[test]
    fn gather_instances_returns_orphan_for_recent_state_without_session() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let sid = "orphan-sess-1";
        let n = now();
        let state_json = json!({
            "our_sid": "orphan-ours",
            "timestamp": n - 10.0,
            "last_event": "Stop",
        });
        std::fs::write(
            paths::STATE_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&state_json).unwrap(),
        )
        .unwrap();
        let cfg = TmuxConfig {
            tmux_bin: "/no/such/bin".into(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].session_id, sid);
        assert_eq!(items[0].status, "ended");
        assert!(!items[0].alive);
        assert_eq!(items[0].our_sid.as_deref(), Some("orphan-ours"));
    }

    #[test]
    fn gather_instances_skips_orphan_older_than_one_day() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let state_json = json!({
            "our_sid": "stale-ours",
            "timestamp": now() - 100_000.0,
        });
        std::fs::write(
            paths::STATE_DIR.join("stale-sid.json"),
            serde_json::to_string(&state_json).unwrap(),
        )
        .unwrap();
        let cfg = TmuxConfig {
            tmux_bin: "/no/such/bin".into(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        assert!(items.is_empty());
    }

    #[test]
    fn gather_instances_skips_orphan_with_subagents_transcript_path() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let state_json = json!({
            "our_sid": "sub-ours",
            "timestamp": now(),
            "transcript_path": "/some/project/subagents/agent-1.jsonl",
        });
        std::fs::write(
            paths::STATE_DIR.join("sub-sid.json"),
            serde_json::to_string(&state_json).unwrap(),
        )
        .unwrap();
        let cfg = TmuxConfig {
            tmux_bin: "/no/such/bin".into(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        assert!(items.is_empty());
    }

    #[test]
    fn gather_instances_skips_orphan_missing_our_sid() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        std::fs::write(
            paths::STATE_DIR.join("no-ours.json"),
            r#"{"timestamp": 0.0}"#,
        )
        .unwrap();
        let cfg = TmuxConfig {
            tmux_bin: "/no/such/bin".into(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let n = now();
        let mut v: Value = serde_json::from_str(
            &std::fs::read_to_string(paths::STATE_DIR.join("no-ours.json")).unwrap(),
        )
        .unwrap();
        v["timestamp"] = json!(n - 5.0);
        std::fs::write(
            paths::STATE_DIR.join("no-ours.json"),
            serde_json::to_string(&v).unwrap(),
        )
        .unwrap();
        let items = gather_instances(&cfg, &ps);
        assert!(items.is_empty());
    }

    #[test]
    fn gather_instances_returns_empty_when_nothing_on_disk() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let cfg = TmuxConfig {
            tmux_bin: "/no/such/bin".into(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        assert!(items.is_empty());
    }

    #[test]
    fn gather_instances_respects_orphan_cap_of_200() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let n = now();
        for i in 0..205 {
            let state_json = json!({
                "our_sid": format!("ours-{i}"),
                "timestamp": n - 5.0,
            });
            std::fs::write(
                paths::STATE_DIR.join(format!("sid-{i:03}.json")),
                serde_json::to_string(&state_json).unwrap(),
            )
            .unwrap();
        }
        let cfg = TmuxConfig {
            tmux_bin: "/no/such/bin".into(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        assert!(items.len() <= 200);
    }

    #[test]
    fn gather_instances_returns_live_instance_when_session_matches_tmux_and_process() {
        use crate::infra::tmux::command::test_stub::write_stub_tmux;
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::SESSIONS_DIR);
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);

        let sid = "live-sess";
        let pid = std::process::id();
        let our_sid = "live-ours";
        let session_json = json!({
            "sessionId": sid,
            "pid": pid,
            "cwd": "/tmp",
            "kind": "tmux",
            "startedAt": 1.0,
        });
        std::fs::write(
            paths::SESSIONS_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&session_json).unwrap(),
        )
        .unwrap();
        let state_json = json!({
            "our_sid": our_sid,
            "timestamp": now(),
            "last_event": "Stop",
        });
        std::fs::write(
            paths::STATE_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&state_json).unwrap(),
        )
        .unwrap();

        let stub_body = format!(
            "#!/bin/sh\necho 'ciu-{our_sid}|0|/tmp'\n",
            our_sid = our_sid,
        );
        let bin = write_stub_tmux(&h, &stub_body);
        let cfg = TmuxConfig {
            tmux_bin: bin.clone(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: 0.0, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        let _ = std::fs::remove_file(&bin);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].session_id, sid);
        assert!(items[0].alive);
        assert_eq!(items[0].our_sid.as_deref(), Some(our_sid));
        assert_eq!(items[0].tmux_session.as_deref(), Some(&*format!("ciu-{our_sid}")));
    }

    #[test]
    fn gather_instances_skips_session_json_missing_pid_or_sid() {
        use crate::infra::tmux::command::test_stub::write_stub_tmux;
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::SESSIONS_DIR);
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        std::fs::write(
            paths::SESSIONS_DIR.join("bad1.json"),
            r#"{"cwd":"/tmp"}"#,
        )
        .unwrap();
        std::fs::write(
            paths::SESSIONS_DIR.join("bad2.json"),
            r#"{"pid":1,"cwd":"/tmp"}"#,
        )
        .unwrap();
        std::fs::write(
            paths::SESSIONS_DIR.join("bad3.json"),
            r#"{"sessionId":"only-sid","cwd":"/tmp"}"#,
        )
        .unwrap();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let cfg = TmuxConfig {
            tmux_bin: bin.clone(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        let _ = std::fs::remove_file(&bin);
        assert!(items.is_empty());
    }

    #[test]
    fn gather_instances_skips_session_when_state_has_no_our_sid() {
        use crate::infra::tmux::command::test_stub::write_stub_tmux;
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::SESSIONS_DIR);
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let sid = "nostatesid";
        let pid = std::process::id();
        std::fs::write(
            paths::SESSIONS_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "sessionId": sid,
                "pid": pid,
                "cwd": "/tmp",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            paths::STATE_DIR.join(format!("{sid}.json")),
            r#"{"timestamp": 0.0}"#,
        )
        .unwrap();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let cfg = TmuxConfig {
            tmux_bin: bin.clone(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        let _ = std::fs::remove_file(&bin);
        assert!(items.iter().find(|i| i.session_id == sid && i.alive).is_none());
    }

    #[test]
    fn gather_instances_skips_session_when_tmux_not_registered() {
        use crate::infra::tmux::command::test_stub::write_stub_tmux;
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::SESSIONS_DIR);
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let sid = "notmuxsid";
        let pid = std::process::id();
        std::fs::write(
            paths::SESSIONS_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "sessionId": sid,
                "pid": pid,
                "cwd": "/tmp",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            paths::STATE_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "our_sid": "unregistered-ours",
                "timestamp": now(),
                "last_event": "Stop",
            }))
            .unwrap(),
        )
        .unwrap();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let cfg = TmuxConfig {
            tmux_bin: bin.clone(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        let _ = std::fs::remove_file(&bin);
        assert!(items.iter().find(|i| i.session_id == sid && i.alive).is_none());
    }

    #[test]
    fn gather_instances_live_with_fresh_subagent_bumps_status_to_running() {
        use crate::infra::tmux::command::test_stub::write_stub_tmux;
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::SESSIONS_DIR);
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let sid = "subrun-sid";
        let pid = std::process::id();
        let our_sid = "subrun-ours";
        let cwd = format!("/tmp/subrun-{}", std::process::id());
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            paths::SESSIONS_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "sessionId": sid,
                "pid": pid,
                "cwd": cwd,
                "kind": "tmux",
                "startedAt": 1.0,
            }))
            .unwrap(),
        )
        .unwrap();
        let stale_ts = now() - 3600.0;
        std::fs::write(
            paths::STATE_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "our_sid": our_sid,
                "timestamp": stale_ts,
                "last_event": "Stop",
            }))
            .unwrap(),
        )
        .unwrap();
        let slug = format!("-{}", cwd.replace('/', "-").trim_start_matches('-'));
        let sub_dir = paths::PROJECTS_DIR.join(&slug).join(sid).join("subagents");
        std::fs::create_dir_all(&sub_dir).unwrap();
        let agent_path = sub_dir.join("agent-fresh.jsonl");
        std::fs::write(
            &agent_path,
            "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();
        let stub_body = format!(
            "#!/bin/sh\necho 'ciu-{our_sid}|0|{cwd}'\n",
            our_sid = our_sid,
            cwd = cwd,
        );
        let bin = write_stub_tmux(&h, &stub_body);
        let cfg = TmuxConfig {
            tmux_bin: bin.clone(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        let _ = std::fs::remove_file(&bin);
        let _ = std::fs::remove_dir_all(&cwd);
        let found = items
            .iter()
            .find(|i| i.session_id == sid && i.alive)
            .expect("expected live instance");
        assert_eq!(found.status, "running");
        assert!(!found.subagents.is_empty());
    }

    #[test]
    fn gather_instances_live_session_resolves_group_membership() {
        use crate::infra::tmux::command::test_stub::write_stub_tmux;
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::SESSIONS_DIR);
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        let sid = "grp-sid";
        let pid = std::process::id();
        let our_sid = "grp-ours";
        std::fs::write(
            paths::SESSIONS_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "sessionId": sid,
                "pid": pid,
                "cwd": "/tmp",
                "kind": "tmux",
                "startedAt": 1.0,
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            paths::STATE_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "our_sid": our_sid,
                "timestamp": now(),
                "last_event": "Stop",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            &*paths::GROUPS_FILE,
            format!(r#"{{"Alpha": ["{sid}"]}}"#),
        )
        .unwrap();
        let stub_body = format!(
            "#!/bin/sh\necho 'ciu-{our_sid}|0|/tmp'\n",
            our_sid = our_sid,
        );
        let bin = write_stub_tmux(&h, &stub_body);
        let cfg = TmuxConfig {
            tmux_bin: bin.clone(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        let _ = std::fs::remove_file(&bin);
        let found = items
            .iter()
            .find(|i| i.session_id == sid && i.alive)
            .expect("expected live instance");
        assert_eq!(found.group.as_deref(), Some("Alpha"));
    }

    #[test]
    fn gather_instances_dead_pid_command_is_empty_string() {
        use crate::infra::tmux::command::test_stub::write_stub_tmux;
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::SESSIONS_DIR);
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = child.id();
        let _ = child.wait();
        let sid = "dead-pid-sid";
        let our_sid = "dead-pid-ours";
        std::fs::write(
            paths::SESSIONS_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "sessionId": sid,
                "pid": dead_pid,
                "cwd": "/tmp",
                "kind": "tmux",
                "startedAt": 1.0,
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            paths::STATE_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "our_sid": our_sid,
                "timestamp": now(),
                "last_event": "Stop",
            }))
            .unwrap(),
        )
        .unwrap();
        let stub_body = format!(
            "#!/bin/sh\necho 'ciu-{our_sid}|0|/tmp'\n",
            our_sid = our_sid,
        );
        let bin = write_stub_tmux(&h, &stub_body);
        let cfg = TmuxConfig {
            tmux_bin: bin.clone(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        let _ = std::fs::remove_file(&bin);
        let found = items
            .iter()
            .find(|i| i.session_id == sid)
            .expect("expected session entry");
        assert!(!found.alive);
        assert_eq!(found.command, "");
    }

    #[test]
    fn gather_instances_live_group_iteration_skips_non_matching_group() {
        use crate::infra::tmux::command::test_stub::write_stub_tmux;
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::SESSIONS_DIR);
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        let sid = "multi-grp-sid";
        let pid = std::process::id();
        let our_sid = "multi-grp-ours";
        std::fs::write(
            paths::SESSIONS_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "sessionId": sid,
                "pid": pid,
                "cwd": "/tmp",
                "kind": "tmux",
                "startedAt": 1.0,
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            paths::STATE_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "our_sid": our_sid,
                "timestamp": now(),
                "last_event": "Stop",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            &*paths::GROUPS_FILE,
            format!(r#"{{"AAA": ["other-a"], "BBB": ["other-b"], "Target": ["{sid}"]}}"#),
        )
        .unwrap();
        let stub_body = format!(
            "#!/bin/sh\necho 'ciu-{our_sid}|0|/tmp'\n",
            our_sid = our_sid,
        );
        let bin = write_stub_tmux(&h, &stub_body);
        let cfg = TmuxConfig {
            tmux_bin: bin.clone(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        let _ = std::fs::remove_file(&bin);
        let found = items
            .iter()
            .find(|i| i.session_id == sid && i.alive)
            .expect("expected live instance");
        assert_eq!(found.group.as_deref(), Some("Target"));
    }

    #[test]
    fn gather_instances_orphan_resolves_group_membership() {
        use crate::infra::tmux::command::test_stub::write_stub_tmux;
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        let sid = "orph-grp-sid";
        std::fs::write(
            paths::STATE_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "our_sid": "orph-grp-ours",
                "timestamp": now() - 10.0,
                "last_event": "Stop",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            &*paths::GROUPS_FILE,
            format!(r#"{{"Beta": ["{sid}"]}}"#),
        )
        .unwrap();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let cfg = TmuxConfig {
            tmux_bin: bin.clone(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        let _ = std::fs::remove_file(&bin);
        let found = items
            .iter()
            .find(|i| i.session_id == sid)
            .expect("expected orphan instance");
        assert_eq!(found.group.as_deref(), Some("Beta"));
    }

    #[test]
    fn gather_instances_orphan_group_iteration_skips_non_matching_group() {
        use crate::infra::tmux::command::test_stub::write_stub_tmux;
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let _ = std::fs::create_dir_all(&*paths::APP_DIR);
        let sid = "orph-multi-sid";
        std::fs::write(
            paths::STATE_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "our_sid": "orph-multi-ours",
                "timestamp": now() - 10.0,
                "last_event": "Stop",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            &*paths::GROUPS_FILE,
            format!(r#"{{"Skip1": ["notme1"], "Skip2": ["notme2"], "Hit": ["{sid}"]}}"#),
        )
        .unwrap();
        let bin = write_stub_tmux(&h, "#!/bin/sh\nexit 0\n");
        let cfg = TmuxConfig {
            tmux_bin: bin.clone(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache { at: f64::MAX, map: HashMap::new() });
        let items = gather_instances(&cfg, &ps);
        let _ = std::fs::remove_file(&bin);
        let found = items
            .iter()
            .find(|i| i.session_id == sid)
            .expect("expected orphan instance");
        assert_eq!(found.group.as_deref(), Some("Hit"));
    }

    fn seed_instance(state: &Arc<AppState>, inst: InstanceData) {
        let mut c = state.instances_cache.lock().unwrap();
        c.at = f64::MAX;
        c.data.push(inst);
    }

    #[tokio::test]
    async fn api_ack_records_timestamp_and_returns_it() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        std::fs::create_dir_all(&*paths::APP_DIR).unwrap();
        let resp = api_ack(
            axum::extract::Path("sid-ack1".into()),
            axum::Json(AckBody { timestamp: 42.0 }),
        )
        .await;
        assert_eq!(
            resp.0.get("ack_timestamp").and_then(Value::as_f64),
            Some(42.0)
        );
    }

    #[tokio::test]
    async fn api_ack_keeps_higher_existing_timestamp() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        std::fs::create_dir_all(&*paths::APP_DIR).unwrap();
        let _ = api_ack(
            axum::extract::Path("sid-ack2".into()),
            axum::Json(AckBody { timestamp: 100.0 }),
        )
        .await;
        let resp = api_ack(
            axum::extract::Path("sid-ack2".into()),
            axum::Json(AckBody { timestamp: 50.0 }),
        )
        .await;
        assert_eq!(
            resp.0.get("ack_timestamp").and_then(Value::as_f64),
            Some(100.0)
        );
    }

    #[tokio::test]
    async fn api_input_returns_not_found_when_session_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let err = api_input(
            State(state),
            axum::extract::Path("missing".into()),
            axum::Json(InputBody {
                text: "x".into(),
                submit: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_input_returns_bad_request_when_not_tmux_owned() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-x", None));
        let err = api_input(
            State(state),
            axum::extract::Path("sid-x".into()),
            axum::Json(InputBody {
                text: "x".into(),
                submit: Some(false),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_input_happy_path_runs_without_error() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-i1", Some("o1")));
        let resp = api_input(
            State(state),
            axum::extract::Path("sid-i1".into()),
            axum::Json(InputBody {
                text: "hi".into(),
                submit: Some(false),
            }),
        )
        .await
        .unwrap();
        assert!(resp.0.ok);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_kill_returns_not_found_when_session_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let err = api_kill(State(state), axum::extract::Path("missing".into()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_kill_returns_bad_request_when_not_tmux_owned() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-k1", None));
        let err = api_kill(State(state), axum::extract::Path("sid-k1".into()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_kill_happy_path_invalidates_cache() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-k2", Some("o2")));
        state.instances_cache.lock().unwrap().at = now();
        let resp = api_kill(State(state.clone()), axum::extract::Path("sid-k2".into()))
            .await
            .unwrap();
        assert!(resp.0.ok);
        assert_eq!(state.instances_cache.lock().unwrap().at, 0.0);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_open_terminal_returns_not_found_when_session_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let err = api_open_terminal(State(state), axum::extract::Path("missing".into()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_open_terminal_returns_bad_request_when_not_tmux_owned() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-o1", None));
        let err = api_open_terminal(State(state), axum::extract::Path("sid-o1".into()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_open_terminal_returns_not_found_when_tmux_session_gone() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 1\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-o2", Some("o2")));
        let err = api_open_terminal(State(state), axum::extract::Path("sid-o2".into()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_recent_cwds_empty_when_no_state_and_no_projects_dir() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let resp = api_recent_cwds(State(state)).await;
        let cwds = resp.get("cwds").and_then(Value::as_array).unwrap();
        assert_eq!(cwds.len(), 0);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_recent_cwds_dedupes_and_sorts_by_recency() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        {
            let mut c = state.instances_cache.lock().unwrap();
            c.at = now();
            let mut older = blank_instance("s-old", None);
            older.cwd = Some("/w/older".into());
            older.hook_timestamp = Some(100.0);
            let mut newer = blank_instance("s-new", None);
            newer.cwd = Some("/w/newer".into());
            newer.hook_timestamp = Some(200.0);
            let mut dup = blank_instance("s-dup", None);
            dup.cwd = Some("/w/older".into());
            dup.hook_timestamp = Some(50.0);
            c.data = vec![older, newer, dup];
        }
        let resp = api_recent_cwds(State(state)).await;
        let cwds: Vec<String> = resp
            .get("cwds")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let idx_new = cwds.iter().position(|c| c == "/w/newer").unwrap();
        let idx_old = cwds.iter().position(|c| c == "/w/older").unwrap();
        assert!(idx_new < idx_old);
        assert_eq!(cwds.iter().filter(|c| *c == "/w/older").count(), 1);
        let _ = std::fs::remove_file(&bin);
    }

    fn install_open_stub(home: &std::path::Path) -> (std::path::PathBuf, Option<String>) {
        use std::os::unix::fs::PermissionsExt;
        let stub_dir = home.join("open-stub");
        let _ = std::fs::create_dir_all(&stub_dir);
        let stub = stub_dir.join("open");
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&stub).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).unwrap();
        let old = std::env::var("PATH").ok();
        std::env::set_var("PATH", stub_dir.to_string_lossy().to_string());
        (stub_dir, old)
    }

    fn restore_path(old: Option<String>) {
        match old {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
    }

    #[tokio::test]
    async fn api_open_dashboard_sets_pending_focus_when_sid_present() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let (_stub_dir, old_path) = install_open_stub(&paths::HOME);
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        std::env::set_var("CIU_HOST", "127.0.0.1");
        std::env::set_var("CIU_PORT", "65000");
        let _ = api_open_dashboard(
            State(state.clone()),
            axum::Json(OpenDashboardBody {
                sid: Some("focused-sid".into()),
            }),
        )
        .await;
        std::env::remove_var("CIU_HOST");
        std::env::remove_var("CIU_PORT");
        restore_path(old_path);
        assert_eq!(
            state.pending_focus.lock().unwrap().sid.as_deref(),
            Some("focused-sid")
        );
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_open_dashboard_without_sid_leaves_pending_focus_clear() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let (_stub_dir, old_path) = install_open_stub(&paths::HOME);
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        std::env::set_var("CIU_HOST", "127.0.0.1");
        std::env::set_var("CIU_PORT", "65001");
        let _ = api_open_dashboard(
            State(state.clone()),
            axum::Json(OpenDashboardBody { sid: None }),
        )
        .await;
        std::env::remove_var("CIU_HOST");
        std::env::remove_var("CIU_PORT");
        restore_path(old_path);
        assert!(state.pending_focus.lock().unwrap().sid.is_none());
        let _ = std::fs::remove_file(&bin);
    }

    fn install_multi_stubs(home: &std::path::Path, scripts: &[(&str, &str)]) -> (std::path::PathBuf, Option<String>) {
        use std::os::unix::fs::PermissionsExt;
        let stub_dir = home.join("multi-stub");
        let _ = std::fs::create_dir_all(&stub_dir);
        for (name, body) in scripts {
            let stub = stub_dir.join(name);
            std::fs::write(&stub, body).unwrap();
            let mut perms = std::fs::metadata(&stub).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&stub, perms).unwrap();
        }
        let old = std::env::var("PATH").ok();
        std::env::set_var("PATH", stub_dir.to_string_lossy().to_string());
        (stub_dir, old)
    }

    #[tokio::test]
    async fn api_signal_happy_path_with_our_sid_returns_ok() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-sig", Some("ours-sig")));
        let resp = api_signal(
            State(state),
            Path("sid-sig".into()),
            Json(SignalBody {
                signal: Some("TERM".into()),
            }),
        )
        .await
        .unwrap();
        assert!(resp.0.ok);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_signal_default_signal_when_none_supplied() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-sig2", Some("ours-sig2")));
        let resp = api_signal(
            State(state),
            Path("sid-sig2".into()),
            Json(SignalBody { signal: None }),
        )
        .await
        .unwrap();
        assert!(resp.0.ok);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_input_submit_path_with_short_text_uses_100ms_settle() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-is", Some("ours-is")));
        let resp = api_input(
            State(state),
            Path("sid-is".into()),
            Json(InputBody {
                text: "hi".into(),
                submit: Some(true),
            }),
        )
        .await
        .unwrap();
        assert!(resp.0.ok);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_input_submit_path_with_long_text_uses_300ms_settle() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-il", Some("ours-il")));
        let resp = api_input(
            State(state),
            Path("sid-il".into()),
            Json(InputBody {
                text: "a".repeat(60),
                submit: Some(true),
            }),
        )
        .await
        .unwrap();
        assert!(resp.0.ok);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_input_submit_default_true_sends_enter() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-id", Some("ours-id")));
        let resp = api_input(
            State(state),
            Path("sid-id".into()),
            Json(InputBody {
                text: String::new(),
                submit: None,
            }),
        )
        .await
        .unwrap();
        assert!(resp.0.ok);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_open_terminal_happy_path_with_stubbed_osascript() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-ot", Some("ours-ot")));
        let (_stub_dir, old_path) = install_multi_stubs(
            &paths::HOME,
            &[("osascript", "#!/bin/sh\nexit 0\n")],
        );
        let resp = api_open_terminal(State(state), Path("sid-ot".into()))
            .await
            .unwrap();
        restore_path(old_path);
        assert!(resp.0.ok);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_recent_cwds_includes_projects_dir_entries() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        std::fs::create_dir_all(&*paths::PROJECTS_DIR).unwrap();
        let target_path = format!("/tmp/aimrctdir{}", std::process::id());
        std::fs::create_dir_all(&target_path).unwrap();
        let slug = format!(
            "-{}",
            target_path
                .replace('/', "-")
                .trim_start_matches('-')
        );
        std::fs::create_dir_all(paths::PROJECTS_DIR.join(&slug)).unwrap();
        std::fs::create_dir_all(paths::PROJECTS_DIR.join("no-leading-dash")).unwrap();
        let nonexistent_slug = "-nope-does-not-exist";
        std::fs::create_dir_all(paths::PROJECTS_DIR.join(nonexistent_slug)).unwrap();
        std::fs::write(paths::PROJECTS_DIR.join("loose-file.txt"), "").unwrap();
        let resp = api_recent_cwds(State(state)).await;
        let cwds: Vec<String> = resp
            .get("cwds")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(cwds.iter().any(|c| c == &target_path), "cwds = {cwds:?}");
        let _ = std::fs::remove_dir_all(&target_path);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_new_instance_mcp_source_uses_provided_file() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        std::fs::create_dir_all(h.join(".claude")).unwrap();
        let src = h.join("custom-mcp.json");
        std::fs::write(
            &src,
            r#"{"mcpServers": {"custom-server": {"command": "x"}}}"#,
        )
        .unwrap();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let resp = api_new_instance(
            State(state),
            Json(NewInstanceBody {
                cwd: h.to_string_lossy().to_string(),
                command: None,
                mcps: Some(vec!["custom-server".into()]),
                mcp_source: Some(src.to_string_lossy().to_string()),
                name: None,
                group: None,
            }),
        )
        .await
        .unwrap();
        assert!(resp.get("our_sid").and_then(|v| v.as_str()).is_some());
        let mut found = false;
        if let Ok(rd) = std::fs::read_dir(&*paths::MCP_CONFIG_DIR) {
            for entry in rd.flatten() {
                let data = read_json(&entry.path());
                if data.pointer("/mcpServers/custom-server").is_some() {
                    found = true;
                }
            }
        }
        assert!(found);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_new_instance_empty_name_and_group_skip_stash() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let resp = api_new_instance(
            State(state),
            Json(NewInstanceBody {
                cwd: paths::HOME.to_string_lossy().to_string(),
                command: None,
                mcps: None,
                mcp_source: None,
                name: Some("   ".into()),
                group: Some("".into()),
            }),
        )
        .await
        .unwrap();
        let osid = resp.get("our_sid").and_then(|v| v.as_str()).unwrap();
        let pn = read_json(&*paths::PENDING_NAMES_FILE);
        let pg = read_json(&*paths::PENDING_GROUPS_FILE);
        assert!(pn.get(osid).is_none());
        assert!(pg.get(osid).is_none());
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_new_instance_resume_command_suppresses_strict_flag() {
        let _g = LOCK.lock().unwrap();
        let h = set_test_home();
        wipe_dirs();
        std::fs::create_dir_all(h.join(".claude")).unwrap();
        std::fs::write(
            h.join(".claude.json"),
            r#"{"mcpServers": {"srvR": {"command": "x"}}}"#,
        )
        .unwrap();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let resp = api_new_instance(
            State(state),
            Json(NewInstanceBody {
                cwd: h.to_string_lossy().to_string(),
                command: Some("claude --resume abc".into()),
                mcps: Some(vec!["srvR".into()]),
                mcp_source: None,
                name: None,
                group: None,
            }),
        )
        .await
        .unwrap();
        assert!(resp.get("our_sid").and_then(|v| v.as_str()).is_some());
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_open_dashboard_focused_existing_window_short_circuits() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let (_stub_dir, old_path) = install_multi_stubs(
            &paths::HOME,
            &[
                ("osascript", "#!/bin/sh\nprintf focused\n"),
                ("open", "#!/bin/sh\nexit 0\n"),
            ],
        );
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        std::env::set_var("CIU_HOST", "127.0.0.1");
        std::env::set_var("CIU_PORT", "65099");
        let resp = api_open_dashboard(
            State(state),
            Json(OpenDashboardBody {
                sid: Some("abc".into()),
            }),
        )
        .await;
        std::env::remove_var("CIU_HOST");
        std::env::remove_var("CIU_PORT");
        restore_path(old_path);
        assert_eq!(resp.get("result").and_then(|v| v.as_str()), Some("focused"));
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn try_focus_existing_window_safari_fallback_when_others_fail() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let script = r#"#!/bin/sh
case "$2" in
  *Safari*) printf focused ;;
  *) printf nf ;;
esac
"#;
        let (_stub_dir, old_path) = install_multi_stubs(
            &paths::HOME,
            &[("osascript", script)],
        );
        let result = try_focus_existing_window("127.0.0.1:65098");
        restore_path(old_path);
        assert_eq!(result.as_deref(), Some("Safari"));
    }

    #[tokio::test]
    async fn try_focus_existing_window_returns_none_when_all_fail() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let (_stub_dir, old_path) = install_multi_stubs(
            &paths::HOME,
            &[("osascript", "#!/bin/sh\nprintf nf\n")],
        );
        let result = try_focus_existing_window("127.0.0.1:65097");
        restore_path(old_path);
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn api_upload_happy_path_writes_file() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode as SC};
        use axum::routing::post;
        use axum::Router;
        use tower::ServiceExt;
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-up", Some("ours-up")));
        let app = Router::new()
            .route("/api/upload/{session_id}", post(api_upload))
            .with_state(state);
        let boundary = "----B";
        let body = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"test!@#.txt\"\r\n\r\nhello\r\n--{b}--\r\n",
            b = boundary
        );
        let req = Request::builder()
            .method("POST")
            .uri("/api/upload/sid-up")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), SC::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body_bytes).unwrap();
        let path_str = v.get("path").and_then(|v| v.as_str()).unwrap();
        assert!(PathBuf::from(path_str).exists());
        assert_eq!(v.get("name").and_then(|v| v.as_str()), Some("test!@#.txt"));
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_upload_rejects_file_too_large() {
        use axum::body::Body;
        use axum::extract::DefaultBodyLimit;
        use axum::http::{Request, StatusCode as SC};
        use axum::routing::post;
        use axum::Router;
        use tower::ServiceExt;
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-big", Some("ours-big")));
        let app = Router::new()
            .route("/api/upload/{session_id}", post(api_upload))
            .layer(DefaultBodyLimit::disable())
            .with_state(state);
        let boundary = "----B";
        let big = "a".repeat(26 * 1024 * 1024);
        let body = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.bin\"\r\n\r\n{big}\r\n--{b}--\r\n",
            b = boundary,
            big = big
        );
        let req = Request::builder()
            .method("POST")
            .uri("/api/upload/sid-big")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), SC::PAYLOAD_TOO_LARGE);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_upload_no_file_returns_bad_request() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode as SC};
        use axum::routing::post;
        use axum::Router;
        use tower::ServiceExt;
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-nf", Some("ours-nf")));
        let app = Router::new()
            .route("/api/upload/{session_id}", post(api_upload))
            .with_state(state);
        let boundary = "----B";
        let body = format!("--{b}--\r\n", b = boundary);
        let req = Request::builder()
            .method("POST")
            .uri("/api/upload/sid-nf")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), SC::BAD_REQUEST);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_upload_404_when_session_missing() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode as SC};
        use axum::routing::post;
        use axum::Router;
        use tower::ServiceExt;
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 0\n");
        let state = fresh_state(bin.clone());
        let app = Router::new()
            .route("/api/upload/{session_id}", post(api_upload))
            .with_state(state);
        let boundary = "----B";
        let body = format!("--{b}--\r\n", b = boundary);
        let req = Request::builder()
            .method("POST")
            .uri("/api/upload/missing")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), SC::NOT_FOUND);
        let _ = std::fs::remove_file(&bin);
    }

    #[tokio::test]
    async fn api_kill_kill_failure_does_not_poison_request() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let bin = write_stub_tmux(&paths::HOME, "#!/bin/sh\nexit 1\n");
        let state = fresh_state(bin.clone());
        seed_instance(&state, blank_instance("sid-kf", Some("ours-kf")));
        let resp = api_kill(State(state), Path("sid-kf".into()))
            .await
            .unwrap();
        assert!(resp.0.ok);
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn resolve_status_notification_approval_returns_message_as_label() {
        let hs = json!({
            "last_event": "Notification",
            "timestamp": now(),
            "notification_message": "please approve this tool",
        });
        let tail = json!({"last_timestamp_epoch": 0.0});
        let (status, label) = resolve_status(&hs, true, &tail);
        assert_eq!(status, "needs_input");
        assert_eq!(label.unwrap(), "please approve this tool");
    }

    #[test]
    fn gather_instances_headless_command_is_skipped() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        wipe_dirs();
        let _ = std::fs::create_dir_all(&*paths::SESSIONS_DIR);
        let _ = std::fs::create_dir_all(&*paths::STATE_DIR);
        let sid = "headless-sid";
        let pid = std::process::id();
        std::fs::write(
            paths::SESSIONS_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "sessionId": sid,
                "pid": pid,
                "cwd": "/tmp",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            paths::STATE_DIR.join(format!("{sid}.json")),
            serde_json::to_string(&json!({
                "our_sid": "ours-h",
                "timestamp": now(),
            }))
            .unwrap(),
        )
        .unwrap();
        let cfg = TmuxConfig {
            tmux_bin: "/no/such/bin".into(),
            socket_name: "ciu-t".into(),
            name_prefix: "ciu-".into(),
        };
        let ps = Mutex::new(PsCache {
            at: f64::MAX,
            map: {
                let mut m = HashMap::new();
                m.insert(
                    pid,
                    PsRow {
                        ppid: 1,
                        tty: "t".into(),
                        command: "claude -p headless-prompt".into(),
                    },
                );
                m
            },
        });
        let items = gather_instances(&cfg, &ps);
        let live_match = items
            .iter()
            .find(|i| i.session_id == sid && i.alive);
        assert!(live_match.is_none(), "headless session should not appear as live");
    }
}
