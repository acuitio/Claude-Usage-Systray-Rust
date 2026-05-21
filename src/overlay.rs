// Layered-window overlay. Per-pixel-alpha bitmap pushed via UpdateLayeredWindow.
// Ports both src/App/OverlayWindow.cs (window lifecycle, drag) and
// OverlayRenderer.cs (token parsing, layout, colors, opacity, font).
//
// The overlay reads every visual property from config.json on each render:
//   - overlay_format            — placeholders + literal text + "|" dividers
//   - font_family + scale_pct   — text size
//   - bg_color + overlay_opacity— translucent background
//   - color_text                — plain text & dividers
//   - color_sufficient/partial/depleted — percentage tokens, by threshold
//   - widget_x / widget_y       — restored position
//
// Known limitation: raw GDI DrawText into a DIB writes RGB but leaves the
// alpha channel at 0. On a layered window that means text pixels render as
// "transparent over the background color we filled," which looks slightly
// muddier than the C# version's GDI+ rendering. The trade for staying off
// GDI+ / Direct2D is a much smaller binary. Acceptable for a tray widget.

use std::ffi::c_void;
use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;
use crate::models::AppConfig;
use crate::settings::hex_to_colorref;

static mut HWND_OVERLAY: HWND = null_mut();
static mut DRAGGING: bool = false;
static mut DRAG_START: POINT = POINT { x: 0, y: 0 };

const MK_LBUTTON: u32 = 0x0001;

pub unsafe fn is_open() -> bool {
    !HWND_OVERLAY.is_null() && IsWindow(HWND_OVERLAY) != 0
}

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
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0, cbWndExtra: 0,
        hInstance: instance,
        hIcon: null_mut(),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name,
        hIconSm: null_mut(),
    };
    RegisterClassExW(&wc);

    let cfg = crate::config_store::load();
    // Initial size is a placeholder; render() resizes to fit the actual
    // measured token content on its first call.
    let (x, y) = if let (Some(cx), Some(cy)) = (cfg.widget_x, cfg.widget_y) {
        (cx, cy)
    } else {
        let mut wa: RECT = std::mem::zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0);
        let cx = wa.left + ((wa.right - wa.left) - 240) / 2;
        let cy = wa.bottom - 44 - 40;
        (cx, cy)
    };

    HWND_OVERLAY = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
        class_name, std::ptr::null(),
        WS_POPUP,
        x, y, 240, 44,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    ShowWindow(HWND_OVERLAY, SW_SHOWNOACTIVATE);
    render();
}

// ─── Token model ──────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum TokenKind { Text, Div }

struct OverlayToken {
    kind: TokenKind,
    text: String,
    font: HFONT,   // not owned by token; freed at end of render()
    color: u32,    // COLORREF
    width: i32,    // filled in during measure pass
}

unsafe fn build_tokens(
    fmt: &str,
    cfg: &AppConfig,
    usage: &UsageData,
    font_main: HFONT,
    font_sep: HFONT,
    text_color: u32,
) -> Vec<OverlayToken> {
    let mut tokens = Vec::new();
    let bytes = fmt.as_bytes();
    let mut i = 0;
    let mut text_start = 0;

    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(rel_close) = fmt[i + 1..].find('}') {
                if i > text_start {
                    emit_plain(&fmt[text_start..i], &mut tokens, font_sep, text_color);
                }
                let key = &fmt[i + 1..i + 1 + rel_close];
                let (display, pct_color) = placeholder_value(key, usage, cfg);
                let has_color = pct_color.is_some();
                tokens.push(OverlayToken {
                    kind: TokenKind::Text,
                    text: if has_color { display.to_uppercase() } else { display },
                    font: if has_color { font_main } else { font_sep },
                    color: pct_color.unwrap_or(text_color),
                    width: 0,
                });
                i = i + 1 + rel_close + 1;
                text_start = i;
                continue;
            }
        }
        i += 1;
    }
    if text_start < fmt.len() {
        emit_plain(&fmt[text_start..], &mut tokens, font_sep, text_color);
    }
    tokens
}

unsafe fn emit_plain(plain: &str, out: &mut Vec<OverlayToken>, font: HFONT, color: u32) {
    let parts: Vec<&str> = plain.split('|').collect();
    let count = parts.len();
    for (i, part) in parts.iter().enumerate() {
        if !part.is_empty() {
            out.push(OverlayToken {
                kind: TokenKind::Text,
                text: part.to_uppercase(),
                font, color,
                width: 0,
            });
        }
        if i < count - 1 {
            out.push(OverlayToken {
                kind: TokenKind::Div,
                text: String::new(),
                font, color,
                width: 0,
            });
        }
    }
}

fn placeholder_value(key: &str, usage: &UsageData, cfg: &AppConfig) -> (String, Option<u32>) {
    match key {
        "session" => (format!("{:.0}%", usage.session_pct), Some(pct_color_cfg(usage.session_pct, cfg))),
        "weekly"  => (format!("{:.0}%", usage.weekly_pct),  Some(pct_color_cfg(usage.weekly_pct,  cfg))),
        "sonnet"  => (format!("{:.0}%", usage.sonnet_pct),  Some(pct_color_cfg(usage.sonnet_pct,  cfg))),
        "s_reset" => (format_reset(usage.session_reset_iso.as_deref()), None),
        "w_reset" => (format_reset(usage.weekly_reset_iso.as_deref()),  None),
        _ => ("?".into(), None),
    }
}

fn pct_color_cfg(pct: f64, cfg: &AppConfig) -> u32 {
    let s = hex_to_colorref(&cfg.color_sufficient).unwrap_or(GREEN);
    let p = hex_to_colorref(&cfg.color_partial).unwrap_or(YELLOW);
    let d = hex_to_colorref(&cfg.color_depleted).unwrap_or(RED);
    if pct < 50.0 { s } else if pct < 90.0 { p } else { d }
}

// ─── Render ───────────────────────────────────────────────────────────

unsafe fn render() {
    if !is_open() { return; }
    let cfg = crate::config_store::load();
    let usage = current_snapshot();

    // Fonts scaled per config.
    let scale = (cfg.scale_pct.max(1) as f64) / 100.0;
    let main_size = ((11.0 * scale * 4.0 / 3.0) as i32).max(9);
    let sep_size  = ((10.0 * scale * 4.0 / 3.0) as i32).max(8);
    let font_main = make_overlay_font(&cfg.font_family, main_size, FW_BOLD as i32);
    let font_sep  = make_overlay_font(&cfg.font_family, sep_size,  FW_NORMAL as i32);

    let text_color = hex_to_colorref(&cfg.color_text).unwrap_or(0x00ff_ffff);
    let mut tokens = build_tokens(&cfg.overlay_format, &cfg, &usage, font_main, font_sep, text_color);

    let div_w = (scale as i32).max(1);
    let div_gap = ((4.0 * scale) as i32).max(3);

    // Measure pass — using a temp DC + bitmap so GetTextExtentPoint32W has
    // the font selected.
    let hdc_screen = GetDC(null_mut());
    let hdc_mem = CreateCompatibleDC(hdc_screen);
    let tmp_bmp = CreateCompatibleBitmap(hdc_screen, 1, 1);
    let old_bmp_measure = SelectObject(hdc_mem, tmp_bmp as HGDIOBJ);

    let mut content_w = 0;
    let mut content_h = 0;
    for tok in &mut tokens {
        if tok.kind == TokenKind::Div {
            tok.width = div_w + div_gap * 2;
        } else {
            SelectObject(hdc_mem, tok.font as HGDIOBJ);
            let w16 = wstr(&tok.text);
            let mut sz: SIZE = std::mem::zeroed();
            // w16.len() includes trailing null — subtract 1.
            GetTextExtentPoint32W(hdc_mem, w16.as_ptr(), (w16.len() - 1) as i32, &mut sz);
            tok.width = sz.cx;
            content_h = content_h.max(sz.cy);
        }
        content_w += tok.width;
    }
    content_h = content_h.max((12.0 * scale) as i32);

    SelectObject(hdc_mem, old_bmp_measure);
    DeleteObject(tmp_bmp as HGDIOBJ);

    let pad_x: i32 = 10;
    let pad_y: i32 = 5;
    let width  = (content_w + pad_x * 2 + 2).max(10);
    let height = (content_h + pad_y * 2 + 2).max(10);

    // Resize the overlay window to fit measured content.
    let mut rc: RECT = std::mem::zeroed();
    GetWindowRect(HWND_OVERLAY, &mut rc);
    if (rc.right - rc.left) != width || (rc.bottom - rc.top) != height {
        SetWindowPos(HWND_OVERLAY, null_mut(),
            0, 0, width, height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        GetWindowRect(HWND_OVERLAY, &mut rc);
    }

    // ARGB DIB section. biHeight negative → top-down so y=0 is the top row.
    let mut bi: BITMAPINFO = std::mem::zeroed();
    bi.bmiHeader.biSize        = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth       = width;
    bi.bmiHeader.biHeight      = -height;
    bi.bmiHeader.biPlanes      = 1;
    bi.bmiHeader.biBitCount    = 32;
    bi.bmiHeader.biCompression = BI_RGB;
    let mut bits: *mut c_void = null_mut();
    let bmp = CreateDIBSection(hdc_mem, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    let old_bmp = SelectObject(hdc_mem, bmp as HGDIOBJ);

    // Background + border are filled NON-premultiplied so GDI text (which
    // writes raw RGB without touching alpha) composes naturally on top.
    // A single post-pass premultiplies everything at the end. Without this,
    // AA edges between text and background end up with mismatched RGB↔A
    // pairs and visibly alias.
    let opacity = cfg.overlay_opacity.clamp(0, 100) as u32;
    let bg_a = (opacity * 255 / 100).max(1);
    let bg_col = hex_to_colorref(&cfg.bg_color).unwrap_or(0x002e_1e1e);
    let bg_pixel = nopremul_bgra(bg_col, bg_a);
    fill_bgra(bits, (width * height) as usize, bg_pixel);

    let border_a = (opacity * 255 / 100 * 3).min(255);
    if border_a > 0 {
        let border_pixel = nopremul_bgra(0x006c_4a4a, border_a);
        draw_border(bits, width, height, border_pixel);
    }

    // Token pass — text & dividers.
    SetBkMode(hdc_mem, TRANSPARENT as i32);
    let div_h = content_h * 60 / 100;
    let div_top = pad_y + 1 + (content_h - div_h) / 2;
    let div_bot = div_top + div_h - 1;
    let mut x = pad_x + 1;
    let y_base = pad_y + 1;

    for tok in &tokens {
        if tok.kind == TokenKind::Div {
            let pen = CreatePen(PS_SOLID as i32, div_w, text_color);
            let old_pen = SelectObject(hdc_mem, pen as HGDIOBJ);
            let lx = x + div_gap;
            MoveToEx(hdc_mem, lx, div_top, null_mut());
            LineTo(hdc_mem, lx, div_bot);
            SelectObject(hdc_mem, old_pen);
            DeleteObject(pen as HGDIOBJ);
        } else {
            SelectObject(hdc_mem, tok.font as HGDIOBJ);
            SetTextColor(hdc_mem, tok.color);
            let w16 = wstr(&tok.text);
            TextOutW(hdc_mem, x, y_base, w16.as_ptr(), (w16.len() - 1) as i32);
        }
        x += tok.width;
    }

    // Premultiply every pixel in one pass. Background, border, and text all
    // get their alpha baked into the RGB channels as ULW_ALPHA requires.
    // GDI didn't touch the A channel during text drawing, so text pixels
    // already have the surrounding bg_a — we just need to scale RGB by it.
    premultiply_all(bits, (width * height) as usize);

    // Push to the layered window.
    let blend = BLENDFUNCTION {
        BlendOp:             AC_SRC_OVER as u8,
        BlendFlags:          0,
        SourceConstantAlpha: 255,
        AlphaFormat:         AC_SRC_ALPHA as u8,
    };
    let mut sz = SIZE { cx: width, cy: height };
    let mut pt_src = POINT { x: 0, y: 0 };
    let mut pt_dst = POINT { x: rc.left, y: rc.top };
    UpdateLayeredWindow(HWND_OVERLAY, hdc_screen,
        &mut pt_dst, &mut sz, hdc_mem, &mut pt_src,
        0, &blend, ULW_ALPHA);

    // Clean up.
    SelectObject(hdc_mem, old_bmp);
    DeleteObject(bmp as HGDIOBJ);
    DeleteObject(font_main as HGDIOBJ);
    DeleteObject(font_sep as HGDIOBJ);
    DeleteDC(hdc_mem);
    ReleaseDC(null_mut(), hdc_screen);
}

unsafe fn make_overlay_font(family: &str, em_size_px: i32, weight: i32) -> HFONT {
    let face = wstr(family);
    // Negative nHeight = em-size in pixels (positive would be cell height).
    // ANTIALIASED_QUALITY (grayscale AA) instead of CLEARTYPE_QUALITY because
    // ClearType's subpixel RGB hinting produces colored fringes once we
    // alpha-blend onto a translucent background.
    CreateFontW(
        -em_size_px, 0, 0, 0, weight,
        0, 0, 0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        ANTIALIASED_QUALITY as u32,
        (DEFAULT_PITCH | FF_DONTCARE) as u32,
        face.as_ptr(),
    )
}

/// Non-premultiplied BGRA pixel. The DIB stores bytes in order B,G,R,A;
/// our u32 layout is `(A << 24) | (B << 16) | (G << 8) | R` to match.
fn nopremul_bgra(colorref: u32, alpha: u32) -> u32 {
    let r = colorref & 0xff;
    let g = (colorref >> 8) & 0xff;
    let b = (colorref >> 16) & 0xff;
    (alpha << 24) | (b << 16) | (g << 8) | r
}

fn premul(component: u32, alpha: u32) -> u32 { component * alpha / 255 }

unsafe fn premultiply_all(bits: *mut c_void, count: usize) {
    let p = bits as *mut u32;
    for i in 0..count {
        let px = *p.add(i);
        let a = (px >> 24) & 0xff;
        if a == 0 || a == 0xff { continue; }
        let b = (px >> 16) & 0xff;
        let g = (px >> 8) & 0xff;
        let r = px & 0xff;
        *p.add(i) = (a << 24)
            | (premul(b, a) << 16)
            | (premul(g, a) << 8)
            | premul(r, a);
    }
}

unsafe fn fill_bgra(bits: *mut c_void, count: usize, pixel: u32) {
    let p = bits as *mut u32;
    for i in 0..count { *p.add(i) = pixel; }
}

unsafe fn draw_border(bits: *mut c_void, w: i32, h: i32, pixel: u32) {
    let p = bits as *mut u32;
    let (wu, hu) = (w as usize, h as usize);
    for x in 0..wu {
        *p.add(x) = pixel;                       // top row
        *p.add((hu - 1) * wu + x) = pixel;       // bottom row
    }
    for y in 0..hu {
        *p.add(y * wu) = pixel;                  // left column
        *p.add(y * wu + (wu - 1)) = pixel;       // right column
    }
}


// ─── Window proc ─────────────────────────────────────────────────────

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
