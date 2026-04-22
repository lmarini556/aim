use std::path::PathBuf;
use std::sync::LazyLock;

pub static HOME: LazyLock<PathBuf> =
    LazyLock::new(|| dirs::home_dir().expect("cannot resolve home directory"));

pub static CLAUDE_DIR: LazyLock<PathBuf> = LazyLock::new(|| HOME.join(".claude"));
pub static SESSIONS_DIR: LazyLock<PathBuf> = LazyLock::new(|| CLAUDE_DIR.join("sessions"));
pub static PROJECTS_DIR: LazyLock<PathBuf> = LazyLock::new(|| CLAUDE_DIR.join("projects"));
pub static GLOBAL_MCP: LazyLock<PathBuf> = LazyLock::new(|| CLAUDE_DIR.join("mcp.json"));
pub static CLAUDE_SETTINGS: LazyLock<PathBuf> = LazyLock::new(|| CLAUDE_DIR.join("settings.json"));
pub static GLOBAL_COMMANDS_DIR: LazyLock<PathBuf> = LazyLock::new(|| CLAUDE_DIR.join("commands"));

pub static APP_DIR: LazyLock<PathBuf> = LazyLock::new(|| HOME.join(".claude-instances-ui"));
pub static STATE_DIR: LazyLock<PathBuf> = LazyLock::new(|| APP_DIR.join("state"));
pub static SUMMARY_DIR: LazyLock<PathBuf> = LazyLock::new(|| APP_DIR.join("summary"));
pub static GROUPS_FILE: LazyLock<PathBuf> = LazyLock::new(|| APP_DIR.join("groups.json"));
pub static NAMES_FILE: LazyLock<PathBuf> = LazyLock::new(|| APP_DIR.join("names.json"));
pub static ACKS_FILE: LazyLock<PathBuf> = LazyLock::new(|| APP_DIR.join("acks.json"));
pub static TOKEN_FILE: LazyLock<PathBuf> = LazyLock::new(|| APP_DIR.join("token"));
pub static SETTINGS_FILE: LazyLock<PathBuf> = LazyLock::new(|| APP_DIR.join("settings.json"));
pub static UPLOAD_DIR: LazyLock<PathBuf> = LazyLock::new(|| APP_DIR.join("uploads"));
pub static MCP_CONFIG_DIR: LazyLock<PathBuf> = LazyLock::new(|| APP_DIR.join("mcp-configs"));
pub static PENDING_NAMES_FILE: LazyLock<PathBuf> =
    LazyLock::new(|| APP_DIR.join("pending_names.json"));
pub static PENDING_GROUPS_FILE: LazyLock<PathBuf> =
    LazyLock::new(|| APP_DIR.join("pending_groups.json"));
pub static HOOK_BIN_DIR: LazyLock<PathBuf> = LazyLock::new(|| APP_DIR.join("bin"));

pub static STATIC_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    resolve_static_dir(
        std::env::current_exe().ok(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")),
    )
});

fn resolve_static_dir(exe: Option<PathBuf>, manifest: PathBuf) -> PathBuf {
    let dev_static = exe
        .as_ref()
        .and_then(|e| e.parent())
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|p| p.join("static"));
    if let Some(ref d) = dev_static {
        if d.is_dir() {
            return d.clone();
        }
    }
    let repo_static = manifest.parent().map(|p| p.join("static"));
    if let Some(ref d) = repo_static {
        if d.is_dir() {
            return d.clone();
        }
    }
    PathBuf::from("static")
}

pub fn ensure_dirs() {
    for d in [
        &*APP_DIR,
        &*STATE_DIR,
        &*SUMMARY_DIR,
        &*UPLOAD_DIR,
        &*MCP_CONFIG_DIR,
        &*HOOK_BIN_DIR,
    ] {
        let _ = std::fs::create_dir_all(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{set_test_home, FS_LOCK as LOCK};

    #[test]
    fn home_is_initialized_to_test_fixture() {
        let expected = set_test_home();
        assert_eq!(&*HOME, &expected);
    }

    #[test]
    fn claude_dir_nested_under_home() {
        let h = set_test_home();
        assert_eq!(&*CLAUDE_DIR, &h.join(".claude"));
    }

    #[test]
    fn sessions_dir_under_claude() {
        let h = set_test_home();
        assert_eq!(&*SESSIONS_DIR, &h.join(".claude").join("sessions"));
    }

    #[test]
    fn projects_dir_under_claude() {
        let h = set_test_home();
        assert_eq!(&*PROJECTS_DIR, &h.join(".claude").join("projects"));
    }

    #[test]
    fn global_mcp_path() {
        let h = set_test_home();
        assert_eq!(&*GLOBAL_MCP, &h.join(".claude").join("mcp.json"));
    }

    #[test]
    fn claude_settings_path() {
        let h = set_test_home();
        assert_eq!(&*CLAUDE_SETTINGS, &h.join(".claude").join("settings.json"));
    }

    #[test]
    fn global_commands_dir_path() {
        let h = set_test_home();
        assert_eq!(&*GLOBAL_COMMANDS_DIR, &h.join(".claude").join("commands"));
    }

    #[test]
    fn app_dir_nested_under_home() {
        let h = set_test_home();
        assert_eq!(&*APP_DIR, &h.join(".claude-instances-ui"));
    }

    #[test]
    fn state_dir_under_app() {
        let h = set_test_home();
        assert_eq!(&*STATE_DIR, &h.join(".claude-instances-ui").join("state"));
    }

    #[test]
    fn summary_dir_under_app() {
        let h = set_test_home();
        assert_eq!(&*SUMMARY_DIR, &h.join(".claude-instances-ui").join("summary"));
    }

    #[test]
    fn groups_file_path() {
        let h = set_test_home();
        assert_eq!(&*GROUPS_FILE, &h.join(".claude-instances-ui").join("groups.json"));
    }

    #[test]
    fn names_file_path() {
        let h = set_test_home();
        assert_eq!(&*NAMES_FILE, &h.join(".claude-instances-ui").join("names.json"));
    }

    #[test]
    fn acks_file_path() {
        let h = set_test_home();
        assert_eq!(&*ACKS_FILE, &h.join(".claude-instances-ui").join("acks.json"));
    }

    #[test]
    fn token_file_path() {
        let h = set_test_home();
        assert_eq!(&*TOKEN_FILE, &h.join(".claude-instances-ui").join("token"));
    }

    #[test]
    fn settings_file_path() {
        let h = set_test_home();
        assert_eq!(&*SETTINGS_FILE, &h.join(".claude-instances-ui").join("settings.json"));
    }

    #[test]
    fn upload_dir_path() {
        let h = set_test_home();
        assert_eq!(&*UPLOAD_DIR, &h.join(".claude-instances-ui").join("uploads"));
    }

    #[test]
    fn mcp_config_dir_path() {
        let h = set_test_home();
        assert_eq!(&*MCP_CONFIG_DIR, &h.join(".claude-instances-ui").join("mcp-configs"));
    }

    #[test]
    fn pending_names_file_path() {
        let h = set_test_home();
        assert_eq!(&*PENDING_NAMES_FILE, &h.join(".claude-instances-ui").join("pending_names.json"));
    }

    #[test]
    fn pending_groups_file_path() {
        let h = set_test_home();
        assert_eq!(&*PENDING_GROUPS_FILE, &h.join(".claude-instances-ui").join("pending_groups.json"));
    }

    #[test]
    fn hook_bin_dir_path() {
        let h = set_test_home();
        assert_eq!(&*HOOK_BIN_DIR, &h.join(".claude-instances-ui").join("bin"));
    }

    #[test]
    fn static_dir_resolves_to_existing_dir_or_literal() {
        let p = &*STATIC_DIR;
        assert!(p.ends_with("static"));
    }

    #[test]
    fn resolve_static_dir_returns_dev_path_when_exists() {
        let tmp = std::env::temp_dir().join(format!("ciu_static_dev_{}", std::process::id()));
        let base = tmp.join("root");
        let fake_deps = base.join("debug").join("deps");
        let fake_static = base.join("static");
        std::fs::create_dir_all(&fake_deps).unwrap();
        std::fs::create_dir_all(&fake_static).unwrap();
        let fake_exe = fake_deps.join("bin");
        std::fs::write(&fake_exe, b"").unwrap();
        let resolved = resolve_static_dir(Some(fake_exe), PathBuf::from("/nonexistent"));
        assert_eq!(resolved, fake_static);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_static_dir_returns_repo_path_when_dev_missing() {
        let tmp = std::env::temp_dir().join(format!("ciu_static_repo_{}", std::process::id()));
        let fake_repo = tmp.join("repo");
        let fake_static = fake_repo.join("static");
        let manifest = fake_repo.join("src");
        std::fs::create_dir_all(&fake_static).unwrap();
        std::fs::create_dir_all(&manifest).unwrap();
        let resolved = resolve_static_dir(None, manifest);
        assert_eq!(resolved, fake_static);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_static_dir_falls_back_to_literal_when_both_missing() {
        let resolved = resolve_static_dir(
            Some(PathBuf::from("/nonexistent/a/b/c/bin")),
            PathBuf::from("/nonexistent-manifest"),
        );
        assert_eq!(resolved, PathBuf::from("static"));
    }

    #[test]
    fn resolve_static_dir_falls_back_when_manifest_has_no_parent() {
        let resolved = resolve_static_dir(None, PathBuf::from(""));
        assert_eq!(resolved, PathBuf::from("static"));
    }

    #[test]
    fn ensure_dirs_creates_all_app_subdirs() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        ensure_dirs();
        for d in [&*APP_DIR, &*STATE_DIR, &*SUMMARY_DIR, &*UPLOAD_DIR, &*MCP_CONFIG_DIR, &*HOOK_BIN_DIR] {
            assert!(d.is_dir(), "{d:?} should exist after ensure_dirs()");
        }
    }

    #[test]
    fn ensure_dirs_is_idempotent() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        ensure_dirs();
        ensure_dirs();
        assert!(APP_DIR.is_dir());
    }
}
