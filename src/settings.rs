// Settings window — WebView2-rendered. The actual form lives in
// assets/settings.html. Rust pushes the current AppConfig (as JSON) to JS
// when the page loads, and listens for "apply" / "save_exit" / "cancel" /
// "reset_defaults" messages back.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, Ordering};
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;
use crate::webview_host::ParentWindow;
use crate::{config_store, models::AppConfig};

static HWND_SETTINGS: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
fn settings_hwnd() -> HWND { HWND_SETTINGS.load(Ordering::Relaxed) }

thread_local! {
    static WEBVIEW: RefCell<Option<wry::WebView>> = const { RefCell::new(None) };
}

const SETTINGS_HTML: &str = include_str!("../assets/settings.html");

pub unsafe fn open(_owner: HWND) {
    let existing = settings_hwnd();
    if !existing.is_null() && IsWindow(existing) != 0 {
        SetForegroundWindow(existing);
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
        hbrBackground: hbr_bg(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name,
        hIconSm: null_mut(),
    };
    RegisterClassExW(&wc);

    let hwnd = CreateWindowExW(
        0, class_name, w!("Settings"),
        WS_OVERLAPPEDWINDOW | WS_VISIBLE,
        CW_USEDEFAULT, CW_USEDEFAULT, 620, 820,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    HWND_SETTINGS.store(hwnd, Ordering::Relaxed);
    SetWindowPos(hwnd, HWND_TOP, 0, 0, 0, 0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
    UpdateWindow(hwnd);
    SetForegroundWindow(hwnd);
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
        "reset_defaults" => {
            // Defaults for everything the form shows — but keep window
            // geometry. Positions aren't represented in the form, so "Reset
            // to defaults" shouldn't teleport the overlay/dashboard; the page
            // stores this composite as its CFG, and a subsequent Apply
            // round-trips it wholesale.
            let old = config_store::load();
            let defaults = AppConfig {
                widget_x:       old.widget_x,
                widget_y:       old.widget_y,
                dashboard_open: old.dashboard_open,
                dashboard_x:    old.dashboard_x,
                dashboard_y:    old.dashboard_y,
                dashboard_w:    old.dashboard_w,
                dashboard_h:    old.dashboard_h,
                ..AppConfig::default()
            };
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
    let Ok(new_cfg): Result<AppConfig, _> = serde_json::from_value(cfg_val.clone()) else { return; };
    let old_cfg = config_store::load();

    // The page round-trips the FULL config object Rust pushed to it, mutating
    // only its form-controlled fields (see CFG in settings.html). Non-form
    // fields — enabled flags, anything added to AppConfig later — arrive back
    // verbatim, so new fields survive Apply without registration anywhere.
    //
    // One exception: window geometry + open-state are owned by the overlay
    // and dashboard windows and can change while Settings sits open (drag the
    // overlay, move the dashboard). The page's CFG snapshot would round-trip
    // the values from when Settings opened, silently reverting those live
    // changes — so take the freshest on-disk values for exactly this
    // "owned elsewhere" set. Unlike the old full preserve-list, forgetting a
    // future field here degrades to stale-while-Settings-open, not
    // wiped-on-Apply.
    let merged = AppConfig {
        widget_x:       old_cfg.widget_x,
        widget_y:       old_cfg.widget_y,
        dashboard_open: old_cfg.dashboard_open,
        dashboard_x:    old_cfg.dashboard_x,
        dashboard_y:    old_cfg.dashboard_y,
        dashboard_w:    old_cfg.dashboard_w,
        dashboard_h:    old_cfg.dashboard_h,
        ..new_cfg
    };

    // Sync the HKCU Run key if startup-on-login toggle changed.
    if merged.start_on_startup != old_cfg.start_on_startup {
        if merged.start_on_startup {
            if let Ok(exe) = std::env::current_exe() {
                crate::startup_registry::set_enabled(Some(&exe.to_string_lossy()));
            }
        } else {
            crate::startup_registry::set_enabled(None);
        }
    }

    if config_store::save(&merged).is_err() { return; }

    // Re-register the global hotkeys against the host window in case a chord
    // changed. UnregisterHotKey is by id; register_hotkey then reads the
    // freshly-saved config. No-op-safe if the host isn't up yet.
    let host = crate::tray::host_hwnd();
    if !host.is_null() {
        crate::imgpaste::unregister_hotkey(host);
        crate::imgpaste::register_hotkey(host);
        crate::imgpull::unregister_hotkey(host);
        crate::imgpull::register_hotkey(host);
    }

    crate::poll_service::notify_ui_refresh();
}

fn close_window() {
    unsafe {
        let h = settings_hwnd();
        if !h.is_null() {
            PostMessageW(h, WM_CLOSE, 0, 0);
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
            // Floor the resize-track size so the user can't shrink the
            // window below what the footer buttons need to render. Logical
            // pixels scaled by current monitor DPI.
            WM_GETMINMAXINFO => {
                let mmi = lp as *mut MINMAXINFO;
                if !mmi.is_null() {
                    let dpi = GetDpiForWindow(hwnd).max(96);
                    let scale = dpi as f32 / 96.0;
                    (*mmi).ptMinTrackSize.x = (500.0 * scale) as i32;
                    (*mmi).ptMinTrackSize.y = (400.0 * scale) as i32;
                }
                0
            }
            WM_CLOSE => { DestroyWindow(hwnd); 0 }
            WM_DESTROY => {
                WEBVIEW.with(|c| { c.borrow_mut().take(); });
                HWND_SETTINGS.store(null_mut(), Ordering::Relaxed);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
