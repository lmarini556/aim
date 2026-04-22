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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_default_all_false() {
        let s = TraySlots::default();
        assert!(!s.any_running && !s.any_needs_input && !s.any_idle);
    }

    #[test]
    fn slots_mask_all_false_is_zero() {
        assert_eq!(TraySlots::default().mask(), 0);
    }

    #[test]
    fn slots_mask_running_only() {
        let s = TraySlots { any_running: true, ..Default::default() };
        assert_eq!(s.mask(), 1);
    }

    #[test]
    fn slots_mask_needs_input_only() {
        let s = TraySlots { any_needs_input: true, ..Default::default() };
        assert_eq!(s.mask(), 2);
    }

    #[test]
    fn slots_mask_idle_only() {
        let s = TraySlots { any_idle: true, ..Default::default() };
        assert_eq!(s.mask(), 4);
    }

    #[test]
    fn slots_mask_all_true_is_seven() {
        let s = TraySlots {
            any_running: true,
            any_needs_input: true,
            any_idle: true,
        };
        assert_eq!(s.mask(), 7);
    }

    #[test]
    fn slots_mask_running_plus_idle() {
        let s = TraySlots {
            any_running: true,
            any_needs_input: false,
            any_idle: true,
        };
        assert_eq!(s.mask(), 5);
    }

    #[test]
    fn slots_clone_copy_debug() {
        let s = TraySlots { any_running: true, ..Default::default() };
        let s2 = s;
        assert_eq!(s.mask(), s2.mask());
        let _ = s.clone();
        assert!(format!("{s:?}").contains("TraySlots"));
    }

    #[test]
    fn icons_array_length_matches_mask_range() {
        assert_eq!(ICONS.len(), 8);
        for bytes in ICONS {
            assert!(!bytes.is_empty());
        }
    }

    #[test]
    fn mask_indexes_valid_icon_for_every_combination() {
        for r in [false, true] {
            for n in [false, true] {
                for i in [false, true] {
                    let s = TraySlots {
                        any_running: r,
                        any_needs_input: n,
                        any_idle: i,
                    };
                    let idx = s.mask();
                    assert!(idx < ICONS.len());
                }
            }
        }
    }
}
