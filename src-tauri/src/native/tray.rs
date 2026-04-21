use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

const ICONS: [&[u8]; 8] = [
    include_bytes!("../../icons/tray-0.png"),
    include_bytes!("../../icons/tray-1.png"),
    include_bytes!("../../icons/tray-2.png"),
    include_bytes!("../../icons/tray-3.png"),
    include_bytes!("../../icons/tray-4.png"),
    include_bytes!("../../icons/tray-5.png"),
    include_bytes!("../../icons/tray-6.png"),
    include_bytes!("../../icons/tray-7.png"),
];

#[derive(Debug, Clone, Copy, Default)]
pub struct TraySlots {
    pub any_running: bool,
    pub any_needs_input: bool,
    pub any_idle: bool,
}

impl TraySlots {
    fn mask(self) -> usize {
        (self.any_running as usize)
            | ((self.any_needs_input as usize) << 1)
            | ((self.any_idle as usize) << 2)
    }
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let icon = tauri::image::Image::from_bytes(ICONS[0])?;

    TrayIconBuilder::with_id("main")
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

pub fn set_state(app: &AppHandle, slots: TraySlots, tooltip: &str) {
    let bytes = ICONS[slots.mask()];

    if let Some(tray) = app.tray_by_id("main") {
        if let Ok(icon) = tauri::image::Image::from_bytes(bytes) {
            let _ = tray.set_icon(Some(icon));
        }
        let _ = tray.set_title::<&str>(None);
        let _ = tray.set_tooltip(Some(tooltip));
    }
}
