use crate::infra::paths;
use crate::services::instances::{read_json, write_json};
use crate::http::dto::SettingsBody;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{set_test_home, FS_LOCK as LOCK};

    #[tokio::test]
    async fn get_settings_returns_defaults_when_no_file() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let _ = std::fs::remove_file(&*paths::SETTINGS_FILE);
        let r = api_get_settings().await;
        assert_eq!(r.get("sound").and_then(Value::as_bool), Some(true));
        assert_eq!(r.get("banner_ttl").and_then(Value::as_u64), Some(30));
        assert_eq!(r.get("poll_interval").and_then(Value::as_u64), Some(2));
    }

    #[tokio::test]
    async fn put_settings_writes_file_and_get_reads_it_back() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let body = SettingsBody {
            sound: Some(false),
            banner_ttl: Some(5),
            poll_interval: Some(10),
        };
        let put_r = api_put_settings(Json(body)).await;
        assert_eq!(put_r.get("ok").and_then(Value::as_bool), Some(true));

        let r = api_get_settings().await;
        assert_eq!(r.get("sound").and_then(Value::as_bool), Some(false));
        assert_eq!(r.get("banner_ttl").and_then(Value::as_u64), Some(5));
        assert_eq!(r.get("poll_interval").and_then(Value::as_u64), Some(10));
    }

    #[tokio::test]
    async fn put_settings_fills_missing_fields_with_defaults() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        let _ = std::fs::remove_file(&*paths::SETTINGS_FILE);
        let body = SettingsBody { sound: None, banner_ttl: None, poll_interval: None };
        let _ = api_put_settings(Json(body)).await;
        let r = api_get_settings().await;
        assert_eq!(r.get("sound").and_then(Value::as_bool), Some(true));
        assert_eq!(r.get("banner_ttl").and_then(Value::as_u64), Some(30));
        assert_eq!(r.get("poll_interval").and_then(Value::as_u64), Some(2));
    }

    #[tokio::test]
    async fn get_settings_falls_back_to_defaults_when_fields_missing() {
        let _g = LOCK.lock().unwrap();
        let _ = set_test_home();
        std::fs::create_dir_all(&*paths::APP_DIR).unwrap();
        std::fs::write(&*paths::SETTINGS_FILE, "{}").unwrap();
        let r = api_get_settings().await;
        assert_eq!(r.get("sound").and_then(Value::as_bool), Some(true));
        assert_eq!(r.get("banner_ttl").and_then(Value::as_u64), Some(30));
    }
}
