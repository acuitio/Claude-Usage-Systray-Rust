// Dashboard window with three usage bars. Matches src/App/DashboardForm.cs.

use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;

static mut HWND_DASH: HWND = null_mut();
const ID_BTN_REFRESH: u16 = 8001;

pub unsafe fn is_open() -> bool {
    !HWND_DASH.is_null() && IsWindow(HWND_DASH) != 0
}

/// Force a repaint against the current cache. Called from the host
/// window's WM_USAGE_UPDATED handler.
pub unsafe fn on_data_changed() {
    if is_open() {
        InvalidateRect(HWND_DASH, std::ptr::null(), 0);
    }
}

pub unsafe fn open(_owner: HWND) {
    if is_open() {
        SetForegroundWindow(HWND_DASH);
        return;
    }

    let class_name = w!("Win32DashboardProto");
    let instance = GetModuleHandleW(std::ptr::null());

    let wc = WNDCLASSEXW {
        cbSize:        std::mem::size_of::<WNDCLASSEXW>() as u32,
        style:         0,
        lpfnWndProc:   Some(wnd_proc),
        cbClsExtra:    0,
        cbWndExtra:    0,
        hInstance:     instance,
        hIcon:         null_mut(),
        hCursor:       LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: HBR_BG,
        lpszMenuName:  std::ptr::null(),
        lpszClassName: class_name,
        hIconSm:       null_mut(),
    };
    // Position center of primary working area (matches the fix we did to
    // DashboardForm.cs for high-DPI multi-monitor setups).
    let mut wa: RECT = std::mem::zeroed();
    SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0);
    let dw = 540;
    let dh = 480;
    let x = wa.left + ((wa.right - wa.left) - dw) / 2;
    let y = wa.top  + ((wa.bottom - wa.top) - dh) / 2;

    RegisterClassExW(&wc);

    // Create with WS_VISIBLE so the window is shown atomically with creation.
    // When called from inside a tray-menu message handler, a separate
    // ShowWindow+UpdateWindow+SetForegroundWindow sequence can race the
    // surrounding handler and end up with an invisible-but-existing window.
    HWND_DASH = CreateWindowExW(
        0, class_name, w!("Dashboard"),
        (WS_OVERLAPPEDWINDOW & !WS_MAXIMIZEBOX) | WS_VISIBLE,
        x, y, dw, dh,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    // Force Z-order to top + foreground without relying on focus-grant rules
    // that don't apply when triggered from a tray click.
    SetWindowPos(HWND_DASH, HWND_TOP, 0, 0, 0, 0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
    UpdateWindow(HWND_DASH);
    SetForegroundWindow(HWND_DASH);
}

unsafe fn build_controls(parent: HWND) {
    // "Refresh Now" button docked at bottom — controls above are custom-painted
    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(parent, &mut rc);

    let btn = create_child(parent, w!("BUTTON"), w!("Refresh Now"),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
        20, rc.bottom - 50, rc.right - 40, 36, ID_BTN_REFRESH);
    set_font(btn, FONT_REG);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);

    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);

    // Background
    FillRect(hdc, &rc, HBR_BG);
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, FG_LIGHT);
    SelectObject(hdc, FONT_REG as HGDIOBJ);

    let usage = current_snapshot();
    let pad = 28;
    let mut y = 24;

    // Header line: plan
    SelectObject(hdc, FONT_BOLD as HGDIOBJ);
    let header_txt = wstr(&usage.plan);
    let rc_hdr = RECT { left: pad, top: y, right: rc.right - pad, bottom: y + 22 };
    DrawTextW(hdc, header_txt.as_ptr(), -1, &rc_hdr as *const _ as *mut _,
              DT_LEFT | DT_TOP | DT_SINGLELINE);
    y += 26;

    // Subtitle: "Cache age: …" or "No usage data yet" if cache missing
    SelectObject(hdc, FONT_REG as HGDIOBJ);
    SetTextColor(hdc, 0x00b0_b0b0);
    let sub_text = match crate::usage_cache::load() {
        Some(c) => format!("Cache age: {}s", c.age_seconds as i64),
        None    => "No usage data yet — waiting for first fetch.".into(),
    };
    let sub = wstr(&sub_text);
    let rc_sub = RECT { left: pad, top: y, right: rc.right - pad, bottom: y + 20 };
    DrawTextW(hdc, sub.as_ptr(), -1, &rc_sub as *const _ as *mut _,
              DT_LEFT | DT_TOP | DT_SINGLELINE);
    y += 30;

    // Separator
    let sep_brush = CreateSolidBrush(BORDER);
    let rc_sep = RECT { left: pad, top: y, right: rc.right - pad, bottom: y + 1 };
    FillRect(hdc, &rc_sep, sep_brush);
    DeleteObject(sep_brush as HGDIOBJ);
    y += 14;

    // Three bars
    let session_reset = crate::common::format_reset(usage.session_reset_iso.as_deref());
    let weekly_reset  = crate::common::format_reset(usage.weekly_reset_iso.as_deref());
    for (label, pct, reset) in [
        ("Session (5-hour)",    usage.session_pct, session_reset.as_str()),
        ("Weekly (All Models)", usage.weekly_pct,  weekly_reset.as_str()),
        ("Weekly (Sonnet)",     usage.sonnet_pct,  weekly_reset.as_str()),
    ] {
        y = draw_bar(hdc, pad, y, rc.right - 2 * pad, label, pct, reset);
        y += 18;
    }

    EndPaint(hwnd, &ps);
}

unsafe fn draw_bar(hdc: HDC, x: i32, y: i32, w: i32, label: &str, pct: f64, reset: &str) -> i32 {
    let mut cur_y = y;
    SetTextColor(hdc, FG_LIGHT);

    // Label (left) + percentage (right)
    SelectObject(hdc, FONT_REG as HGDIOBJ);
    let lbl = wstr(label);
    let rc_lbl = RECT { left: x, top: cur_y, right: x + w, bottom: cur_y + 22 };
    DrawTextW(hdc, lbl.as_ptr(), -1, &rc_lbl as *const _ as *mut _,
              DT_LEFT | DT_TOP | DT_SINGLELINE);

    SelectObject(hdc, FONT_BOLD as HGDIOBJ);
    SetTextColor(hdc, pct_color(pct));
    let pctstr = wstr(&format!("{:.0}%", pct));
    let rc_pct = RECT { left: x, top: cur_y, right: x + w, bottom: cur_y + 22 };
    DrawTextW(hdc, pctstr.as_ptr(), -1, &rc_pct as *const _ as *mut _,
              DT_RIGHT | DT_TOP | DT_SINGLELINE);
    cur_y += 22;

    // Bar background + fill
    let bar_h = 6;
    let bar_bg = CreateSolidBrush(0x003d_3630);
    let rc_bar_bg = RECT { left: x, top: cur_y, right: x + w, bottom: cur_y + bar_h };
    FillRect(hdc, &rc_bar_bg, bar_bg);
    DeleteObject(bar_bg as HGDIOBJ);
    if pct > 0.0 {
        let fill_w = (w as f64 * pct.min(100.0) / 100.0) as i32;
        let bar_fill = CreateSolidBrush(pct_color(pct));
        let rc_fill = RECT { left: x, top: cur_y, right: x + fill_w, bottom: cur_y + bar_h };
        FillRect(hdc, &rc_fill, bar_fill);
        DeleteObject(bar_fill as HGDIOBJ);
    }
    cur_y += bar_h + 6;

    // Reset line
    SelectObject(hdc, FONT_REG as HGDIOBJ);
    SetTextColor(hdc, 0x00a0_a0a0);
    let txt = wstr(&format!("Resets in {}", reset));
    let rc_reset = RECT { left: x, top: cur_y, right: x + w, bottom: cur_y + 20 };
    DrawTextW(hdc, txt.as_ptr(), -1, &rc_reset as *const _ as *mut _,
              DT_LEFT | DT_TOP | DT_SINGLELINE);
    cur_y + 20
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => { build_controls(hwnd); 0 }
            WM_PAINT  => { paint(hwnd); 0 }
            WM_ERASEBKGND => 1, // skip — we handle background in WM_PAINT
            WM_COMMAND => {
                let id = (wp & 0xffff) as u16;
                if id == ID_BTN_REFRESH {
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
                0
            }
            WM_CTLCOLORBTN => {
                SetBkColor(wp as HDC, BG_DARK);
                SetTextColor(wp as HDC, FG_LIGHT);
                HBR_BG as LRESULT
            }
            WM_DPICHANGED => { handle_dpi_changed(hwnd, lp); 0 }
            WM_DESTROY => { HWND_DASH = null_mut(); 0 }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
