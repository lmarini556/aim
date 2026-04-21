use crate::paths;
use crate::server::instances::{read_json, write_json};
use crate::server::types::SettingsBody;
use axum::Json;
use serde_json::{json, Value};

pub async fn api_get_settings() -> Json<Value> {
    let saved = read_json(&paths::SETTINGS_FILE);
    let sound = saved.get("sound").and_then(Value::as_bool).unwrap_or(true);
    let banner_ttl = saved
        .get("banner_ttl")
        .and_then(Value::as_u64)
        .unwrap_or(30) as u32;
    let poll_interval = saved
        .get("poll_interval")
        .and_then(Value::as_u64)
        .unwrap_or(2) as u32;

    Json(json!({
        "sound": sound,
        "banner_ttl": banner_ttl,
        "poll_interval": poll_interval,
    }))
}

pub async fn api_put_settings(Json(body): Json<SettingsBody>) -> Json<Value> {
    let data = json!({
        "sound": body.sound.unwrap_or(true),
        "banner_ttl": body.banner_ttl.unwrap_or(30),
        "poll_interval": body.poll_interval.unwrap_or(2),
    });
    write_json(&paths::SETTINGS_FILE, &data);
    Json(json!({"ok": true}))
}
