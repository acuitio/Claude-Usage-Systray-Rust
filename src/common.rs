// Shared constants, palette, fonts, and tiny helpers used across modules.

use std::ptr::null_mut;
use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// ─── Palette (COLORREF = 0x00BBGGRR) ──────────────────────────────────
pub const BG_DARK: u32      = 0x002e_1e1e;
pub const FG_LIGHT: u32     = 0x00ff_ffff;
pub const SURFACE: u32      = 0x0025_2525;
pub const BORDER: u32       = 0x0033_3333;
pub const ACCENT: u32       = 0x00c8_d456;
pub const GREEN: u32        = 0x0033_ff33;
pub const YELLOW: u32       = 0x0032_c8e6;
pub const RED: u32          = 0x0050_50e6;

// SS_ETCHEDFRAME not exposed by windows-sys; raw value from winuser.h.
pub const SS_ETCHEDFRAME: u32 = 0x0000_0016;

// ─── Cached resources (UI thread only) ────────────────────────────────
pub static mut HBR_BG: HBRUSH = null_mut();
pub static mut HBR_SURFACE: HBRUSH = null_mut();
pub static mut FONT_REG: HFONT = null_mut();
pub static mut FONT_BOLD: HFONT = null_mut();

pub unsafe fn init_resources() {
    HBR_BG      = CreateSolidBrush(BG_DARK);
    HBR_SURFACE = CreateSolidBrush(SURFACE);
    FONT_REG    = make_font(-14, FW_NORMAL as i32);
    FONT_BOLD   = make_font(-14, FW_BOLD as i32);
}

unsafe fn make_font(height: i32, weight: i32) -> HFONT {
    CreateFontW(
        height, 0, 0, 0, weight,
        0, 0, 0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        (DEFAULT_PITCH | FF_DONTCARE) as u32,
        windows_sys::w!("Segoe UI"),
    )
}

// ─── Small helpers ────────────────────────────────────────────────────
pub fn wstr(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn color_hex(c: u32) -> Vec<u16> {
    let r = c & 0xff;
    let g = (c >> 8) & 0xff;
    let b = (c >> 16) & 0xff;
    let s = format!("#{:02x}{:02x}{:02x}\0", r, g, b);
    s.encode_utf16().collect()
}

pub fn pct_color(pct: f64) -> u32 {
    if pct < 50.0 { GREEN }
    else if pct < 90.0 { YELLOW }
    else { RED }
}

pub unsafe fn create_child(
    parent: HWND, class: PCWSTR, text: PCWSTR,
    style: u32, x: i32, y: i32, w: i32, h: i32, id: u16,
) -> HWND {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    CreateWindowExW(
        0, class, text, style,
        x, y, w, h,
        parent,
        id as HMENU,
        GetModuleHandleW(std::ptr::null()),
        std::ptr::null(),
    )
}

pub unsafe fn set_font(hwnd: HWND, font: HFONT) {
    SendMessageW(hwnd, WM_SETFONT, font as WPARAM, 1);
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
    pub session_reset_iso: Option<String>,
    pub weekly_reset_iso:  Option<String>,
    pub extra: Option<crate::models::ExtraUsage>,
    pub plan:  String,
}

pub fn current_snapshot() -> UsageData {
    let plan = crate::credentials::plan_label(
        crate::credentials::read_full().as_ref());

    match crate::usage_cache::load().and_then(|c| c.data) {
        Some(d) => UsageData {
            session_pct: d.five_hour.as_ref().map(|m| m.utilization).unwrap_or(0.0),
            weekly_pct:  d.seven_day.as_ref().map(|m| m.utilization).unwrap_or(0.0),
            sonnet_pct:  d.seven_day_sonnet.as_ref().map(|m| m.utilization).unwrap_or(0.0),
            session_reset_iso: d.five_hour.as_ref().and_then(|m| m.resets_at.clone()),
            weekly_reset_iso:  d.seven_day.as_ref().and_then(|m| m.resets_at.clone()),
            extra: d.extra_usage,
            plan,
        },
        None => UsageData {
            session_pct: 0.0, weekly_pct: 0.0, sonnet_pct: 0.0,
            session_reset_iso: None, weekly_reset_iso: None,
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
    for m in 0..(mo as usize).saturating_sub(1).min(12) { days += mdays[m]; }
    days += d as i64 - 1;
    days * 86_400 + (h as i64) * 3600 + (mn as i64) * 60
}

fn is_leap(y: i32) -> bool { (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 }
