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

pub use crate::domain::instance::InstanceData;
pub use crate::domain::transcript::TranscriptEntry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstancesResponse {
    pub instances: Vec<InstanceData>,
    pub served_at: f64,
    pub server_start: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_focus: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rt<T>(v: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let s = serde_json::to_string(v).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn rename_body_round_trip() {
        let b = RenameBody { name: "x".into() };
        assert_eq!(rt(&b).name, "x");
    }

    #[test]
    fn rename_body_debug_and_clone() {
        let b = RenameBody { name: "y".into() };
        let _ = b.clone();
        assert!(format!("{b:?}").contains("RenameBody"));
    }

    #[test]
    fn group_body_some() {
        let b: GroupBody = serde_json::from_value(json!({"group": "alpha"})).unwrap();
        assert_eq!(b.group.as_deref(), Some("alpha"));
    }

    #[test]
    fn group_body_null() {
        let b: GroupBody = serde_json::from_value(json!({"group": null})).unwrap();
        assert!(b.group.is_none());
    }

    #[test]
    fn signal_body_default_is_term() {
        let b: SignalBody = serde_json::from_value(json!({})).unwrap();
        assert_eq!(b.signal.as_deref(), Some("TERM"));
    }

    #[test]
    fn signal_body_explicit() {
        let b: SignalBody = serde_json::from_value(json!({"signal": "KILL"})).unwrap();
        assert_eq!(b.signal.as_deref(), Some("KILL"));
    }

    #[test]
    fn new_instance_body_minimum_fields() {
        let b: NewInstanceBody = serde_json::from_value(json!({"cwd": "/tmp"})).unwrap();
        assert_eq!(b.cwd, "/tmp");
        assert!(b.command.is_none());
        assert!(b.mcps.is_none());
        assert!(b.mcp_source.is_none());
        assert!(b.name.is_none());
        assert!(b.group.is_none());
    }

    #[test]
    fn new_instance_body_all_fields() {
        let b: NewInstanceBody = serde_json::from_value(json!({
            "cwd": "/x", "command": "cmd", "mcps": ["a", "b"],
            "mcp_source": "s", "name": "n", "group": "g"
        })).unwrap();
        assert_eq!(b.mcps.unwrap(), vec!["a", "b"]);
        assert_eq!(b.name.as_deref(), Some("n"));
    }

    #[test]
    fn input_body_default_submit_true() {
        let b: InputBody = serde_json::from_value(json!({"text": "hi"})).unwrap();
        assert_eq!(b.submit, Some(true));
    }

    #[test]
    fn input_body_submit_false() {
        let b: InputBody = serde_json::from_value(json!({"text": "hi", "submit": false})).unwrap();
        assert_eq!(b.submit, Some(false));
    }

    #[test]
    fn ack_body_round_trip() {
        let b = AckBody { timestamp: 123.45 };
        assert!((rt(&b).timestamp - 123.45).abs() < 1e-9);
    }

    #[test]
    fn config_file_body_round_trip() {
        let b: ConfigFileBody = serde_json::from_value(json!({"path": "/a"})).unwrap();
        assert_eq!(b.path, "/a");
    }

    #[test]
    fn config_write_body_fields() {
        let b: ConfigWriteBody = serde_json::from_value(json!({
            "path": "/a.md", "content": "body"
        })).unwrap();
        assert_eq!(b.path, "/a.md");
        assert_eq!(b.content, "body");
    }

    #[test]
    fn skill_create_body_fields() {
        let b: SkillCreateBody = serde_json::from_value(json!({
            "scope": "global", "name": "mytool"
        })).unwrap();
        assert_eq!(b.scope, "global");
        assert_eq!(b.name, "mytool");
    }

    #[test]
    fn skill_delete_body_field() {
        let b: SkillDeleteBody = serde_json::from_value(json!({"path": "/p"})).unwrap();
        assert_eq!(b.path, "/p");
    }

    #[test]
    fn mcp_list_body_field() {
        let b: McpListBody = serde_json::from_value(json!({"path": "/mcp"})).unwrap();
        assert_eq!(b.path, "/mcp");
    }

    #[test]
    fn open_dashboard_body_with_sid() {
        let b: OpenDashboardBody = serde_json::from_value(json!({"sid": "abc"})).unwrap();
        assert_eq!(b.sid.as_deref(), Some("abc"));
    }

    #[test]
    fn open_dashboard_body_no_sid() {
        let b: OpenDashboardBody = serde_json::from_value(json!({})).unwrap();
        assert!(b.sid.is_none());
    }

    #[test]
    fn settings_body_all_optional() {
        let b: SettingsBody = serde_json::from_value(json!({})).unwrap();
        assert!(b.sound.is_none());
        assert!(b.banner_ttl.is_none());
        assert!(b.poll_interval.is_none());
    }

    #[test]
    fn settings_body_full() {
        let b: SettingsBody = serde_json::from_value(json!({
            "sound": true, "banner_ttl": 5, "poll_interval": 1000
        })).unwrap();
        assert_eq!(b.sound, Some(true));
        assert_eq!(b.banner_ttl, Some(5));
        assert_eq!(b.poll_interval, Some(1000));
    }

    #[test]
    fn instance_data_serializes_skipping_none() {
        let d = InstanceData {
            session_id: "s".into(),
            pid: None,
            alive: true,
            name: "n".into(),
            title: None,
            custom_name: None,
            first_user: None,
            cwd: None,
            kind: None,
            started_at: None,
            command: "c".into(),
            status: "idle".into(),
            last_event: None,
            last_tool: None,
            notification_message: None,
            hook_timestamp: None,
            transcript: json!(null),
            summary: json!(null),
            mcps: json!([]),
            subagents: vec![],
            group: None,
            ack_timestamp: 0.0,
            our_sid: None,
            tmux_session: None,
        };
        let v = serde_json::to_value(&d).unwrap();
        assert!(v.get("pid").is_none());
        assert!(v.get("title").is_none());
        assert!(v.get("our_sid").is_none());
        assert_eq!(v.get("session_id").and_then(|x| x.as_str()), Some("s"));
    }

    #[test]
    fn instance_data_round_trip_full() {
        let d = InstanceData {
            session_id: "sid".into(),
            pid: Some(99),
            alive: true,
            name: "n".into(),
            title: Some("t".into()),
            custom_name: Some("cn".into()),
            first_user: Some("fu".into()),
            cwd: Some("/cwd".into()),
            kind: Some("k".into()),
            started_at: Some(1.0),
            command: "c".into(),
            status: "idle".into(),
            last_event: Some("PostToolUse".into()),
            last_tool: Some("Bash".into()),
            notification_message: Some("msg".into()),
            hook_timestamp: Some(2.0),
            transcript: json!({}),
            summary: json!({}),
            mcps: json!([]),
            subagents: vec![json!({"name": "sa"})],
            group: Some("g".into()),
            ack_timestamp: 3.0,
            our_sid: Some("os".into()),
            tmux_session: Some("ts".into()),
        };
        let back: InstanceData = rt(&d);
        assert_eq!(back.pid, Some(99));
        assert_eq!(back.subagents.len(), 1);
    }

    #[test]
    fn instances_response_round_trip() {
        let r = InstancesResponse {
            instances: vec![],
            served_at: 1.0,
            server_start: 2.0,
            pending_focus: Some("pf".into()),
        };
        let back: InstancesResponse = rt(&r);
        assert_eq!(back.pending_focus.as_deref(), Some("pf"));
    }

    #[test]
    fn instances_response_pending_focus_skipped_when_none() {
        let r = InstancesResponse {
            instances: vec![],
            served_at: 1.0,
            server_start: 2.0,
            pending_focus: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("pending_focus").is_none());
    }

    #[test]
    fn transcript_entry_type_rename() {
        let e: TranscriptEntry = serde_json::from_value(json!({
            "type": "user", "parts": []
        })).unwrap();
        assert_eq!(e.entry_type, "user");
        assert!(e.uuid.is_none());
        assert!(e.timestamp.is_none());
    }

    #[test]
    fn transcript_entry_serializes_with_type_key() {
        let e = TranscriptEntry {
            uuid: Some("u".into()),
            entry_type: "assistant".into(),
            timestamp: Some("2024-01-01".into()),
            parts: vec![json!({"kind": "text"})],
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v.get("type").and_then(|x| x.as_str()), Some("assistant"));
        assert!(v.get("entry_type").is_none());
    }

    #[test]
    fn transcript_response_round_trip() {
        let r = TranscriptResponse {
            session: json!({"id": "x"}),
            entries: vec![],
        };
        let back: TranscriptResponse = rt(&r);
        assert_eq!(back.session.get("id").and_then(|x| x.as_str()), Some("x"));
    }

    #[test]
    fn ok_response_default_is_false() {
        let r = OkResponse::default();
        assert!(!r.ok);
    }

    #[test]
    fn ok_response_new_is_true() {
        let r = OkResponse::new();
        assert!(r.ok);
    }

    #[test]
    fn ok_response_round_trip() {
        let r = OkResponse { ok: true };
        let back: OkResponse = rt(&r);
        assert!(back.ok);
    }

    #[test]
    fn default_signal_helper_returns_term() {
        assert_eq!(default_signal().as_deref(), Some("TERM"));
    }

    #[test]
    fn default_submit_helper_returns_true() {
        assert_eq!(default_submit(), Some(true));
    }
}
