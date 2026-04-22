#![cfg(target_os = "macos")]

use objc2::ffi::{class_replaceMethod, sel_registerName};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{msg_send, sel};
use std::ffi::{c_char, c_void};
use tauri::WebviewWindow;
use tracing::{info, warn};

extern "C" fn menu_for_event_nil(
    _this: *mut AnyObject,
    _sel: Sel,
    _event: *mut AnyObject,
) -> *mut AnyObject {
    std::ptr::null_mut()
}

pub fn disable_context_menu(win: &WebviewWindow) {
    let res = win.with_webview(|webview| unsafe {
        let ns_view = webview.inner() as *mut AnyObject;
        if ns_view.is_null() {
            warn!("webview inner pointer is null");
            return;
        }

        let cls: *const AnyClass = msg_send![ns_view, class];
        if cls.is_null() {
            warn!("webview class is null");
            return;
        }

        let sel = sel_registerName(b"menuForEvent:\0".as_ptr() as *const c_char);
        let types = b"@@:@\0".as_ptr() as *const c_char;
        let imp: unsafe extern "C" fn() =
            std::mem::transmute(menu_for_event_nil as *const c_void);

        class_replaceMethod(
            cls as *mut _,
            sel,
            Some(imp),
            types,
        );

        // reference sel! so the selector constant is registered once at startup
        let _ = sel!(menuForEvent:);
        info!("native WKWebView context menu suppressed");
    });

    if let Err(e) = res {
        warn!("with_webview failed: {e}");
    }
}
