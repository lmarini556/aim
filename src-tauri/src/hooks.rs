use crate::paths;
use serde_json::{json, Value};
use tracing::info;

const MARKER: &str = "# claude-instances-ui";

const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "SubagentStop",
    "Notification",
    "Stop",
    "SessionEnd",
];

fn hook_entry() -> Value {
    let installed_hook = paths::HOOK_BIN_DIR.join("hook_writer.py");
    json!({
        "hooks": [{
            "type": "command",
            "command": format!("/usr/bin/env python3 {} {MARKER}", installed_hook.display()),
        }]
    })
}

pub fn install(source_hook: &std::path::Path) {
    if !source_hook.exists() {
        tracing::warn!("hook source not found at {}", source_hook.display());
        return;
    }

    let _ = std::fs::create_dir_all(&*paths::HOOK_BIN_DIR);
    let dest = paths::HOOK_BIN_DIR.join("hook_writer.py");
    let _ = std::fs::copy(source_hook, &dest);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
    }

    let mut settings: Value = if paths::CLAUDE_SETTINGS.exists() {
        std::fs::read_to_string(&*paths::CLAUDE_SETTINGS)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(json!({}))
    } else {
        json!({})
    };

    let hooks = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));

    for event in EVENTS {
        let bucket = hooks
            .as_object_mut()
            .unwrap()
            .entry(*event)
            .or_insert_with(|| json!([]));

        if let Some(arr) = bucket.as_array_mut() {
            arr.retain(|entry| {
                let s = serde_json::to_string(entry).unwrap_or_default();
                !s.contains(MARKER)
            });
            arr.push(hook_entry());
        }
    }

    let _ = std::fs::create_dir_all(
        paths::CLAUDE_SETTINGS
            .parent()
            .unwrap_or(std::path::Path::new(".")),
    );
    let _ = std::fs::write(
        &*paths::CLAUDE_SETTINGS,
        serde_json::to_string_pretty(&settings).unwrap_or_default(),
    );

    info!("hooks installed to {}", paths::CLAUDE_SETTINGS.display());
}

pub fn uninstall() {
    if paths::CLAUDE_SETTINGS.exists() {
        let mut settings: Value = std::fs::read_to_string(&*paths::CLAUDE_SETTINGS)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(json!({}));

        if let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) {
            for event in EVENTS {
                if let Some(bucket) = hooks.get_mut(*event).and_then(Value::as_array_mut) {
                    bucket.retain(|entry| {
                        let s = serde_json::to_string(entry).unwrap_or_default();
                        !s.contains(MARKER)
                    });
                }
            }
            let empty_keys: Vec<String> = hooks
                .iter()
                .filter(|(_, v)| v.as_array().is_some_and(|a| a.is_empty()))
                .map(|(k, _)| k.clone())
                .collect();
            for k in empty_keys {
                hooks.remove(&k);
            }
            if hooks.is_empty() {
                settings.as_object_mut().unwrap().remove("hooks");
            }
        }

        let _ = std::fs::write(
            &*paths::CLAUDE_SETTINGS,
            serde_json::to_string_pretty(&settings).unwrap_or_default(),
        );
        info!("hooks uninstalled from {}", paths::CLAUDE_SETTINGS.display());
    }

    let dest = paths::HOOK_BIN_DIR.join("hook_writer.py");
    if dest.exists() {
        let _ = std::fs::remove_file(&dest);
    }
}
