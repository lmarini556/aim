use crate::infra::paths;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{reset_fs, set_test_home, FS_LOCK as LOCK};

    fn write_stub_hook() -> std::path::PathBuf {
        let home = set_test_home();
        let src = home.join("stub_hook.py");
        std::fs::write(&src, "#!/usr/bin/env python3\nprint('x')\n").unwrap();
        src
    }

    #[test]
    fn marker_constant_matches_expected() {
        assert_eq!(MARKER, "# claude-instances-ui");
    }

    #[test]
    fn events_list_has_eight_entries() {
        assert_eq!(EVENTS.len(), 8);
    }

    #[test]
    fn events_list_contains_session_and_tool_events() {
        for e in ["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse",
                  "SubagentStop", "Notification", "Stop", "SessionEnd"] {
            assert!(EVENTS.contains(&e), "missing event {e}");
        }
    }

    #[test]
    fn hook_entry_contains_marker_and_bin_path() {
        let _ = set_test_home();
        let e = hook_entry();
        let cmd = e.pointer("/hooks/0/command").and_then(Value::as_str).unwrap();
        assert!(cmd.contains(MARKER));
        assert!(cmd.contains("hook_writer.py"));
        assert!(cmd.contains("/usr/bin/env python3"));
    }

    #[test]
    fn hook_entry_type_is_command() {
        let _ = set_test_home();
        let e = hook_entry();
        let ty = e.pointer("/hooks/0/type").and_then(Value::as_str).unwrap();
        assert_eq!(ty, "command");
    }

    #[test]
    fn install_writes_hook_file_to_bin_dir() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        let src = write_stub_hook();
        install(&src);
        let dest = paths::HOOK_BIN_DIR.join("hook_writer.py");
        assert!(dest.is_file());
        let _ = std::fs::remove_file(&src);
    }

    #[test]
    fn install_writes_settings_with_all_events() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        let src = write_stub_hook();
        install(&src);
        let _ = std::fs::remove_file(&src);

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap(),
        ).unwrap();
        let hooks = settings.get("hooks").unwrap().as_object().unwrap();
        for e in EVENTS {
            let bucket = hooks.get(*e).unwrap().as_array().unwrap();
            assert_eq!(bucket.len(), 1, "event {e}");
        }
    }

    #[test]
    fn install_noop_when_source_missing() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        let home = set_test_home();
        install(&home.join("does-not-exist.py"));
        assert!(!paths::CLAUDE_SETTINGS.exists());
    }

    #[test]
    fn install_preserves_unrelated_settings() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        std::fs::create_dir_all(&*paths::CLAUDE_DIR).unwrap();
        std::fs::write(
            &*paths::CLAUDE_SETTINGS,
            serde_json::to_string_pretty(&json!({"theme": "dark"})).unwrap(),
        ).unwrap();

        let src = write_stub_hook();
        install(&src);
        let _ = std::fs::remove_file(&src);

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap(),
        ).unwrap();
        assert_eq!(settings.get("theme").and_then(Value::as_str), Some("dark"));
        assert!(settings.get("hooks").is_some());
    }

    #[test]
    fn install_preserves_unrelated_entries_in_same_bucket() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        std::fs::create_dir_all(&*paths::CLAUDE_DIR).unwrap();
        std::fs::write(
            &*paths::CLAUDE_SETTINGS,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "SessionStart": [{
                        "hooks": [{"type": "command", "command": "/bin/other-hook"}]
                    }]
                }
            })).unwrap(),
        ).unwrap();

        let src = write_stub_hook();
        install(&src);
        let _ = std::fs::remove_file(&src);

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap(),
        ).unwrap();
        let bucket = settings.pointer("/hooks/SessionStart").unwrap().as_array().unwrap();
        assert_eq!(bucket.len(), 2);
    }

    #[test]
    fn install_twice_does_not_duplicate_marker_entry() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        let src = write_stub_hook();
        install(&src);
        install(&src);
        let _ = std::fs::remove_file(&src);

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap(),
        ).unwrap();
        for e in EVENTS {
            let bucket = settings.pointer(&format!("/hooks/{e}")).unwrap().as_array().unwrap();
            let marker_count = bucket.iter().filter(|entry| {
                entry.pointer("/hooks/0/command").and_then(Value::as_str)
                    .map(|c| c.contains(MARKER)).unwrap_or(false)
            }).count();
            assert_eq!(marker_count, 1, "event {e}");
        }
    }

    #[test]
    fn install_replaces_corrupt_settings_with_fresh() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        std::fs::create_dir_all(&*paths::CLAUDE_DIR).unwrap();
        std::fs::write(&*paths::CLAUDE_SETTINGS, "not valid json {").unwrap();

        let src = write_stub_hook();
        install(&src);
        let _ = std::fs::remove_file(&src);

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap(),
        ).unwrap();
        assert!(settings.get("hooks").is_some());
    }

    #[test]
    fn uninstall_removes_hooks_key_when_buckets_all_empty() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        let src = write_stub_hook();
        install(&src);
        uninstall();
        let _ = std::fs::remove_file(&src);

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap(),
        ).unwrap();
        assert!(settings.get("hooks").is_none());
    }

    #[test]
    fn uninstall_removes_hook_file() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        let src = write_stub_hook();
        install(&src);
        uninstall();
        let _ = std::fs::remove_file(&src);

        let dest = paths::HOOK_BIN_DIR.join("hook_writer.py");
        assert!(!dest.exists());
    }

    #[test]
    fn uninstall_preserves_non_marker_entries() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        let src = write_stub_hook();
        install(&src);

        let mut settings: Value = serde_json::from_str(
            &std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap(),
        ).unwrap();
        settings.pointer_mut("/hooks/SessionStart").unwrap().as_array_mut().unwrap()
            .push(json!({"hooks": [{"type": "command", "command": "/bin/other"}]}));
        std::fs::write(
            &*paths::CLAUDE_SETTINGS,
            serde_json::to_string_pretty(&settings).unwrap(),
        ).unwrap();

        uninstall();
        let _ = std::fs::remove_file(&src);

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap(),
        ).unwrap();
        let bucket = settings.pointer("/hooks/SessionStart").unwrap().as_array().unwrap();
        assert_eq!(bucket.len(), 1);
        assert!(bucket[0].pointer("/hooks/0/command").and_then(Value::as_str)
            .unwrap().contains("other"));
    }

    #[test]
    fn uninstall_noop_when_settings_missing() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        uninstall();
        assert!(!paths::CLAUDE_SETTINGS.exists());
    }

    #[test]
    fn uninstall_handles_corrupt_settings_gracefully() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        std::fs::create_dir_all(&*paths::CLAUDE_DIR).unwrap();
        std::fs::write(&*paths::CLAUDE_SETTINGS, "{broken").unwrap();
        uninstall();
        let contents = std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap();
        let _: Value = serde_json::from_str(&contents).unwrap();
    }

    #[test]
    fn install_leaves_non_array_bucket_untouched() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        std::fs::create_dir_all(&*paths::CLAUDE_DIR).unwrap();
        std::fs::write(
            &*paths::CLAUDE_SETTINGS,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "SessionStart": "not-an-array",
                }
            })).unwrap(),
        ).unwrap();

        let src = write_stub_hook();
        install(&src);
        let _ = std::fs::remove_file(&src);

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap(),
        ).unwrap();
        assert_eq!(
            settings.pointer("/hooks/SessionStart").and_then(Value::as_str),
            Some("not-an-array")
        );
    }

    #[test]
    fn uninstall_leaves_non_array_bucket_untouched() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        std::fs::create_dir_all(&*paths::CLAUDE_DIR).unwrap();
        std::fs::write(
            &*paths::CLAUDE_SETTINGS,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "SessionStart": "not-an-array",
                }
            })).unwrap(),
        ).unwrap();

        uninstall();

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap(),
        ).unwrap();
        assert_eq!(
            settings.pointer("/hooks/SessionStart").and_then(Value::as_str),
            Some("not-an-array")
        );
    }

    #[test]
    fn uninstall_preserves_unrelated_top_level_settings() {
        let _g = LOCK.lock().unwrap();
        reset_fs();
        let src = write_stub_hook();
        install(&src);

        let mut settings: Value = serde_json::from_str(
            &std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap(),
        ).unwrap();
        settings.as_object_mut().unwrap().insert("theme".into(), json!("dark"));
        std::fs::write(
            &*paths::CLAUDE_SETTINGS,
            serde_json::to_string_pretty(&settings).unwrap(),
        ).unwrap();

        uninstall();
        let _ = std::fs::remove_file(&src);

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(&*paths::CLAUDE_SETTINGS).unwrap(),
        ).unwrap();
        assert_eq!(settings.get("theme").and_then(Value::as_str), Some("dark"));
    }
}
