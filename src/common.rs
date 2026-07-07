// Shared constants, palette, and tiny helpers used across modules.

use std::sync::atomic::{AtomicPtr, Ordering};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// ─── Palette (COLORREF = 0x00BBGGRR) ──────────────────────────────────
pub const BG_DARK: u32 = 0x002e_1e1e;
pub const GREEN:   u32 = 0x0033_ff33;
pub const YELLOW:  u32 = 0x0032_c8e6;
pub const RED:     u32 = 0x0050_50e6;

// Cached background brush, shared by the WebView2 host windows
// (dashboard / settings / chart). Single-init at startup; never freed
// — process exit is fine.
static HBR_BG_PTR: AtomicPtr<core::ffi::c_void> =
    AtomicPtr::new(std::ptr::null_mut());

pub fn hbr_bg() -> HBRUSH {
    HBR_BG_PTR.load(Ordering::Relaxed)
}

pub unsafe fn init_resources() {
    HBR_BG_PTR.store(CreateSolidBrush(BG_DARK), Ordering::Relaxed);
}

// ─── Small helpers ────────────────────────────────────────────────────
pub fn wstr(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// "Ctrl+Alt+Shift+V"-style label for a Win32 hotkey (MOD_* bitmask + VK code).
/// Mirrors chordText() in assets/settings.html so UI surfaces agree.
pub fn chord_label(mods: u32, vk: u32) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if mods & 0x0002 != 0 { parts.push("Ctrl"); }
    if mods & 0x0001 != 0 { parts.push("Alt"); }
    if mods & 0x0004 != 0 { parts.push("Shift"); }
    if mods & 0x0008 != 0 { parts.push("Win"); }
    let key = match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(), // 0-9, A-Z
        0x70..=0x87 => format!("F{}", vk - 0x6F),                      // F1-F24
        _ => format!("0x{vk:X}"),
    };
    let mut s = parts.join("+");
    if !s.is_empty() { s.push('+'); }
    s + &key
}

/// Parse "#RRGGBB" → COLORREF (0x00BBGGRR). Returns None on malformed input.
pub fn hex_to_colorref(s: &str) -> Option<u32> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 { return None; }
    let r = u32::from_str_radix(&s[0..2], 16).ok()?;
    let g = u32::from_str_radix(&s[2..4], 16).ok()?;
    let b = u32::from_str_radix(&s[4..6], 16).ok()?;
    Some(r | (g << 8) | (b << 16))
}

pub fn pct_color(pct: f64) -> u32 {
    if pct < 50.0 { GREEN }
    else if pct < 90.0 { YELLOW }
    else { RED }
}

/// WM_DPICHANGED handler. Windows supplies a suggested RECT in lParam with
/// the new monitor's DPI-scaled coordinates; we just honor it. Per-monitor
/// V2 DPI awareness auto-scales common controls; custom-painted regions
/// use device units and stay correct as long as we resize the parent.
///
/// wParam encodes the new DPI but we don't need it for our usage.
pub unsafe fn handle_dpi_changed(hwnd: HWND, lp: LPARAM) {
    let rc = lp as *const RECT;
    if rc.is_null() { return; }
    let r = *rc;
    SetWindowPos(hwnd, std::ptr::null_mut(),
        r.left, r.top, r.right - r.left, r.bottom - r.top,
        SWP_NOZORDER | SWP_NOACTIVATE);
    InvalidateRect(hwnd, std::ptr::null(), 0);
}

// ─── Current usage snapshot — reads cache + credentials ───────────────
// Falls back to zeros + "Unknown" plan if no cache yet. The HTTP layer
// (future work) populates the cache; UI just reads it.

pub struct UsageData {
    pub session_pct: f64,
    pub weekly_pct:  f64,
    pub sonnet_pct:  f64,
    pub fable_pct:   f64,
    pub session_reset_iso: Option<String>,
    pub weekly_reset_iso:  Option<String>,
    pub fable_reset_iso:   Option<String>,
    pub extra: Option<crate::models::ExtraUsage>,
    pub plan:  String,
}

pub fn current_snapshot() -> UsageData {
    let plan = crate::credentials::plan_label(
        crate::credentials::read_full().as_ref());

    match crate::usage_cache::load().and_then(|c| c.data) {
        Some(d) => {
            // Extract Fable into owned locals *before* the struct literal —
            // fable_limit() borrows `d`, but `extra: d.extra_usage` moves out
            // of `d`, so the borrow must end first.
            let (fable_pct, fable_reset_iso) = match d.fable_limit() {
                Some(l) => (l.percent, l.resets_at.clone()),
                None    => (0.0, None),
            };
            UsageData {
                session_pct: d.five_hour.as_ref().map(|m| m.utilization).unwrap_or(0.0),
                weekly_pct:  d.seven_day.as_ref().map(|m| m.utilization).unwrap_or(0.0),
                sonnet_pct:  d.seven_day_sonnet.as_ref().map(|m| m.utilization).unwrap_or(0.0),
                fable_pct,
                session_reset_iso: d.five_hour.as_ref().and_then(|m| m.resets_at.clone()),
                weekly_reset_iso:  d.seven_day.as_ref().and_then(|m| m.resets_at.clone()),
                fable_reset_iso,
                extra: d.extra_usage,
                plan,
            }
        }
        None => UsageData {
            session_pct: 0.0, weekly_pct: 0.0, sonnet_pct: 0.0, fable_pct: 0.0,
            session_reset_iso: None, weekly_reset_iso: None, fable_reset_iso: None,
            extra: None, plan,
        },
    }
}

/// "Resets in 3h 14m" style — accepts an ISO 8601 timestamp string.
/// Returns "—" if missing, "unknown" if unparsable. Minimal hand-rolled
/// parser to avoid pulling in the chrono crate for this one feature.
pub fn format_reset(iso: Option<&str>) -> String {
    let Some(iso) = iso else { return "—".into() };
    let core = iso.split('.').next().unwrap_or(iso);
    let parts: Vec<&str> = core.split('T').collect();
    if parts.len() != 2 { return "unknown".into(); }
    let dparts: Vec<&str> = parts[0].split('-').collect();
    let time = parts[1].trim_end_matches('Z');
    let tparts: Vec<&str> = time.split(':').collect();
    if dparts.len() != 3 || tparts.len() < 2 { return "unknown".into(); }
    let (Ok(y), Ok(mo), Ok(d), Ok(hr), Ok(mn)) = (
        dparts[0].parse::<i32>(), dparts[1].parse::<u32>(), dparts[2].parse::<u32>(),
        tparts[0].parse::<u32>(), tparts[1].parse::<u32>(),
    ) else { return "unknown".into() };
    let target = unix_from_ymdhm(y, mo, d, hr, mn);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let delta = (target - now).max(0);
    let total_min = delta / 60;
    let h = total_min / 60;
    let m = total_min % 60;
    if h >= 24 {
        let days = h / 24;
        let h = h % 24;
        format!("{days}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

// Naive UTC date → unix-seconds (Gregorian, sufficient for 2026+).
fn unix_from_ymdhm(y: i32, mo: u32, d: u32, h: u32, mn: u32) -> i64 {
    let mut days: i64 = 0;
    for year in 1970..y {
        days += if is_leap(year) { 366 } else { 365 };
    }
    let mdays = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    for d in &mdays[..(mo as usize).saturating_sub(1).min(12)] { days += *d as i64; }
    days += d as i64 - 1;
    days * 86_400 + (h as i64) * 3600 + (mn as i64) * 60
}

fn is_leap(y: i32) -> bool { (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 }

/// Clamp `(x, y)` to the full-monitor area of whichever monitor it's
/// closest to. We use rcMonitor (not rcWork) so the user can drag the
/// overlay over the taskbar's footprint; the topmost re-assertion timer
/// keeps it visually above.
pub unsafe fn clamp_to_virtual_screen(x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    let pt = POINT { x: x + w / 2, y: y + h / 2 };
    let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
    let mut mi: MONITORINFO = std::mem::zeroed();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    let rc = if GetMonitorInfoW(monitor, &mut mi) != 0 {
        mi.rcMonitor
    } else {
        RECT {
            left: 0, top: 0,
            right:  GetSystemMetrics(SM_CXSCREEN),
            bottom: GetSystemMetrics(SM_CYSCREEN),
        }
    };
    let max_x = (rc.right  - w).max(rc.left);
    let max_y = (rc.bottom - h).max(rc.top);
    (x.clamp(rc.left, max_x), y.clamp(rc.top, max_y))
}
