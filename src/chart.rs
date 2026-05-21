// Usage history line chart. Reads usage_history.json and plots all three
// series (session/weekly/sonnet) on one canvas using raw GDI.
//
// Inspired by src/App/ChartForm.cs but plots all three lines together so
// the user can compare at a glance, instead of one-per-window.

use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;
use crate::usage_history;

static mut HWND_CHART: HWND = null_mut();

const SESSION_COLOR: u32 = 0x0033_ff33; // green
const WEEKLY_COLOR:  u32 = 0x0032_c8e6; // yellow-orange
const SONNET_COLOR:  u32 = 0x00ff_8888; // soft red
const GRID_COLOR:    u32 = 0x004a_2a2a; // dark slate

pub unsafe fn is_open() -> bool {
    !HWND_CHART.is_null() && IsWindow(HWND_CHART) != 0
}

pub unsafe fn open(_owner: HWND) {
    if is_open() {
        SetForegroundWindow(HWND_CHART);
        return;
    }
    let class_name = w!("Win32ChartRustClass");
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

    // Place on primary working area, mid-screen.
    let mut wa: RECT = std::mem::zeroed();
    SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0);
    let cw = 720;
    let ch = 460;
    let cx = wa.left + ((wa.right - wa.left) - cw) / 2;
    let cy = wa.top  + ((wa.bottom - wa.top) - ch) / 2;

    HWND_CHART = CreateWindowExW(
        0, class_name, w!("Usage Chart"),
        WS_OVERLAPPEDWINDOW | WS_VISIBLE,
        cx, cy, cw, ch,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    SetWindowPos(HWND_CHART, HWND_TOP, 0, 0, 0, 0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
    UpdateWindow(HWND_CHART);
    SetForegroundWindow(HWND_CHART);
}

pub unsafe fn on_data_changed() {
    if is_open() { InvalidateRect(HWND_CHART, std::ptr::null(), 0); }
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);

    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    FillRect(hdc, &rc, HBR_BG);
    SetBkMode(hdc, TRANSPARENT as i32);

    let hist = usage_history::load();

    let pad_l = 48;
    let pad_r = 20;
    let pad_t = 36;
    let pad_b = 36;
    let gw = rc.right - rc.left - pad_l - pad_r;
    let gh = rc.bottom - rc.top - pad_t - pad_b;
    if gw <= 10 || gh <= 10 { EndPaint(hwnd, &ps); return; }

    // Title
    SelectObject(hdc, FONT_BOLD as HGDIOBJ);
    SetTextColor(hdc, FG_LIGHT);
    let title = wstr("Usage History");
    let rc_title = RECT { left: pad_l, top: 8, right: pad_l + gw, bottom: 28 };
    DrawTextW(hdc, title.as_ptr(), -1, &rc_title as *const _ as *mut _,
              DT_CENTER | DT_TOP | DT_SINGLELINE);

    if hist.len() < 2 {
        SelectObject(hdc, FONT_REG as HGDIOBJ);
        SetTextColor(hdc, 0x00a0_a0a0);
        let msg = wstr("Insufficient data — waiting for more samples.");
        let rc_msg = RECT { left: pad_l, top: pad_t, right: pad_l + gw, bottom: pad_t + gh };
        DrawTextW(hdc, msg.as_ptr(), -1, &rc_msg as *const _ as *mut _,
                  DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        EndPaint(hwnd, &ps);
        return;
    }

    let t0 = hist[0][0];
    let t1 = hist[hist.len() - 1][0];
    let t_span = (t1 - t0).max(1.0);

    // Gridlines + Y labels (25/50/75/100)
    let grid_pen = CreatePen(PS_SOLID as i32, 1, GRID_COLOR);
    let old_pen = SelectObject(hdc, grid_pen as HGDIOBJ);
    SelectObject(hdc, FONT_REG as HGDIOBJ);
    SetTextColor(hdc, 0x00b0_b0b0);
    for i in 0..=4 {
        let y = pad_t + gh * i / 4;
        let pct = 100 - 25 * i;
        MoveToEx(hdc, pad_l, y, std::ptr::null_mut());
        LineTo(hdc, pad_l + gw, y);
        let s = wstr(&format!("{pct}%"));
        let rc_lbl = RECT { left: 0, top: y - 9, right: pad_l - 4, bottom: y + 9 };
        DrawTextW(hdc, s.as_ptr(), -1, &rc_lbl as *const _ as *mut _,
                  DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
    }
    // X-axis baseline already at i=4

    // X-axis time labels — show 3 evenly-spaced labels (start / mid / end)
    for i in 0..=2 {
        let xpos = pad_l + gw * i / 2;
        let t = t0 + t_span * (i as f64) / 2.0;
        let label = format_clock_label(t as u64, t_span);
        let s = wstr(&label);
        let rc_lbl = RECT { left: xpos - 50, top: pad_t + gh + 4, right: xpos + 50, bottom: pad_t + gh + 24 };
        DrawTextW(hdc, s.as_ptr(), -1, &rc_lbl as *const _ as *mut _,
                  DT_CENTER | DT_TOP | DT_SINGLELINE);
    }

    SelectObject(hdc, old_pen);
    DeleteObject(grid_pen as HGDIOBJ);

    // Plot each series
    for (idx, color, name) in [
        (1, SESSION_COLOR, "Session"),
        (2, WEEKLY_COLOR,  "Weekly"),
        (3, SONNET_COLOR,  "Sonnet"),
    ] {
        let pen = CreatePen(PS_SOLID as i32, 2, color);
        let old = SelectObject(hdc, pen as HGDIOBJ);
        let mut first = true;
        for entry in &hist {
            let ts = entry[0];
            let val = entry[idx];
            let x = pad_l + ((ts - t0) / t_span * gw as f64) as i32;
            let y = pad_t + ((1.0 - val / 100.0) * gh as f64) as i32;
            if first {
                MoveToEx(hdc, x, y, std::ptr::null_mut());
                first = false;
            } else {
                LineTo(hdc, x, y);
            }
        }
        SelectObject(hdc, old);
        DeleteObject(pen as HGDIOBJ);

        // Legend swatch + label (top-right of chart area)
        let lx = pad_l + gw - 240 + (idx as i32 - 1) * 80;
        let ly = 12;
        let swatch_brush = CreateSolidBrush(color);
        let rc_swatch = RECT { left: lx, top: ly + 4, right: lx + 12, bottom: ly + 12 };
        FillRect(hdc, &rc_swatch, swatch_brush);
        DeleteObject(swatch_brush as HGDIOBJ);
        SetTextColor(hdc, color);
        let s = wstr(name);
        let rc_n = RECT { left: lx + 16, top: ly, right: lx + 76, bottom: ly + 18 };
        DrawTextW(hdc, s.as_ptr(), -1, &rc_n as *const _ as *mut _,
                  DT_LEFT | DT_TOP | DT_SINGLELINE);
    }

    EndPaint(hwnd, &ps);
}

/// "HH:MM" when the span is short, "MM/DD HH:MM" when longer than a day.
fn format_clock_label(epoch_secs: u64, span_secs: f64) -> String {
    let (_y, mo, d, h, mn, _s) = ymdhms_from_epoch(epoch_secs);
    if span_secs > 86_400.0 {
        format!("{mo:02}/{d:02} {h:02}:{mn:02}")
    } else {
        format!("{h:02}:{mn:02}")
    }
}

fn ymdhms_from_epoch(epoch: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = (epoch % 60) as u32;
    let m = ((epoch / 60) % 60) as u32;
    let h = ((epoch / 3600) % 24) as u32;
    let mut days = epoch / 86400;
    let mut year: u32 = 1970;
    loop {
        let din = if is_leap(year as i32) { 366 } else { 365 };
        if days < din { break; }
        days -= din;
        year += 1;
    }
    let mdays: [u64; 12] = if is_leap(year as i32) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month: u32 = 1;
    for md in mdays.iter() {
        if days < *md { break; }
        days -= md;
        month += 1;
    }
    (year, month, days as u32 + 1, h, m, s)
}

fn is_leap(y: i32) -> bool { (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 }

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT      => { paint(hwnd); 0 }
            WM_ERASEBKGND => 1,
            WM_SIZE       => { InvalidateRect(hwnd, std::ptr::null(), 0); 0 }
            WM_CLOSE      => { DestroyWindow(hwnd); 0 }
            WM_DESTROY    => { HWND_CHART = null_mut(); 0 }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
