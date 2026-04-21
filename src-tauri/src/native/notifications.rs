use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;
use tracing::warn;

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

    if let Err(e) = result {
        warn!("notification failed: {e}");
        let _ = app.emit(
            "notification",
            serde_json::json!({
                "title": title,
                "body": body,
                "session_id": session_id,
            }),
        );
    }
}
