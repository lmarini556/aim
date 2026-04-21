use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameBody {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupBody {
    pub group: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalBody {
    #[serde(default = "default_signal")]
    pub signal: Option<String>,
}

fn default_signal() -> Option<String> {
    Some("TERM".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewInstanceBody {
    pub cwd: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub mcps: Option<Vec<String>>,
    #[serde(default)]
    pub mcp_source: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputBody {
    pub text: String,
    #[serde(default = "default_submit")]
    pub submit: Option<bool>,
}

fn default_submit() -> Option<bool> {
    Some(true)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckBody {
    pub timestamp: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigFileBody {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigWriteBody {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCreateBody {
    pub scope: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDeleteBody {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpListBody {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenDashboardBody {
    #[serde(default)]
    pub sid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsBody {
    #[serde(default)]
    pub sound: Option<bool>,
    #[serde(default)]
    pub banner_ttl: Option<u32>,
    #[serde(default)]
    pub poll_interval: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceData {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub alive: bool,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    pub command: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notification_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_timestamp: Option<f64>,
    pub transcript: serde_json::Value,
    pub summary: serde_json::Value,
    pub mcps: serde_json::Value,
    pub subagents: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub ack_timestamp: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub our_sid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstancesResponse {
    pub instances: Vec<InstanceData>,
    pub served_at: f64,
    pub server_start: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_focus: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(rename = "type")]
    pub entry_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub parts: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptResponse {
    pub session: serde_json::Value,
    pub entries: Vec<TranscriptEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OkResponse {
    pub ok: bool,
}

impl OkResponse {
    pub fn new() -> Self {
        Self { ok: true }
    }
}
