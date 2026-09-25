// Dashboard window — now a thin Win32 host for a WebView2 control. The
// actual rendering lives in assets/dashboard.html (HTML + CSS + a sliver
// of JS). Rust pushes a JSON snapshot to JS via evaluate_script and
// listens for "refresh" / "open_account_settings" messages back.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, Ordering};
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;
use crate::webview_host::ParentWindow;

static HWND_DASH: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
fn dash_hwnd() -> HWND { HWND_DASH.load(Ordering::Relaxed) }

thread_local! {
    static WEBVIEW: RefCell<Option<wry::WebView>> = const { RefCell::new(None) };
}

const DASHBOARD_HTML: &str = include_str!("../assets/dashboard.html");

pub unsafe fn is_open() -> bool {
    let h = dash_hwnd();
    !h.is_null() && IsWindow(h) != 0
}

pub unsafe fn on_data_changed() {
    if is_open() { push_snapshot(); }
}

pub unsafe fn open(_owner: HWND) {
    if is_open() {
        let h = dash_hwnd();
        if GetWindowLongW(h, GWL_STYLE) & WS_MINIMIZE as i32 != 0 {
            ShowWindow(h, SW_RESTORE);
        }
        SetForegroundWindow(h);
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
        hbrBackground: hbr_bg(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name,
        hIconSm: null_mut(),
    };
    RegisterClassExW(&wc);

    let mut cfg = crate::config_store::load();
    let (x, y, dw, dh) = match (cfg.dashboard_x, cfg.dashboard_y, cfg.dashboard_w, cfg.dashboard_h) {
        (Some(cx), Some(cy), Some(cw), Some(ch)) => (cx, cy, cw, ch),
        _ => {
            let mut wa: RECT = std::mem::zeroed();
            SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0);
            let dw = 540;
            let dh = 560;
            (
                wa.left + ((wa.right - wa.left) - dw) / 2,
                wa.top  + ((wa.bottom - wa.top) - dh) / 2,
                dw, dh,
            )
        }
    };
    // The monitor a saved position was on may have been unplugged since.
    let (x, y) = clamp_to_virtual_screen(x, y, dw, dh);

    let ex_style = if cfg.dashboard_on_top { WS_EX_TOPMOST } else { 0 };
    let hwnd = CreateWindowExW(
        ex_style, class_name, w!("Dashboard"),
        WS_OVERLAPPEDWINDOW | WS_VISIBLE,
        x, y, dw, dh,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    HWND_DASH.store(hwnd, Ordering::Relaxed);
    SetWindowPos(hwnd, HWND_TOP, 0, 0, 0, 0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
    UpdateWindow(hwnd);
    SetForegroundWindow(hwnd);

    // Mark dashboard as open so the next launch can auto-restore.
    if !cfg.dashboard_open {
        cfg.dashboard_open = true;
        let _ = crate::config_store::save(&cfg);
    }
}

/// Restore dashboard if it was open last session, at its last-saved rect.
pub unsafe fn open_if_persisted() {
    let cfg = crate::config_store::load();
    if cfg.dashboard_open && !is_open() {
        open(null_mut());
    }
}

/// Persist the current window rect (x/y/w/h) to config. Called on
/// WM_EXITSIZEMOVE and WM_DESTROY so the next launch can restore it.
unsafe fn persist_rect(hwnd: HWND) {
    // A minimized window reports (-32000, -32000); persisting that would
    // restore the dashboard permanently off-screen.
    if IsIconic(hwnd) != 0 { return; }
    let mut rc: RECT = std::mem::zeroed();
    if GetWindowRect(hwnd, &mut rc) == 0 { return; }
    let mut cfg = crate::config_store::load();
    cfg.dashboard_x = Some(rc.left);
    cfg.dashboard_y = Some(rc.top);
    cfg.dashboard_w = Some(rc.right  - rc.left);
    cfg.dashboard_h = Some(rc.bottom - rc.top);
    let _ = crate::config_store::save(&cfg);
}

unsafe fn attach_webview(parent_hwnd: HWND) {
    let parent = ParentWindow(parent_hwnd);
    let result = wry::WebViewBuilder::new_as_child(&parent)
        .with_html(DASHBOARD_HTML)
        .with_ipc_handler(handle_ipc)
        .with_on_page_load_handler(|_event, _url| {
            // Belt-and-braces: even if the "ready" IPC racing the WebView
            // initialization gets dropped, page-load always fires.
            push_snapshot();
        })
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
    let fable_reset   = crate::common::format_reset(usage.fable_reset_iso.as_deref());

    // "Updated Xs ago" — derived from the cache file's refresh timestamp.
    let last_refresh_ago = crate::usage_cache::load()
        .and_then(|c| if c.age_seconds.is_finite() { Some(c.age_seconds as i64) } else { None })
        .map(|secs| {
            if secs < 60 { format!("{secs}s ago") }
            else if secs < 3600 { format!("{}m ago", secs / 60) }
            else { format!("{}h {}m ago", secs / 3600, (secs % 3600) / 60) }
        });

    let hist = crate::usage_history::load();

    let extra_json = match usage.extra.as_ref() {
        Some(e) => serde_json::json!({
            "is_enabled":    e.is_enabled,
            "used_credits":  e.used_credits,
            "monthly_limit": e.monthly_limit,
        }),
        None => serde_json::Value::Null,
    };

    let quota_envelope = crate::state_store::load().quota_feed;
    let quota_data = quota_envelope.as_ref().and_then(|e| e.data.as_ref());
    let quota_interval = crate::quota_feed::poll_interval(quota_data);
    let now = unix_now();
    let alt = serde_json::json!({
        "session": quota_meter_json(quota_data, "claude_alt", "five_hour", now, quota_interval),
        "weekly": quota_meter_json(quota_data, "claude_alt", "seven_day", now, quota_interval),
        "fable": quota_meter_json(quota_data, "claude_alt", "seven_day_fable", now, quota_interval),
        "last_error": quota_envelope.as_ref().and_then(|e| e.last_error.clone()),
        "last_attempt_at": quota_envelope.as_ref().map(|e| e.last_attempt_at),
    });
    let codex = serde_json::json!({
        "weekly": quota_meter_json(quota_data, "codex", "codex_primary", now, quota_interval),
        "last_error": quota_envelope.as_ref().and_then(|e| e.last_error.clone()),
        "last_attempt_at": quota_envelope.as_ref().map(|e| e.last_attempt_at),
    });

    let v = serde_json::json!({
        "plan":           usage.plan,
        "session_pct":    usage.session_pct,
        "weekly_pct":     usage.weekly_pct,
        "fable_pct":      usage.fable_pct,
        "session_reset":  session_reset,
        "weekly_reset":   weekly_reset,
        "fable_reset":    fable_reset,
        "session_eta":    depletion_eta(&hist, 1),
        "weekly_eta":     depletion_eta(&hist, 2),
        "fable_eta":      depletion_eta(&hist, 3),
        "show_session":   cfg.show_session,
        "show_weekly":    cfg.show_weekly,
        "show_fable":     cfg.show_fable,
        "show_depletion_estimates": cfg.show_depletion_estimates,
        "show_last_refresh":        cfg.show_last_refresh,
        "quota_feed_enabled": !cfg.quota_feed_url.is_empty(),
        "show_alt": cfg.show_alt,
        "show_codex": cfg.show_codex,
        "alt": alt,
        "codex": codex,
        "last_refresh_ago": last_refresh_ago,
        "extra":          extra_json,
    });
    v.to_string()
}

fn quota_meter_json(
    data: Option<&crate::models::QuotaFeedResponse>, meter: &str, window: &str,
    now: f64, poll_interval: f64,
) -> serde_json::Value {
    let row = data.and_then(|response| crate::quota_feed::find(response, meter, window));
    serde_json::json!({
        "pct": row.and_then(|r| r.pct),
        "reset": row.and_then(|r| r.resets_at).map(format_unix_reset).unwrap_or_else(|| "--".into()),
        "projection": row.and_then(|r| r.projection.as_ref()).and_then(|p| p.label.clone()),
        "observed_at": row.and_then(|r| r.observed_at),
        "observed_age": row.and_then(|r| r.observed_at).map(|at| age_text(now - at)),
        "stale": row.map(|r| crate::quota_feed::is_stale(r.observed_at, now, poll_interval)).unwrap_or(true),
        "reason": row.and_then(|r| r.reason.clone()),
    })
}

fn age_text(age: f64) -> String {
    let secs = age.max(0.0) as i64;
    if secs < 60 { format!("{secs}s ago") }
    else if secs < 3600 { format!("{}m ago", secs / 60) }
    else { format!("{}h {}m ago", secs / 3600, (secs % 3600) / 60) }
}

pub(crate) fn format_unix_reset(reset_at: f64) -> String {
    if !reset_at.is_finite() { return "--".into(); }
    let seconds = (reset_at - unix_now()).max(0.0) as i64;
    let minutes = seconds / 60;
    if minutes >= 1440 { format!("{}d {}h", minutes / 1440, (minutes / 60) % 24) }
    else if minutes >= 60 { format!("{}h {}m", minutes / 60, minutes % 60) }
    else { format!("{minutes}m") }
}

fn unix_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Estimate "how long until this metric hits 100%". Linear projection from
/// the trend across the trailing ~2h of history (not the full up-to-500-
/// sample span, which can cross a weekly reset and make the trend garbage).
/// Mirrors the C# logic.
fn depletion_eta(hist: &crate::models::UsageHistory, metric_idx: usize) -> Option<String> {
    if hist.len() < 2 { return None; }
    let last = hist.last()?;
    let cutoff = last[0] - 7200.0;
    let first = hist.iter().find(|e| e[0] >= cutoff)?;
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
            WM_EXITSIZEMOVE => { persist_rect(hwnd); 0 }
            WM_DPICHANGED   => { handle_dpi_changed(hwnd, lp); 0 }
            WM_CLOSE        => { DestroyWindow(hwnd); 0 }
            WM_DESTROY => {
                persist_rect(hwnd);
                let mut cfg = crate::config_store::load();
                if cfg.dashboard_open {
                    cfg.dashboard_open = false;
                    let _ = crate::config_store::save(&cfg);
                }
                WEBVIEW.with(|c| { c.borrow_mut().take(); });
                HWND_DASH.store(null_mut(), Ordering::Relaxed);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
