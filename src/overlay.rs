// Layered-window overlay. Per-pixel-alpha bitmap pushed via UpdateLayeredWindow.
// Matches src/App/OverlayWindow.cs + OverlayRenderer.cs behavior.

use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;

static mut HWND_OVERLAY: HWND = null_mut();
static mut DRAGGING: bool = false;
static mut DRAG_START: POINT = POINT { x: 0, y: 0 };

pub unsafe fn is_open() -> bool {
    !HWND_OVERLAY.is_null() && IsWindow(HWND_OVERLAY) != 0
}

/// Force a re-render against the current cache. Called from the host
/// window's WM_USAGE_UPDATED handler.
pub unsafe fn on_data_changed() {
    if is_open() { render(); }
}

pub unsafe fn toggle(_owner: HWND) {
    if is_open() {
        DestroyWindow(HWND_OVERLAY);
        HWND_OVERLAY = null_mut();
    } else {
        show();
    }
}

unsafe fn show() {
    let class_name = w!("Win32OverlayProto");
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
        hbrBackground: null_mut(),
        lpszMenuName:  std::ptr::null(),
        lpszClassName: class_name,
        hIconSm:       null_mut(),
    };
    RegisterClassExW(&wc);

    // Restore last drag position from config if present, else default to
    // screen-bottom-centered above the taskbar.
    let cfg = crate::config_store::load();
    let w = 240;
    let h = 44;
    let (x, y) = if let (Some(cx), Some(cy)) = (cfg.widget_x, cfg.widget_y) {
        (cx, cy)
    } else {
        let mut wa: RECT = std::mem::zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0);
        let cx = ((wa.right - wa.left) - w) / 2 + wa.left;
        let cy = wa.bottom - h - 40;
        (cx, cy)
    };

    HWND_OVERLAY = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
        class_name, std::ptr::null(),
        WS_POPUP,
        x, y, w, h,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    ShowWindow(HWND_OVERLAY, SW_SHOWNOACTIVATE);
    render();
}

unsafe fn render() {
    if !is_open() { return; }
    let mut rc: RECT = std::mem::zeroed();
    GetWindowRect(HWND_OVERLAY, &mut rc);
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;

    let hdc_screen = GetDC(null_mut());
    let hdc_mem = CreateCompatibleDC(hdc_screen);

    // 32bpp ARGB bitmap for per-pixel alpha
    let mut bi: BITMAPINFO = std::mem::zeroed();
    bi.bmiHeader.biSize        = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth       = w;
    bi.bmiHeader.biHeight      = h;
    bi.bmiHeader.biPlanes      = 1;
    bi.bmiHeader.biBitCount    = 32;
    bi.bmiHeader.biCompression = BI_RGB;
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let bmp = CreateDIBSection(hdc_mem, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    let old_bmp = SelectObject(hdc_mem, bmp as HGDIOBJ);

    // Premultiplied translucent background — 85% opacity dark navy
    let bg_a: u32 = 217; // ~85%
    let bgcol: u32 = ((bg_a) << 24) | premul(0x2e, bg_a) << 16 | premul(0x1e, bg_a) << 8 | premul(0x1e, bg_a);
    fill_bgra(bits, (w * h) as usize, bgcol);

    // Draw the formatted percentages with GDI text on top
    let old_font = SelectObject(hdc_mem, FONT_BOLD as HGDIOBJ);
    SetBkMode(hdc_mem, TRANSPARENT as i32);
    let usage = current_snapshot();
    let txt = wstr(&format!("{:.0}%  |  {:.0}%  |  {:.0}%",
                            usage.session_pct, usage.weekly_pct, usage.sonnet_pct));
    SetTextColor(hdc_mem, FG_LIGHT);
    let rc_txt = RECT { left: 0, top: 0, right: w, bottom: h };
    // Drawing on a premultiplied surface with opaque text — GDI doesn't know
    // about premultiplied alpha so the text gets bg-on-text aliasing. Real
    // port uses GDI+ TextRenderingHint.AntiAlias (see OverlayRenderer.cs).
    DrawTextW(hdc_mem, txt.as_ptr(), -1, &rc_txt as *const _ as *mut _,
              DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    SelectObject(hdc_mem, old_font);

    let blend = BLENDFUNCTION {
        BlendOp:             AC_SRC_OVER as u8,
        BlendFlags:          0,
        SourceConstantAlpha: 255,
        AlphaFormat:         AC_SRC_ALPHA as u8,
    };
    let mut sz = SIZE { cx: w, cy: h };
    let mut pt_src = POINT { x: 0, y: 0 };
    let mut pt_dst = POINT { x: rc.left, y: rc.top };
    UpdateLayeredWindow(HWND_OVERLAY, hdc_screen,
        &mut pt_dst, &mut sz, hdc_mem, &mut pt_src,
        0, &blend, ULW_ALPHA);

    SelectObject(hdc_mem, old_bmp);
    DeleteObject(bmp as HGDIOBJ);
    DeleteDC(hdc_mem);
    ReleaseDC(null_mut(), hdc_screen);
}

unsafe fn fill_bgra(bits: *mut std::ffi::c_void, count: usize, pixel_bgra: u32) {
    let p = bits as *mut u32;
    for i in 0..count {
        *p.add(i) = pixel_bgra;
    }
}

fn premul(component: u32, alpha: u32) -> u32 { component * alpha / 255 }

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_LBUTTONDOWN => {
                let pt = lparam_to_point(lp);
                DRAGGING = false;
                DRAG_START = pt;
                SetCapture(hwnd);
                0
            }
            WM_MOUSEMOVE => {
                if (wp & MK_LBUTTON as usize) != 0 {
                    let pt = lparam_to_point(lp);
                    if (pt.x - DRAG_START.x).abs() > 3 || (pt.y - DRAG_START.y).abs() > 3 {
                        DRAGGING = true;
                    }
                    if DRAGGING {
                        let mut rc: RECT = std::mem::zeroed();
                        GetWindowRect(hwnd, &mut rc);
                        let dx = pt.x - DRAG_START.x;
                        let dy = pt.y - DRAG_START.y;
                        SetWindowPos(hwnd, null_mut(), rc.left + dx, rc.top + dy, 0, 0,
                                     SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                        render();
                    }
                }
                0
            }
            WM_LBUTTONUP => {
                ReleaseCapture();
                if DRAGGING {
                    // Persist new position so it sticks across launches.
                    let mut rc: RECT = std::mem::zeroed();
                    GetWindowRect(hwnd, &mut rc);
                    let mut cfg = crate::config_store::load();
                    cfg.widget_x = Some(rc.left);
                    cfg.widget_y = Some(rc.top);
                    let _ = crate::config_store::save(&cfg);
                }
                DRAGGING = false;
                0
            }
            WM_DESTROY => {
                HWND_OVERLAY = null_mut();
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn lparam_to_point(lp: LPARAM) -> POINT {
    POINT {
        x: (lp & 0xffff) as i16 as i32,
        y: ((lp >> 16) & 0xffff) as i16 as i32,
    }
}

// ─── Bits exposed from windows-sys we need locally typed ──────────────
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
const MK_LBUTTON: u32 = 0x0001;
