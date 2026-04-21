use crate::native::{notifications, sound, tray};
use crate::server::instances::AppState;
use crate::server::summarizer::Summarizer;
use crate::server::transcript::jsonl_summary;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::AppHandle;

struct InstanceState {
    status: String,
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

        let mut counts: HashMap<&str, usize> = HashMap::new();
        for inst in &instances {
            *counts.entry(inst.status.as_str()).or_default() += 1;
        }

        let needs_input = counts.get("needs_input").copied().unwrap_or(0);
        let running = counts.get("running").copied().unwrap_or(0);
        let unacked_reply = instances.iter().any(|i| {
            i.status == "idle"
                && i.hook_timestamp.unwrap_or(0.0) > i.ack_timestamp
        });

        let slots = tray::TraySlots {
            any_running: running > 0,
            any_needs_input: needs_input > 0,
            any_idle: unacked_reply,
        };

        let mut tooltip_parts: Vec<String> = Vec::new();
        for (status, count) in &counts {
            tooltip_parts.push(format!("{count} {status}"));
        }
        let tooltip = if tooltip_parts.is_empty() {
            "AIM — no instances".to_string()
        } else {
            tooltip_parts.join(" · ")
        };

        tray::set_state(&app, slots, &tooltip);

        let settings = crate::server::instances::read_json(&crate::paths::SETTINGS_FILE);
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
                    match (old.status.as_str(), inst.status.as_str()) {
                        (_, "needs_input") => {
                            let name = inst
                                .custom_name
                                .as_deref()
                                .or(inst.title.as_deref())
                                .unwrap_or(&inst.name);
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
                        (_, "idle") if old.status == "running" => {
                            let name = inst
                                .custom_name
                                .as_deref()
                                .or(inst.title.as_deref())
                                .unwrap_or(&inst.name);
                            let title = format!("🟢 {name}");
                            notifications::send(&app, &title, "Reply finished", Some(sid));
                            if sound_enabled {
                                sound::play_funk();
                            }
                        }
                        _ => {}
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
