use crate::native::{notifications, sound, tray};
use crate::services::instances::AppState;
use crate::services::summarizer::Summarizer;
use crate::services::transcript::jsonl_summary;
use crate::http::dto::InstanceData;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::AppHandle;

struct InstanceState {
    status: String,
}

fn count_statuses(instances: &[InstanceData]) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for inst in instances {
        *counts.entry(inst.status.clone()).or_default() += 1;
    }
    counts
}

fn has_unacked_reply(instances: &[InstanceData]) -> bool {
    instances
        .iter()
        .any(|i| i.status == "idle" && i.hook_timestamp.unwrap_or(0.0) > i.ack_timestamp)
}

fn compute_slots(counts: &HashMap<String, usize>, unacked_reply: bool) -> tray::TraySlots {
    let needs_input = counts.get("needs_input").copied().unwrap_or(0);
    let running = counts.get("running").copied().unwrap_or(0);
    tray::TraySlots {
        any_running: running > 0,
        any_needs_input: needs_input > 0,
        any_idle: unacked_reply,
    }
}

fn format_tooltip(counts: &HashMap<String, usize>) -> String {
    let mut parts: Vec<String> = counts
        .iter()
        .map(|(status, count)| format!("{count} {status}"))
        .collect();
    parts.sort();
    if parts.is_empty() {
        "AIM — no instances".to_string()
    } else {
        parts.join(" · ")
    }
}

fn display_name(inst: &InstanceData) -> String {
    inst.custom_name
        .as_deref()
        .or(inst.title.as_deref())
        .unwrap_or(&inst.name)
        .to_string()
}

#[derive(Debug, PartialEq)]
enum Transition {
    NeedsInput,
    ReplyFinished,
    None,
}

fn classify_transition(old_status: &str, new_status: &str) -> Transition {
    match new_status {
        "needs_input" => Transition::NeedsInput,
        "idle" if old_status == "running" => Transition::ReplyFinished,
        _ => Transition::None,
    }
}

pub async fn run(
    app: AppHandle,
    state: Arc<AppState>,
    summarizer: Arc<Summarizer>,
) {
    let mut prev: HashMap<String, InstanceState> = HashMap::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));

    loop {
        interval.tick().await;

        let instances = state.cached_instances();

        let counts = count_statuses(&instances);
        let unacked_reply = has_unacked_reply(&instances);
        let slots = compute_slots(&counts, unacked_reply);
        let tooltip = format_tooltip(&counts);

        tray::set_state(&app, slots, &tooltip);

        let settings = crate::services::instances::read_json(&crate::infra::paths::SETTINGS_FILE);
        let sound_enabled = settings
            .get("sound")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);

        for inst in &instances {
            let sid = &inst.session_id;
            let old = prev.get(sid);

            let status_changed = old.is_none()
                || old.is_some_and(|o| o.status != inst.status);

            if status_changed {
                if let Some(old) = old {
                    match classify_transition(&old.status, &inst.status) {
                        Transition::NeedsInput => {
                            let name = display_name(inst);
                            let title = format!("🟠 {name}");
                            let body = inst
                                .notification_message
                                .as_deref()
                                .unwrap_or("Needs input");
                            {
                                let mut pf = state.pending_focus.lock().unwrap();
                                pf.sid = Some(sid.clone());
                                pf.ts = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs_f64();
                            }
                            notifications::send(&app, &title, body, Some(sid));
                            if sound_enabled {
                                sound::play_glass();
                            }
                        }
                        Transition::ReplyFinished => {
                            let name = display_name(inst);
                            let title = format!("🟢 {name}");
                            notifications::send(&app, &title, "Reply finished", Some(sid));
                            if sound_enabled {
                                sound::play_funk();
                            }
                        }
                        Transition::None => {}
                    }
                }
            }

            if inst.alive {
                let summary_ctx = jsonl_summary(&inst.session_id, inst.cwd.as_deref());
                summarizer.request(&inst.session_id, &summary_ctx);
            }
        }

        prev = instances
            .into_iter()
            .map(|i| {
                let sid = i.session_id.clone();
                (
                    sid,
                    InstanceState {
                        status: i.status,
                    },
                )
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inst(sid: &str, status: &str) -> InstanceData {
        InstanceData {
            session_id: sid.into(),
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
            status: status.into(),
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
            our_sid: None,
            tmux_session: None,
        }
    }

    #[test]
    fn count_statuses_tallies_per_status() {
        let instances = vec![
            inst("a", "idle"),
            inst("b", "idle"),
            inst("c", "running"),
            inst("d", "needs_input"),
        ];
        let c = count_statuses(&instances);
        assert_eq!(c.get("idle"), Some(&2));
        assert_eq!(c.get("running"), Some(&1));
        assert_eq!(c.get("needs_input"), Some(&1));
    }

    #[test]
    fn count_statuses_empty_input_returns_empty_map() {
        assert!(count_statuses(&[]).is_empty());
    }

    #[test]
    fn has_unacked_reply_true_when_idle_with_newer_hook() {
        let mut i = inst("a", "idle");
        i.hook_timestamp = Some(10.0);
        i.ack_timestamp = 5.0;
        assert!(has_unacked_reply(&[i]));
    }

    #[test]
    fn has_unacked_reply_false_when_hook_equal_or_older() {
        let mut i = inst("a", "idle");
        i.hook_timestamp = Some(5.0);
        i.ack_timestamp = 5.0;
        assert!(!has_unacked_reply(&[i]));
    }

    #[test]
    fn has_unacked_reply_false_when_status_not_idle() {
        let mut i = inst("a", "running");
        i.hook_timestamp = Some(10.0);
        i.ack_timestamp = 0.0;
        assert!(!has_unacked_reply(&[i]));
    }

    #[test]
    fn has_unacked_reply_false_when_hook_timestamp_absent() {
        let mut i = inst("a", "idle");
        i.hook_timestamp = None;
        i.ack_timestamp = 0.0;
        assert!(!has_unacked_reply(&[i]));
    }

    #[test]
    fn compute_slots_all_false_when_counts_empty_and_no_idle() {
        let c = HashMap::new();
        let s = compute_slots(&c, false);
        assert!(!s.any_running && !s.any_needs_input && !s.any_idle);
    }

    #[test]
    fn compute_slots_any_running_when_running_positive() {
        let mut c = HashMap::new();
        c.insert("running".to_string(), 1);
        let s = compute_slots(&c, false);
        assert!(s.any_running && !s.any_needs_input && !s.any_idle);
    }

    #[test]
    fn compute_slots_any_needs_input_when_needs_input_positive() {
        let mut c = HashMap::new();
        c.insert("needs_input".to_string(), 2);
        let s = compute_slots(&c, false);
        assert!(!s.any_running && s.any_needs_input && !s.any_idle);
    }

    #[test]
    fn compute_slots_uses_unacked_reply_flag_for_idle() {
        let c = HashMap::new();
        let s = compute_slots(&c, true);
        assert!(s.any_idle);
    }

    #[test]
    fn format_tooltip_empty_counts_yields_fallback() {
        assert_eq!(format_tooltip(&HashMap::new()), "AIM — no instances");
    }

    #[test]
    fn format_tooltip_single_entry() {
        let mut c = HashMap::new();
        c.insert("idle".to_string(), 3);
        assert_eq!(format_tooltip(&c), "3 idle");
    }

    #[test]
    fn format_tooltip_multi_entries_sorted_joined_by_middot() {
        let mut c = HashMap::new();
        c.insert("idle".to_string(), 2);
        c.insert("running".to_string(), 1);
        let out = format_tooltip(&c);
        assert!(out.contains("2 idle"));
        assert!(out.contains("1 running"));
        assert!(out.contains(" · "));
    }

    #[test]
    fn display_name_uses_custom_name_first() {
        let mut i = inst("s", "idle");
        i.name = "fallback-name".into();
        i.title = Some("title".into());
        i.custom_name = Some("custom".into());
        assert_eq!(display_name(&i), "custom");
    }

    #[test]
    fn display_name_falls_back_to_title_when_no_custom_name() {
        let mut i = inst("s", "idle");
        i.name = "fallback-name".into();
        i.title = Some("title".into());
        i.custom_name = None;
        assert_eq!(display_name(&i), "title");
    }

    #[test]
    fn display_name_falls_back_to_name_when_no_custom_or_title() {
        let mut i = inst("s", "idle");
        i.name = "just-name".into();
        i.title = None;
        i.custom_name = None;
        assert_eq!(display_name(&i), "just-name");
    }

    #[test]
    fn classify_transition_needs_input_from_anything() {
        assert_eq!(
            classify_transition("running", "needs_input"),
            Transition::NeedsInput
        );
        assert_eq!(
            classify_transition("idle", "needs_input"),
            Transition::NeedsInput
        );
    }

    #[test]
    fn classify_transition_reply_finished_only_from_running_to_idle() {
        assert_eq!(
            classify_transition("running", "idle"),
            Transition::ReplyFinished
        );
        assert_eq!(
            classify_transition("needs_input", "idle"),
            Transition::None
        );
    }

    #[test]
    fn classify_transition_none_for_same_status() {
        assert_eq!(classify_transition("idle", "idle"), Transition::None);
    }

    #[test]
    fn instance_state_struct_is_constructible() {
        let s = InstanceState { status: "idle".into() };
        assert_eq!(s.status, "idle");
    }
}
