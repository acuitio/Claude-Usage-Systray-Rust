// Dashboard window — now a thin Win32 host for a WebView2 control. The
// actual rendering lives in assets/dashboard.html (HTML + CSS + a sliver
// of JS). Rust pushes a JSON snapshot to JS via evaluate_script and
// listens for "refresh" / "open_account_settings" messages back.

use std::cell::RefCell;
use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;
use crate::webview_host::ParentWindow;
use wry::WebViewBuilderExtWindows;

static mut HWND_DASH: HWND = null_mut();

thread_local! {
    static WEBVIEW: RefCell<Option<wry::WebView>> = const { RefCell::new(None) };
}

const DASHBOARD_HTML: &str = include_str!("../assets/dashboard.html");

pub unsafe fn is_open() -> bool {
    !HWND_DASH.is_null() && IsWindow(HWND_DASH) != 0
}

pub unsafe fn on_data_changed() {
    if is_open() { push_snapshot(); }
}

pub unsafe fn open(_owner: HWND) {
    if is_open() {
        if GetWindowLongW(HWND_DASH, GWL_STYLE) & WS_MINIMIZE as i32 != 0 {
            ShowWindow(HWND_DASH, SW_RESTORE);
        }
        SetForegroundWindow(HWND_DASH);
        return;
    }

    let class_name = w!("Win32DashboardWebView");
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

    let mut wa: RECT = std::mem::zeroed();
    SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0);
    let dw = 540;
    let dh = 560;
    let x = wa.left + ((wa.right - wa.left) - dw) / 2;
    let y = wa.top + ((wa.bottom - wa.top) - dh) / 2;

    HWND_DASH = CreateWindowExW(
        0, class_name, w!("Dashboard"),
        WS_OVERLAPPEDWINDOW | WS_VISIBLE,
        x, y, dw, dh,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    SetWindowPos(HWND_DASH, HWND_TOP, 0, 0, 0, 0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
    UpdateWindow(HWND_DASH);
    SetForegroundWindow(HWND_DASH);
}

unsafe fn attach_webview(parent_hwnd: HWND) {
    let parent = ParentWindow(parent_hwnd);
    let result = wry::WebViewBuilder::new_as_child(&parent)
        .with_html(DASHBOARD_HTML)
        .with_ipc_handler(handle_ipc)
        .with_transparent(false)
        .build();

    match result {
        Ok(webview) => {
            // Initial size will be applied via WM_SIZE; this seeds it.
            resize_webview_to_parent(&webview, parent_hwnd);
            WEBVIEW.with(|c| *c.borrow_mut() = Some(webview));
        }
        Err(e) => {
            let msg = wstr(&format!(
                "Failed to initialize WebView2:\n{e}\n\n\
                Make sure the WebView2 Runtime is installed (built into Windows 11)."
            ));
            MessageBoxW(parent_hwnd, msg.as_ptr(), w!("Dashboard"),
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

/// Build the snapshot JSON the JS expects.
fn build_snapshot_json() -> String {
    let cfg   = crate::config_store::load();
    let usage = crate::common::current_snapshot();
    let session_reset = crate::common::format_reset(usage.session_reset_iso.as_deref());
    let weekly_reset  = crate::common::format_reset(usage.weekly_reset_iso.as_deref());

    // "Updated Xs ago" — derived from the cache file's refresh timestamp.
    let last_refresh_ago = crate::usage_cache::load()
        .and_then(|c| if c.age_seconds.is_finite() { Some(c.age_seconds as i64) } else { None })
        .map(|secs| {
            if secs < 60 { format!("{secs}s ago") }
            else if secs < 3600 { format!("{}m ago", secs / 60) }
            else { format!("{}h {}m ago", secs / 3600, (secs % 3600) / 60) }
        });

    let extra_json = match usage.extra.as_ref() {
        Some(e) => serde_json::json!({
            "is_enabled":    e.is_enabled,
            "used_credits":  e.used_credits,
            "monthly_limit": e.monthly_limit,
        }),
        None => serde_json::Value::Null,
    };

    let v = serde_json::json!({
        "plan":           usage.plan,
        "email":          "",  // not yet fetched from /profile in this port
        "session_pct":    usage.session_pct,
        "weekly_pct":     usage.weekly_pct,
        "sonnet_pct":     usage.sonnet_pct,
        "session_reset":  session_reset,
        "weekly_reset":   weekly_reset,
        "session_eta":    depletion_eta(1),
        "weekly_eta":     depletion_eta(2),
        "sonnet_eta":     depletion_eta(3),
        "show_session":   cfg.show_session,
        "show_weekly":    cfg.show_weekly,
        "show_sonnet":    cfg.show_sonnet,
        "last_refresh_ago": last_refresh_ago,
        "extra":          extra_json,
    });
    v.to_string()
}

/// Estimate "how long until this metric hits 100%". Simple linear projection
/// from the trend of the last two history samples. Mirrors the C# logic.
fn depletion_eta(metric_idx: usize) -> Option<String> {
    let hist = crate::usage_history::load();
    if hist.len() < 2 { return None; }
    let first = &hist[0];
    let last  = &hist[hist.len() - 1];
    let dt = last[0] - first[0];
    if dt < 60.0 { return Some("need more time".into()); }
    let dp = last[metric_idx] - first[metric_idx];
    if dp <= 0.0 { return Some("never at current rate".into()); }
    let minutes = ((100.0 - last[metric_idx]) / (dp / dt)) / 60.0;
    if minutes < 60.0 { Some(format!("~{:.0}m", minutes)) }
    else { Some(format!("~{:.0}h {:.0}m", minutes / 60.0, minutes % 60.0)) }
}

fn push_snapshot() {
    let snap = build_snapshot_json();
    let js = format!("window.applySnapshot && window.applySnapshot({snap})");
    WEBVIEW.with(|c| {
        if let Some(wv) = c.borrow().as_ref() {
            let _ = wv.evaluate_script(&js);
        }
    });
}

fn handle_ipc(req: wry::http::Request<String>) {
    let Ok(value): Result<serde_json::Value, _> = serde_json::from_str(req.body()) else { return; };
    let cmd = value.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
    match cmd {
        "ready"   => push_snapshot(),
        "refresh" => crate::poll_service::trigger_refresh(),
        "open_account_settings" => {
            let _ = std::process::Command::new("rundll32")
                .args(["url.dll,FileProtocolHandler", "https://claude.ai/settings/usage"])
                .spawn();
        }
        _ => {}
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => { attach_webview(hwnd); 0 }
            WM_SIZE   => {
                WEBVIEW.with(|c| {
                    if let Some(wv) = c.borrow().as_ref() {
                        resize_webview_to_parent(wv, hwnd);
                    }
                });
                0
            }
            WM_DPICHANGED => { handle_dpi_changed(hwnd, lp); 0 }
            WM_CLOSE      => { DestroyWindow(hwnd); 0 }
            WM_DESTROY => {
                WEBVIEW.with(|c| { c.borrow_mut().take(); });
                HWND_DASH = null_mut();
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
