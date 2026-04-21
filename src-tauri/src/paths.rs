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
    let exe = std::env::current_exe().unwrap_or_default();
    let dev_static = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|p| p.join("static"));
    if let Some(ref d) = dev_static {
        if d.is_dir() {
            return d.clone();
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_static = manifest.parent().map(|p| p.join("static"));
    if let Some(ref d) = repo_static {
        if d.is_dir() {
            return d.clone();
        }
    }
    PathBuf::from("static")
});

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
