// Settings window — WebView2-rendered. The actual form lives in
// assets/settings.html. Rust pushes the current AppConfig (as JSON) to JS
// when the page loads, and listens for "apply" / "save_exit" / "cancel" /
// "reset_defaults" messages back.

use std::cell::RefCell;
use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;
use crate::webview_host::ParentWindow;
use crate::{config_store, models::AppConfig};
use wry::WebViewBuilderExtWindows;

static mut HWND_SETTINGS: HWND = null_mut();

thread_local! {
    static WEBVIEW: RefCell<Option<wry::WebView>> = const { RefCell::new(None) };
}

const SETTINGS_HTML: &str = include_str!("../assets/settings.html");

pub unsafe fn open(_owner: HWND) {
    if !HWND_SETTINGS.is_null() && IsWindow(HWND_SETTINGS) != 0 {
        SetForegroundWindow(HWND_SETTINGS);
        return;
    }
    let class_name = w!("Win32SettingsWebView");
    let instance = GetModuleHandleW(std::ptr::null());

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0, cbWndExtra: 0,
        hInstance: instance,
        hIcon: null_mut(),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: HBR_BG,
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name,
        hIconSm: null_mut(),
    };
    RegisterClassExW(&wc);

    HWND_SETTINGS = CreateWindowExW(
        0, class_name, w!("Settings"),
        WS_OVERLAPPEDWINDOW | WS_VISIBLE,
        CW_USEDEFAULT, CW_USEDEFAULT, 620, 820,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    SetWindowPos(HWND_SETTINGS, HWND_TOP, 0, 0, 0, 0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
    UpdateWindow(HWND_SETTINGS);
    SetForegroundWindow(HWND_SETTINGS);
}

unsafe fn attach_webview(parent_hwnd: HWND) {
    let parent = ParentWindow(parent_hwnd);
    let result = wry::WebViewBuilder::new_as_child(&parent)
        .with_html(SETTINGS_HTML)
        .with_ipc_handler(handle_ipc)
        .with_on_page_load_handler(|_event, _url| push_config())
        .with_transparent(false)
        .build();

    match result {
        Ok(webview) => {
            resize_webview_to_parent(&webview, parent_hwnd);
            WEBVIEW.with(|c| *c.borrow_mut() = Some(webview));
        }
        Err(e) => {
            let msg = wstr(&format!("Failed to initialize WebView2:\n{e}"));
            MessageBoxW(parent_hwnd, msg.as_ptr(), w!("Settings"),
                        MB_OK | MB_ICONERROR);
        }
    }
}

fn resize_webview_to_parent(webview: &wry::WebView, parent: HWND) {
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(parent, &mut rc); }
    let _ = webview.set_bounds(wry::Rect {
        position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
        size:     wry::dpi::PhysicalSize::new(
            (rc.right - rc.left) as u32,
            (rc.bottom - rc.top) as u32,
        ).into(),
    });
}

/// Serialize the current on-disk config and ship it to the page.
fn push_config() {
    let cfg = config_store::load();
    match serde_json::to_string(&cfg) {
        Ok(json) => {
            let js = format!("window.applyConfig && window.applyConfig({json})");
            WEBVIEW.with(|c| {
                if let Some(wv) = c.borrow().as_ref() {
                    let _ = wv.evaluate_script(&js);
                }
            });
        }
        Err(e) => eprintln!("settings: serialize config failed: {e}"),
    }
}

fn handle_ipc(req: wry::http::Request<String>) {
    let Ok(msg): Result<serde_json::Value, _> = serde_json::from_str(req.body()) else { return; };
    let cmd = msg.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
    match cmd {
        "ready" => push_config(),
        "apply" => {
            if let Some(cfg_val) = msg.get("cfg") {
                save_with_side_effects(cfg_val);
            }
        }
        "save_exit" => {
            if let Some(cfg_val) = msg.get("cfg") {
                save_with_side_effects(cfg_val);
            }
            close_window();
        }
        "cancel" => close_window(),
        "reset_defaults" => {
            let defaults = AppConfig::default();
            if let Ok(json) = serde_json::to_string(&defaults) {
                let js = format!("window.applyConfig && window.applyConfig({json})");
                WEBVIEW.with(|c| {
                    if let Some(wv) = c.borrow().as_ref() {
                        let _ = wv.evaluate_script(&js);
                    }
                });
            }
        }
        _ => {}
    }
}

fn save_with_side_effects(cfg_val: &serde_json::Value) {
    let new_cfg: AppConfig = match serde_json::from_value(cfg_val.clone()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("settings: failed to deserialize incoming config: {e}");
            return;
        }
    };
    let old_cfg = config_store::load();

    // Sync the HKCU Run key if startup-on-login toggle changed.
    if new_cfg.start_on_startup != old_cfg.start_on_startup {
        if new_cfg.start_on_startup {
            if let Ok(exe) = std::env::current_exe() {
                crate::startup_registry::set_enabled(Some(&exe.to_string_lossy()));
            }
        } else {
            crate::startup_registry::set_enabled(None);
        }
    }

    if let Err(e) = config_store::save(&new_cfg) {
        eprintln!("settings: save failed: {e}");
        return;
    }

    // Push the new config out to all live UI surfaces.
    crate::poll_service::notify_ui_refresh();
}

fn close_window() {
    unsafe {
        if !HWND_SETTINGS.is_null() {
            PostMessageW(HWND_SETTINGS, WM_CLOSE, 0, 0);
        }
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => { attach_webview(hwnd); 0 }
            WM_SIZE => {
                WEBVIEW.with(|c| {
                    if let Some(wv) = c.borrow().as_ref() {
                        resize_webview_to_parent(wv, hwnd);
                    }
                });
                0
            }
            WM_DPICHANGED => { handle_dpi_changed(hwnd, lp); 0 }
            WM_CLOSE => { DestroyWindow(hwnd); 0 }
            WM_DESTROY => {
                WEBVIEW.with(|c| { c.borrow_mut().take(); });
                HWND_SETTINGS = null_mut();
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
