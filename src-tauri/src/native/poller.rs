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

        let tray_state = if counts.get("needs_input").copied().unwrap_or(0) > 0 {
            tray::TrayState::NeedsInput
        } else if counts.get("running").copied().unwrap_or(0) > 0 {
            tray::TrayState::Running
        } else if instances.is_empty() {
            tray::TrayState::Unreachable
        } else {
            tray::TrayState::Idle
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

        tray::set_state(&app, tray_state, &tooltip);

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
                            let title = inst
                                .custom_name
                                .as_deref()
                                .or(inst.title.as_deref())
                                .unwrap_or(&inst.name);
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
                            notifications::send(&app, title, body, Some(sid));
                            if sound_enabled {
                                sound::play_glass();
                            }
                        }
                        (_, "idle") if old.status == "running" => {
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
