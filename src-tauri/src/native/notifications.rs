use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;
use tracing::{info, warn};

pub fn send(
    app: &AppHandle,
    title: &str,
    body: &str,
    session_id: Option<&str>,
) {
    let result = app.notification()
        .builder()
        .title(title)
        .body(body)
        .show();

    match result {
        Ok(_) => info!("notification posted via plugin: {title}"),
        Err(e) => {
            warn!("notification plugin failed: {e} — falling back to osascript");
            osascript_notify(title, body);
        }
    }

    let _ = app.emit(
        "notification",
        serde_json::json!({
            "title": title,
            "body": body,
            "session_id": session_id,
        }),
    );
}

fn osascript_notify(title: &str, body: &str) {
    let escape = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display notification \"{}\" with title \"{}\" sound name \"Glass\"",
        escape(body),
        escape(title),
    );
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
