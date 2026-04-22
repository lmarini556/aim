// Pinning tests for I/O-bound behavior (hooks settings merge, json round-trips).
// Each integration-test file is its own binary, so the `HOME` env override
// below only affects this test process.
//
// These tests live in `tests/` so they run under `cargo test` but are
// excluded from the coverage number, which is measured via `cargo llvm-cov --lib`.
//
// NOTE: `paths::HOME` is a `LazyLock<PathBuf>` — once accessed, it's frozen for
// the life of the process. So the whole file shares ONE HOME fixture and tests
// must clean up between runs.

use aim_lib::services::instances::{read_json, write_json};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Once;

static INIT: Once = Once::new();

fn fixture_home() -> PathBuf {
    INIT.call_once(|| {
        let tmp = std::env::temp_dir().join(format!(
            "aim-pin-io-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("HOME", &tmp);
    });
    // Fetch the path that `dirs::home_dir()` resolved — this is what
    // `paths::HOME` has cached.
    dirs::home_dir().unwrap()
}

fn reset_filesystem() {
    let home = fixture_home();
    for sub in [".claude", ".claude-instances-ui"] {
        let p = home.join(sub);
        if p.is_dir() {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

// ----- read_json / write_json round-trips -----

#[test]
fn read_json_missing_file_returns_empty_object() {
    let home = fixture_home();
    let v = read_json(&home.join("missing-zzz.json"));
    assert_eq!(v, json!({}));
}

#[test]
fn read_json_invalid_json_returns_empty_object() {
    let home = fixture_home();
    let p = home.join("bad-zzz.json");
    std::fs::write(&p, "not json").unwrap();
    assert_eq!(read_json(&p), json!({}));
    let _ = std::fs::remove_file(&p);
}

#[test]
fn read_json_valid_returns_parsed_value() {
    let home = fixture_home();
    let p = home.join("good-zzz.json");
    std::fs::write(&p, r#"{"a": 1, "b": [2, 3]}"#).unwrap();
    assert_eq!(read_json(&p), json!({"a": 1, "b": [2, 3]}));
    let _ = std::fs::remove_file(&p);
}

#[test]
fn write_json_creates_parent_dirs() {
    let home = fixture_home();
    let p = home.join("write-parents-zzz").join("deep").join("out.json");
    write_json(&p, &json!({"x": 42}));
    assert!(p.is_file());
    let back = read_json(&p);
    assert_eq!(back, json!({"x": 42}));
    let _ = std::fs::remove_dir_all(home.join("write-parents-zzz"));
}

#[test]
fn write_json_overwrites_existing() {
    let home = fixture_home();
    let p = home.join("over-zzz.json");
    write_json(&p, &json!({"v": 1}));
    write_json(&p, &json!({"v": 2}));
    assert_eq!(read_json(&p), json!({"v": 2}));
    let _ = std::fs::remove_file(&p);
}

#[test]
fn write_json_pretty_formatted() {
    let home = fixture_home();
    let p = home.join("pretty-zzz.json");
    write_json(&p, &json!({"a": 1}));
    let raw = std::fs::read_to_string(&p).unwrap();
    assert!(raw.contains('\n'), "expected pretty-printed JSON with newlines");
    let _ = std::fs::remove_file(&p);
}

// ----- hooks.rs install / uninstall -----
// These must NOT run in parallel because they all touch the same `~/.claude/settings.json`.
// `cargo test` sharding is by default parallel; we neutralize that by using a file lock.

use std::sync::Mutex;
static HOOKS_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn hooks_install_adds_all_events() {
    let _g = HOOKS_LOCK.lock().unwrap();
    reset_filesystem();
    let home = fixture_home();

    let src = home.join("hook_writer.py");
    std::fs::write(&src, "#!/usr/bin/env python3\nprint('hi')\n").unwrap();
    aim_lib::hooks::install(&src);
    let _ = std::fs::remove_file(&src);

    let settings_path = home.join(".claude").join("settings.json");
    assert!(settings_path.is_file(), "settings.json should be written");
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();

    let hooks = settings.get("hooks").unwrap().as_object().unwrap();
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "SubagentStop",
        "Notification",
        "Stop",
        "SessionEnd",
    ] {
        let bucket = hooks.get(event).unwrap().as_array().unwrap();
        assert_eq!(bucket.len(), 1, "event {event} should have 1 entry");
        let cmd = bucket[0]
            .pointer("/hooks/0/command")
            .and_then(Value::as_str)
            .expect("hook command string");
        assert!(cmd.contains("# claude-instances-ui"));
        assert!(cmd.contains("hook_writer.py"));
    }
}

#[test]
fn hooks_install_preserves_unrelated_entries() {
    let _g = HOOKS_LOCK.lock().unwrap();
    reset_filesystem();
    let home = fixture_home();

    let src = home.join("hook_writer.py");
    std::fs::write(&src, "stub").unwrap();

    let settings_path = home.join(".claude").join("settings.json");
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{"type": "command", "command": "/usr/bin/env other-hook"}]
                }]
            },
            "other_setting": true
        }))
        .unwrap(),
    )
    .unwrap();

    aim_lib::hooks::install(&src);
    let _ = std::fs::remove_file(&src);

    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(settings.get("other_setting"), Some(&Value::Bool(true)));
    let bucket = settings
        .pointer("/hooks/SessionStart")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(bucket.len(), 2, "existing + new entry");
    let cmds: Vec<&str> = bucket
        .iter()
        .filter_map(|e| e.pointer("/hooks/0/command").and_then(Value::as_str))
        .collect();
    assert!(cmds.iter().any(|c| c.contains("other-hook")));
    assert!(cmds.iter().any(|c| c.contains("# claude-instances-ui")));
}

#[test]
fn hooks_install_replaces_existing_marker_entry_without_duplicating() {
    let _g = HOOKS_LOCK.lock().unwrap();
    reset_filesystem();
    let home = fixture_home();

    let src = home.join("hook_writer.py");
    std::fs::write(&src, "stub").unwrap();

    aim_lib::hooks::install(&src);
    aim_lib::hooks::install(&src);
    let _ = std::fs::remove_file(&src);

    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();

    let bucket = settings
        .pointer("/hooks/SessionStart")
        .and_then(Value::as_array)
        .unwrap();
    let marker_entries: Vec<_> = bucket
        .iter()
        .filter(|e| {
            e.pointer("/hooks/0/command")
                .and_then(Value::as_str)
                .map(|c| c.contains("# claude-instances-ui"))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(marker_entries.len(), 1, "should not duplicate");
}

#[test]
fn hooks_uninstall_removes_marker_entries_and_empty_buckets() {
    let _g = HOOKS_LOCK.lock().unwrap();
    reset_filesystem();
    let home = fixture_home();

    let src = home.join("hook_writer.py");
    std::fs::write(&src, "stub").unwrap();

    aim_lib::hooks::install(&src);
    aim_lib::hooks::uninstall();
    let _ = std::fs::remove_file(&src);

    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();

    assert!(
        settings.get("hooks").is_none(),
        "hooks key should be removed when all events emptied; got {settings:?}"
    );
    assert!(!home
        .join(".claude-instances-ui")
        .join("bin")
        .join("hook_writer.py")
        .is_file());
}

#[test]
fn hooks_uninstall_preserves_non_marker_entries() {
    let _g = HOOKS_LOCK.lock().unwrap();
    reset_filesystem();
    let home = fixture_home();

    let src = home.join("hook_writer.py");
    std::fs::write(&src, "stub").unwrap();

    aim_lib::hooks::install(&src);

    let settings_path = home.join(".claude").join("settings.json");
    let mut settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let bucket = settings
        .pointer_mut("/hooks/SessionStart")
        .and_then(Value::as_array_mut)
        .unwrap();
    bucket.push(json!({
        "hooks": [{"type": "command", "command": "/usr/bin/env other-hook"}]
    }));
    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

    aim_lib::hooks::uninstall();
    let _ = std::fs::remove_file(&src);

    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let bucket = settings
        .pointer("/hooks/SessionStart")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(bucket.len(), 1);
    assert!(bucket[0]
        .pointer("/hooks/0/command")
        .and_then(Value::as_str)
        .unwrap()
        .contains("other-hook"));
}

#[test]
fn hooks_uninstall_is_noop_when_settings_missing() {
    let _g = HOOKS_LOCK.lock().unwrap();
    reset_filesystem();
    aim_lib::hooks::uninstall(); // must not panic
}

#[test]
fn hooks_install_noop_if_source_missing() {
    let _g = HOOKS_LOCK.lock().unwrap();
    reset_filesystem();
    let home = fixture_home();
    let settings_path = home.join(".claude").join("settings.json");
    let _ = std::fs::remove_file(&settings_path);

    aim_lib::hooks::install(&home.join("does-not-exist.py"));

    assert!(
        !settings_path.is_file(),
        "should not write settings when source missing"
    );
}
