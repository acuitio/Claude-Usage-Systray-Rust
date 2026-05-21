// Tray icon + context menu. Matches src/App/TrayApp.cs behaviour.

use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;
use crate::{dashboard, overlay, settings};

pub const WM_TRAY_CALLBACK: u32 = WM_APP + 1;

// Menu command IDs (kept separate from settings IDs)
const ID_MENU_DASHBOARD: u16 = 9001;
const ID_MENU_OVERLAY:   u16 = 9002;
const ID_MENU_REFRESH:   u16 = 9003;
const ID_MENU_SETTINGS:  u16 = 9004;
const ID_MENU_QUIT:      u16 = 9005;

static mut TRAY_HWND: HWND = null_mut();

pub unsafe fn install(host: HWND) {
    TRAY_HWND = host;

    let icon = build_tray_icon(29.0);
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = host;
    nid.uID = 1;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY_CALLBACK;
    nid.hIcon = icon;
    // Tooltip: "Usage: X% | Y% | Z%\n<plan>" — same as the C# version.
    let tip = wstr("Usage: 29% | 11% | 0%\nClaude Pro Max");
    for (i, c) in tip.iter().take(127).enumerate() {
        nid.szTip[i] = *c;
    }

    Shell_NotifyIconW(NIM_ADD, &nid);
}

pub unsafe fn handle_tray_callback(host: HWND, lp: LPARAM) {
    let event = (lp & 0xffff) as u32;
    if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);

        let menu = CreatePopupMenu();
        let dash_open = dashboard::is_open();
        let overlay_open = overlay::is_open();
        AppendMenuW(menu,
            MF_STRING | if dash_open { MF_CHECKED } else { MF_UNCHECKED },
            ID_MENU_DASHBOARD as usize, w!("Dashboard"));
        AppendMenuW(menu,
            MF_STRING | if overlay_open { MF_CHECKED } else { MF_UNCHECKED },
            ID_MENU_OVERLAY as usize, w!("Overlay"));
        AppendMenuW(menu, MF_STRING, ID_MENU_REFRESH as usize, w!("Refresh Now"));
        AppendMenuW(menu, MF_STRING, ID_MENU_SETTINGS as usize, w!("Settings"));
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, ID_MENU_QUIT as usize, w!("Quit"));

        // Required so TrackPopupMenu can dismiss on outside click
        SetForegroundWindow(host);
        let cmd = TrackPopupMenu(
            menu, TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_BOTTOMALIGN,
            pt.x, pt.y, 0, host, std::ptr::null(),
        );
        DestroyMenu(menu);

        match cmd as u16 {
            ID_MENU_DASHBOARD => dashboard::open(host),
            ID_MENU_OVERLAY   => overlay::toggle(host),
            ID_MENU_REFRESH   => { /* would re-fetch usage; no-op in prototype */ }
            ID_MENU_SETTINGS  => settings::open(host),
            ID_MENU_QUIT      => { remove(); PostQuitMessage(0); }
            _ => {}
        }
    }
}

pub unsafe fn remove() {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = TRAY_HWND;
    nid.uID = 1;
    Shell_NotifyIconW(NIM_DELETE, &nid);
}

// Render the bar-chart icon at 64×64, then convert to HICON.
unsafe fn build_tray_icon(pct: f64) -> HICON {
    let hdc_screen = GetDC(null_mut());
    let hdc_mem = CreateCompatibleDC(hdc_screen);
    let bmp = CreateCompatibleBitmap(hdc_screen, 64, 64);
    let old_bmp = SelectObject(hdc_mem, bmp as HGDIOBJ);

    // Background
    let brush_bg = CreateSolidBrush(0x002e_1e1e);
    let rc = RECT { left: 0, top: 0, right: 64, bottom: 64 };
    FillRect(hdc_mem, &rc, brush_bg);
    DeleteObject(brush_bg as HGDIOBJ);

    // Inner box
    let brush_inner = CreateSolidBrush(0x0046_3232);
    let rc_inner = RECT { left: 4, top: 8, right: 60, bottom: 56 };
    FillRect(hdc_mem, &rc_inner, brush_inner);
    DeleteObject(brush_inner as HGDIOBJ);

    // Bar
    let bar_h = (48.0 * pct.min(100.0) / 100.0) as i32;
    if bar_h > 0 {
        let brush_bar = CreateSolidBrush(pct_color(pct));
        let rc_bar = RECT { left: 4, top: 56 - bar_h, right: 60, bottom: 56 };
        FillRect(hdc_mem, &rc_bar, brush_bar);
        DeleteObject(brush_bar as HGDIOBJ);
    }

    // "X" text — Arial Bold 18px, drawn at y=56 (matches Python original)
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

    // Convert bitmap to icon
    let mut mask_bits: [u8; 64 * 64 / 8] = [0; 64 * 64 / 8];
    let mask = CreateBitmap(64, 64, 1, 1, mask_bits.as_mut_ptr() as _);
    let ii = ICONINFO {
        fIcon: 1,
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: bmp,
    };
    let icon = CreateIconIndirect(&ii);
    DeleteObject(bmp as HGDIOBJ);
    DeleteObject(mask as HGDIOBJ);
    icon
}
