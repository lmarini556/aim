use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

const ICON_IDLE: &[u8] = include_bytes!("../../icons/tray-idle.png");
const ICON_RUNNING: &[u8] = include_bytes!("../../icons/tray-running.png");
const ICON_NEEDS_INPUT: &[u8] = include_bytes!("../../icons/tray-needs-input.png");
const ICON_FRESH: &[u8] = include_bytes!("../../icons/tray-fresh.png");
const ICON_UNREACHABLE: &[u8] = include_bytes!("../../icons/tray-unreachable.png");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    Idle,
    Running,
    NeedsInput,
    Fresh,
    Unreachable,
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let icon = tauri::image::Image::from_bytes(ICON_IDLE)?;

    TrayIconBuilder::new()
        .icon(icon)
        .icon_as_template(false)
        .tooltip("AIM")
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(win) = app.get_webview_window("main") {
                    if win.is_visible().unwrap_or(false) {
                        let _ = win.hide();
                    } else {
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                }
            }
        })
        .build(app)?;

    Ok(())
}

pub fn set_state(app: &AppHandle, state: TrayState, tooltip: &str) {
    let bytes = match state {
        TrayState::Idle => ICON_IDLE,
        TrayState::Running => ICON_RUNNING,
        TrayState::NeedsInput => ICON_NEEDS_INPUT,
        TrayState::Fresh => ICON_FRESH,
        TrayState::Unreachable => ICON_UNREACHABLE,
    };

    if let Ok(icon) = tauri::image::Image::from_bytes(bytes) {
        if let Some(tray) = app.tray_by_id("main") {
            let _ = tray.set_icon(Some(icon));
            let _ = tray.set_tooltip(Some(tooltip));
        }
    }
}
