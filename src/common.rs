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

// ─── Dummy usage data (a real port would call the Anthropic API) ──────
pub struct UsageData {
    pub session_pct: f64,
    pub weekly_pct:  f64,
    pub sonnet_pct:  f64,
    pub session_reset: &'static str,
    pub weekly_reset:  &'static str,
}

pub fn dummy_usage() -> UsageData {
    UsageData {
        session_pct: 29.0, weekly_pct: 11.0, sonnet_pct: 0.0,
        session_reset: "3h 14m", weekly_reset: "4d 2h 14m",
    }
}
