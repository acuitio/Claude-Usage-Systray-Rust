// Tray icon + context menu. Matches src/App/TrayApp.cs behaviour.

use std::cell::RefCell;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, Ordering};
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;
use crate::{chart, dashboard, overlay, settings};

pub const WM_TRAY_CALLBACK: u32 = WM_APP + 1;

// Menu command IDs.
const ID_MENU_DASHBOARD: u16 = 9001;
const ID_MENU_OVERLAY:   u16 = 9002;
const ID_MENU_REFRESH:   u16 = 9003;
const ID_MENU_SETTINGS:  u16 = 9004;
const ID_MENU_CHART:     u16 = 9006;
const ID_MENU_IMGPASTE:   u16 = 9007;
const ID_MENU_QUIT:      u16 = 9005;

// The hidden host window that receives our tray callback. Set once at
// install() and read by refresh()/remove() — single-thread access in practice,
// but `AtomicPtr` is the cheapest way to satisfy Rust without `static mut`.
static TRAY_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(null_mut());

// Icon cache. Rebuilding the 64×64 icon on every poll wastes GDI cycles when
// the rounded percentage and color tier haven't changed. Keyed on
// (pct_int, color); only NIM_MODIFY when those move.
struct IconCache {
    pct_int: i32,
    color:   u32,
    icon:    HICON,
}
thread_local! {
    static ICON_CACHE: RefCell<Option<IconCache>> = const { RefCell::new(None) };
}

pub unsafe fn install(host: HWND) {
    TRAY_HWND.store(host, Ordering::Relaxed);

    let usage = current_snapshot();
    let icon  = get_or_build_icon(usage.session_pct);
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd   = host;
    nid.uID    = 1;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY_CALLBACK;
    nid.hIcon  = icon;
    write_tooltip(&mut nid.szTip, &usage);
    Shell_NotifyIconW(NIM_ADD, &nid);
}

pub unsafe fn handle_tray_callback(host: HWND, lp: LPARAM) {
    let event = (lp & 0xffff) as u32;
    if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);

        let menu = CreatePopupMenu();
        let dash_open    = dashboard::is_open();
        let overlay_open = overlay::is_open();
        AppendMenuW(menu,
            MF_STRING | if dash_open { MF_CHECKED } else { MF_UNCHECKED },
            ID_MENU_DASHBOARD as usize, w!("Dashboard"));
        AppendMenuW(menu,
            MF_STRING | if overlay_open { MF_CHECKED } else { MF_UNCHECKED },
            ID_MENU_OVERLAY as usize, w!("Overlay"));
        AppendMenuW(menu,
            MF_STRING | if chart::is_open() { MF_CHECKED } else { MF_UNCHECKED },
            ID_MENU_CHART as usize, w!("Usage Chart"));
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

        AppendMenuW(menu, MF_STRING, ID_MENU_REFRESH as usize,  w!("Refresh Now"));
        AppendMenuW(menu, MF_STRING, ID_MENU_SETTINGS as usize, w!("Settings"));
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

        AppendMenuW(menu, MF_STRING, ID_MENU_IMGPASTE as usize,
            w!("Send Clipboard Image/Files\tAlt+Shift+V"));
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

        AppendMenuW(menu, MF_STRING, ID_MENU_QUIT as usize, w!("Quit"));

        SetForegroundWindow(host);
        let cmd = TrackPopupMenu(
            menu, TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_BOTTOMALIGN,
            pt.x, pt.y, 0, host, std::ptr::null(),
        );
        DestroyMenu(menu);

        match cmd as u16 {
            ID_MENU_DASHBOARD => dashboard::open(host),
            ID_MENU_OVERLAY   => overlay::toggle(host),
            ID_MENU_REFRESH   => { crate::poll_service::trigger_refresh(); }
            ID_MENU_CHART     => chart::open(host),
            ID_MENU_SETTINGS  => settings::open(host),
            ID_MENU_IMGPASTE   => crate::imgpaste::handle_hotkey(),
            ID_MENU_QUIT      => { remove(); PostQuitMessage(0); }
            _ => {}
        }
    }
}

/// Re-read cache + credentials and push the updated icon + tooltip into
/// the tray. Called from the host window's WM_USAGE_UPDATED handler.
pub unsafe fn refresh() {
    let host = TRAY_HWND.load(Ordering::Relaxed);
    if host.is_null() { return; }
    let usage = current_snapshot();
    let icon  = get_or_build_icon(usage.session_pct);
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd   = host;
    nid.uID    = 1;
    nid.uFlags = NIF_ICON | NIF_TIP;
    nid.hIcon  = icon;
    write_tooltip(&mut nid.szTip, &usage);
    Shell_NotifyIconW(NIM_MODIFY, &nid);
}

unsafe fn write_tooltip(buf: &mut [u16], usage: &crate::common::UsageData) {
    let cd = crate::cooldown::remaining_seconds();
    let suffix = if cd > 0 {
        format!(" · Rate-limited {}", crate::cooldown::format(cd))
    } else {
        String::new()
    };
    let tip_str = format!(
        "Usage: {:.0}% | {:.0}% | {:.0}%\n{}{}",
        usage.session_pct, usage.weekly_pct, usage.sonnet_pct, usage.plan, suffix,
    );
    let tip = wstr(&tip_str);
    let limit = buf.len().min(127);
    for (i, c) in tip.iter().take(limit).enumerate() {
        buf[i] = *c;
    }
}

pub unsafe fn remove() {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd   = TRAY_HWND.load(Ordering::Relaxed);
    nid.uID    = 1;
    Shell_NotifyIconW(NIM_DELETE, &nid);
}

/// Return the cached HICON if (rounded-pct, color-tier) match the last call;
/// otherwise rebuild, destroy the previous handle, and cache the new one.
unsafe fn get_or_build_icon(pct: f64) -> HICON {
    let pct_int = pct.round() as i32;
    let color   = pct_color(pct);
    ICON_CACHE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if let Some(c) = slot.as_ref() {
            if c.pct_int == pct_int && c.color == color {
                return c.icon;
            }
            DestroyIcon(c.icon);
        }
        let icon = build_tray_icon(pct);
        *slot = Some(IconCache { pct_int, color, icon });
        icon
    })
}

// Render the bar-chart icon at 64×64, then convert to HICON.
unsafe fn build_tray_icon(pct: f64) -> HICON {
    let hdc_screen = GetDC(null_mut());
    let hdc_mem    = CreateCompatibleDC(hdc_screen);
    let bmp        = CreateCompatibleBitmap(hdc_screen, 64, 64);
    let old_bmp    = SelectObject(hdc_mem, bmp as HGDIOBJ);

    let brush_bg = CreateSolidBrush(0x002e_1e1e);
    let rc = RECT { left: 0, top: 0, right: 64, bottom: 64 };
    FillRect(hdc_mem, &rc, brush_bg);
    DeleteObject(brush_bg as HGDIOBJ);

    let brush_inner = CreateSolidBrush(0x0046_3232);
    let rc_inner = RECT { left: 4, top: 8, right: 60, bottom: 56 };
    FillRect(hdc_mem, &rc_inner, brush_inner);
    DeleteObject(brush_inner as HGDIOBJ);

    let bar_h = (48.0 * pct.min(100.0) / 100.0) as i32;
    if bar_h > 0 {
        let brush_bar = CreateSolidBrush(pct_color(pct));
        let rc_bar = RECT { left: 4, top: 56 - bar_h, right: 60, bottom: 56 };
        FillRect(hdc_mem, &rc_bar, brush_bar);
        DeleteObject(brush_bar as HGDIOBJ);
    }

    let font = CreateFontW(
        18, 0, 0, 0, FW_BOLD as i32,
        0, 0, 0, DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32, CLEARTYPE_QUALITY as u32,
        (DEFAULT_PITCH | FF_DONTCARE) as u32, w!("Arial"),
    );
    let old_font = SelectObject(hdc_mem, font as HGDIOBJ);
    SetBkMode(hdc_mem, TRANSPARENT as i32);
    SetTextColor(hdc_mem, 0x00ff_ffff);
    let txt = wstr(&format!("{}", pct as i32));
    let rc_txt = RECT { left: 0, top: 56, right: 64, bottom: 76 };
    DrawTextW(hdc_mem, txt.as_ptr(), -1, &rc_txt as *const _ as *mut _,
              DT_CENTER | DT_TOP | DT_SINGLELINE);
    SelectObject(hdc_mem, old_font);
    DeleteObject(font as HGDIOBJ);

    SelectObject(hdc_mem, old_bmp);
    DeleteDC(hdc_mem);
    ReleaseDC(null_mut(), hdc_screen);

    let mut mask_bits: [u8; 64 * 64 / 8] = [0; 64 * 64 / 8];
    let mask = CreateBitmap(64, 64, 1, 1, mask_bits.as_mut_ptr() as _);
    let ii = ICONINFO {
        fIcon: 1, xHotspot: 0, yHotspot: 0,
        hbmMask: mask, hbmColor: bmp,
    };
    let icon = CreateIconIndirect(&ii);
    DeleteObject(bmp as HGDIOBJ);
    DeleteObject(mask as HGDIOBJ);
    icon
}
