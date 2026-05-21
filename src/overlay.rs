// Layered-window overlay. Per-pixel-alpha bitmap pushed via UpdateLayeredWindow.
// Text is rendered via GDI+ (TextRenderingHint::AntiAlias + SmoothingMode::AntiAlias),
// so AA fringes get proper per-pixel alpha — matches OverlayRenderer.cs in the
// .NET port.
//
// The overlay reads every visual property from config.json on each render:
//   - overlay_format            — placeholders + literal text + "|" dividers
//   - font_family + scale_pct   — text size
//   - bg_color + overlay_opacity— translucent background
//   - color_text                — plain text & dividers
//   - color_sufficient/partial/depleted — percentage tokens, by threshold
//   - widget_x / widget_y       — restored position

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Graphics::GdiPlus::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;
use crate::models::AppConfig;

// HWND of the overlay window. Set when `show()` succeeds, cleared on
// WM_DESTROY. AtomicPtr satisfies the borrow checker without forcing a
// Mutex on what is effectively single-threaded state.
static HWND_OVERLAY: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static DRAGGING: AtomicBool = AtomicBool::new(false);
thread_local! {
    static DRAG_START: Cell<POINT> = const { Cell::new(POINT { x: 0, y: 0 }) };
}

fn overlay_hwnd() -> HWND { HWND_OVERLAY.load(Ordering::Relaxed) }
fn set_overlay_hwnd(h: HWND) { HWND_OVERLAY.store(h, Ordering::Relaxed); }

const MK_LBUTTON: u32 = 0x0001;
/// Re-assert HWND_TOPMOST every 200ms so the overlay can't slip under the
/// taskbar (which is itself a topmost window).
const TIMER_TOPMOST: usize = 1;

// GDI+ pixel format for the DIB-backed bitmap. PARGB = premultiplied ARGB,
// which is what UpdateLayeredWindow expects — letting GDI+ do the multiplication
// for us means we don't need a post-render pass.
const PIXEL_FORMAT_32BPP_PARGB: i32 = 0x0e200b;

pub unsafe fn is_open() -> bool {
    let h = overlay_hwnd();
    !h.is_null() && IsWindow(h) != 0
}

pub unsafe fn on_data_changed() {
    if is_open() { render(); }
}

pub unsafe fn toggle(_owner: HWND) {
    let now_open = if is_open() {
        DestroyWindow(overlay_hwnd());
        set_overlay_hwnd(null_mut());
        false
    } else {
        show();
        true
    };
    let mut cfg = crate::config_store::load();
    let new_mode = if now_open { "overlay" } else { "tray" };
    if cfg.display_mode != new_mode {
        cfg.display_mode = new_mode.to_string();
        let _ = crate::config_store::save(&cfg);
    }
}

/// Open the overlay without flipping `display_mode` — for the startup
/// auto-restore path.
pub unsafe fn open_if_persisted() {
    let cfg = crate::config_store::load();
    if cfg.display_mode == "overlay" && !is_open() {
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
    let (x, y) = if let (Some(cx), Some(cy)) = (cfg.widget_x, cfg.widget_y) {
        clamp_to_virtual_screen(cx, cy, 240, 44)
    } else {
        let mut wa: RECT = std::mem::zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0);
        let cx = wa.left + ((wa.right - wa.left) - 240) / 2;
        let cy = wa.bottom - 44 - 40;
        (cx, cy)
    };

    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
        class_name, std::ptr::null(),
        WS_POPUP,
        x, y, 240, 44,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    set_overlay_hwnd(hwnd);
    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    SetTimer(hwnd, TIMER_TOPMOST, 200, None);
    render();
}

// ─── Token model ──────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum TokenKind { Text, Div }

#[derive(Clone, Copy, PartialEq)]
enum FontStyle { Main, Sep }

struct OverlayToken {
    kind: TokenKind,
    text: String,
    style: FontStyle,
    color: u32,    // COLORREF
    width: i32,    // filled in during measure pass
    height: i32,   // ditto
}

fn build_tokens(
    fmt: &str,
    cfg: &AppConfig,
    usage: &UsageData,
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
                    emit_plain(&fmt[text_start..i], &mut tokens, text_color);
                }
                let key = &fmt[i + 1..i + 1 + rel_close];
                let (display, pct_color) = placeholder_value(key, usage, cfg);
                let has_color = pct_color.is_some();
                tokens.push(OverlayToken {
                    kind:  TokenKind::Text,
                    text:  if has_color { display.to_uppercase() } else { display },
                    style: if has_color { FontStyle::Main } else { FontStyle::Sep },
                    color: pct_color.unwrap_or(text_color),
                    width: 0, height: 0,
                });
                i = i + 1 + rel_close + 1;
                text_start = i;
                continue;
            }
        }
        i += 1;
    }
    if text_start < fmt.len() {
        emit_plain(&fmt[text_start..], &mut tokens, text_color);
    }
    tokens
}

fn emit_plain(plain: &str, out: &mut Vec<OverlayToken>, color: u32) {
    let parts: Vec<&str> = plain.split('|').collect();
    let count = parts.len();
    for (i, part) in parts.iter().enumerate() {
        if !part.is_empty() {
            out.push(OverlayToken {
                kind:  TokenKind::Text,
                text:  part.to_uppercase(),
                style: FontStyle::Sep,
                color, width: 0, height: 0,
            });
        }
        if i < count - 1 {
            out.push(OverlayToken {
                kind:  TokenKind::Div,
                text:  String::new(),
                style: FontStyle::Sep,
                color, width: 0, height: 0,
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

    // Font sizes scaled per config — same formula as the .NET port: em-size
    // in pixels = base * scale * 4/3, with sensible floors.
    let scale = (cfg.scale_pct.max(1) as f32) / 100.0;
    let main_em = (11.0 * scale * 4.0 / 3.0).max(9.0);
    let sep_em  = (10.0 * scale * 4.0 / 3.0).max(8.0);

    let text_color = hex_to_colorref(&cfg.color_text).unwrap_or(0x00ff_ffff);
    let mut tokens = build_tokens(&cfg.overlay_format, &cfg, &usage, text_color);

    let div_w   = (scale as i32).max(1);
    let div_gap = ((4.0 * scale) as i32).max(3);

    // ── GDI+ font + format setup (used for both measure and draw) ───────
    let family = create_font_family(&cfg.font_family);
    let font_main = create_font(family, main_em, FontStyleBold);
    let font_sep  = create_font(family, sep_em,  FontStyleRegular);
    let sf        = create_string_format();

    // We need a Graphics for measuring even before the bitmap exists.
    // GDI+ requires a real device context here.
    let hdc_screen = GetDC(null_mut());
    let mut g_measure: *mut GpGraphics = null_mut();
    GdipCreateFromHDC(hdc_screen, &mut g_measure);
    GdipSetTextRenderingHint(g_measure, TextRenderingHintAntiAlias);

    // ── Measure pass ────────────────────────────────────────────────────
    let mut content_w = 0;
    let mut content_h = 0;
    for tok in &mut tokens {
        if tok.kind == TokenKind::Div {
            tok.width = div_w + div_gap * 2;
        } else {
            let font = if tok.style == FontStyle::Main { font_main } else { font_sep };
            let (tw, th) = measure_string(g_measure, &tok.text, font, sf);
            tok.width  = tw.ceil() as i32;
            tok.height = th.ceil() as i32;
            content_h = content_h.max(tok.height);
        }
        content_w += tok.width;
    }
    content_h = content_h.max((12.0 * scale) as i32);

    // Two-pass drop shadow: a tight CORE defines the letter shape, a wide
    // GLOW diffuses outward so the text integrates with the backdrop rather
    // than looking pasted on. Constants mirror Python's _render_overlay_image.
    let shadow_dy        = (scale * 1.0).max(1.0);
    let shadow_core_blur = (scale * 1.5).max(1.5);
    let shadow_glow_blur = (scale * 5.0).max(4.0);
    // Grow the bitmap so the widest blurred shadow can spread below the text
    // without being clipped.
    content_h += shadow_dy as i32 + shadow_glow_blur as i32 + 1;

    GdipDeleteGraphics(g_measure);

    let pad_x: i32 = 10;
    let pad_y: i32 = 5;
    let width  = (content_w + pad_x * 2 + 2).max(10);
    let height = (content_h + pad_y * 2 + 2).max(10);

    // Resize the overlay window to fit measured content.
    let hwnd = overlay_hwnd();
    let mut rc: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut rc);
    if (rc.right - rc.left) != width || (rc.bottom - rc.top) != height {
        SetWindowPos(hwnd, null_mut(),
            0, 0, width, height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        GetWindowRect(hwnd, &mut rc);
    }

    // ── DIB section + GDI+ bitmap ───────────────────────────────────────
    // Top-down BI_RGB 32bpp; we'll let GDI+ interpret the memory as
    // PixelFormat32bppPARGB so it writes premultiplied alpha directly —
    // ready for UpdateLayeredWindow with ULW_ALPHA.
    let hdc_mem = CreateCompatibleDC(hdc_screen);
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

    let mut gp_bitmap: *mut GpBitmap = null_mut();
    GdipCreateBitmapFromScan0(
        width, height,
        width * 4,  // stride in bytes
        PIXEL_FORMAT_32BPP_PARGB,
        bits as *mut u8,
        &mut gp_bitmap,
    );

    let mut g: *mut GpGraphics = null_mut();
    GdipGetImageGraphicsContext(gp_bitmap as *mut GpImage, &mut g);
    GdipSetTextRenderingHint(g, TextRenderingHintAntiAlias);
    GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
    GdipSetCompositingMode(g, CompositingModeSourceOver);

    // Start with a fully transparent surface (alpha=0 everywhere).
    GdipGraphicsClear(g, 0);

    // ── Background fill ────────────────────────────────────────────────
    let opacity = cfg.overlay_opacity.clamp(0, 100) as u32;
    let bg_a = (opacity * 255 / 100).max(1);
    let bg_col = hex_to_colorref(&cfg.bg_color).unwrap_or(0x002e_1e1e);
    let bg_argb = colorref_to_argb(bg_col, bg_a as u8);

    let mut bg_brush: *mut GpSolidFill = null_mut();
    GdipCreateSolidFill(bg_argb, &mut bg_brush);
    GdipFillRectangleI(g, bg_brush as *mut GpBrush, 0, 0, width, height);
    GdipDeleteBrush(bg_brush as *mut GpBrush);

    // ── Two-pass drop shadow (glow first, then tight core on top) ──────
    // Dividers are intentionally not shadowed — they're thin lines that
    // would just blur into smudges.
    const SHADOW_DX:           f32 = 0.0;
    const SHADOW_GLOW_FACTOR:  f32 = 0.35;
    const SHADOW_CORE_FACTOR:  f32 = 0.70;
    let y_base = pad_y + 1;
    apply_shadow_layer(
        hdc_screen, width, height, &tokens, pad_x, y_base,
        SHADOW_DX, shadow_dy, font_main, font_sep, sf,
        shadow_glow_blur, SHADOW_GLOW_FACTOR, g,
    );
    apply_shadow_layer(
        hdc_screen, width, height, &tokens, pad_x, y_base,
        SHADOW_DX, shadow_dy, font_main, font_sep, sf,
        shadow_core_blur, SHADOW_CORE_FACTOR, g,
    );

    // ── Tokens — text and dividers ──────────────────────────────────────
    let div_h = content_h * 60 / 100;
    let div_top = pad_y + 1 + (content_h - div_h) / 2;
    let div_bot = div_top + div_h - 1;
    let mut x = pad_x + 1;

    for tok in &tokens {
        if tok.kind == TokenKind::Div {
            let div_argb = colorref_to_argb(text_color, 0xB4); // ~70% alpha
            let mut pen: *mut GpPen = null_mut();
            GdipCreatePen1(div_argb, div_w as f32, UnitPixel, &mut pen);
            let lx = (x + div_gap) as f32 + 0.5; // half-pixel for crisp 1px line
            GdipDrawLine(g, pen, lx, div_top as f32, lx, div_bot as f32);
            GdipDeletePen(pen);
        } else {
            let font = if tok.style == FontStyle::Main { font_main } else { font_sep };
            let argb = colorref_to_argb(tok.color, 0xFF);
            let mut brush: *mut GpSolidFill = null_mut();
            GdipCreateSolidFill(argb, &mut brush);
            let text_wide = wstr(&tok.text);
            let layout = RectF {
                X: x as f32, Y: y_base as f32,
                Width: tok.width as f32 + 4.0,
                Height: tok.height as f32 + 4.0,
            };
            GdipDrawString(
                g, text_wide.as_ptr(), (text_wide.len() - 1) as i32,
                font, &layout, sf, brush as *mut GpBrush,
            );
            GdipDeleteBrush(brush as *mut GpBrush);
        }
        x += tok.width;
    }

    // ── Push to the layered window ──────────────────────────────────────
    let blend = BLENDFUNCTION {
        BlendOp:             AC_SRC_OVER as u8,
        BlendFlags:          0,
        SourceConstantAlpha: 255,
        AlphaFormat:         AC_SRC_ALPHA as u8,
    };
    let sz = SIZE { cx: width, cy: height };
    let pt_src = POINT { x: 0, y: 0 };
    let pt_dst = POINT { x: rc.left, y: rc.top };
    UpdateLayeredWindow(hwnd, hdc_screen,
        &pt_dst, &sz, hdc_mem, &pt_src,
        0, &blend, ULW_ALPHA);

    // ── Cleanup ─────────────────────────────────────────────────────────
    GdipDeleteGraphics(g);
    GdipDisposeImage(gp_bitmap as *mut GpImage);
    GdipDeleteFont(font_main);
    GdipDeleteFont(font_sep);
    GdipDeleteFontFamily(family);
    GdipDeleteStringFormat(sf);

    SelectObject(hdc_mem, old_bmp);
    DeleteObject(bmp as HGDIOBJ);
    DeleteDC(hdc_mem);
    ReleaseDC(null_mut(), hdc_screen);
}

// ─── Shadow layer ─────────────────────────────────────────────────────

/// One pass of the two-pass drop shadow. Renders black text into a private
/// PARGB scratch bitmap, runs the GDI+ Blur effect on it at `blur_radius`,
/// scales the alpha channel by `alpha_factor`, then composites the result
/// onto `main_g`. Call twice per render — wide+faint for the glow,
/// tight+stronger for the core — to match Python's _render_overlay_image.
#[allow(clippy::too_many_arguments)]
unsafe fn apply_shadow_layer(
    hdc_screen: HDC,
    width: i32, height: i32,
    tokens: &[OverlayToken],
    pad_x: i32, y_base: i32,
    shadow_dx: f32, shadow_dy: f32,
    font_main: *mut GpFont, font_sep: *mut GpFont, sf: *mut GpStringFormat,
    blur_radius: f32, alpha_factor: f32,
    main_g: *mut GpGraphics,
) {
    // ── Scratch DIB-backed PARGB bitmap, same dimensions as the overlay ─
    let hdc_mem = CreateCompatibleDC(hdc_screen);
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

    let mut gp_bmp: *mut GpBitmap = null_mut();
    GdipCreateBitmapFromScan0(
        width, height, width * 4,
        PIXEL_FORMAT_32BPP_PARGB,
        bits as *mut u8, &mut gp_bmp,
    );

    let mut g: *mut GpGraphics = null_mut();
    GdipGetImageGraphicsContext(gp_bmp as *mut GpImage, &mut g);
    GdipSetTextRenderingHint(g, TextRenderingHintAntiAlias);
    GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
    GdipGraphicsClear(g, 0);

    // ── Draw text tokens in opaque black (R=G=B=0, A=255) ──────────────
    let mut brush: *mut GpSolidFill = null_mut();
    GdipCreateSolidFill(0xFF00_0000, &mut brush);
    let mut x = pad_x + 1;
    for tok in tokens {
        if tok.kind == TokenKind::Div { x += tok.width; continue; }
        let font = if tok.style == FontStyle::Main { font_main } else { font_sep };
        let text_wide = wstr(&tok.text);
        let layout = RectF {
            X: x as f32 + shadow_dx,
            Y: y_base as f32 + shadow_dy,
            Width:  tok.width as f32 + 4.0,
            Height: tok.height as f32 + 4.0,
        };
        GdipDrawString(
            g, text_wide.as_ptr(), (text_wide.len() - 1) as i32,
            font, &layout, sf, brush as *mut GpBrush,
        );
        x += tok.width;
    }
    GdipDeleteBrush(brush as *mut GpBrush);
    GdipDeleteGraphics(g);

    // ── Apply Gaussian blur in place via the GDI+ Effects API ──────────
    // expandEdge=false: keep output the same size as input. We pre-sized
    // the overlay bitmap with extra room below the text to fit the glow.
    let mut effect: *mut CGpEffect = null_mut();
    if GdipCreateEffect(BlurEffectGuid, &mut effect) == 0 && !effect.is_null() {
        let params = BlurParams { radius: blur_radius, expandEdge: 0 };
        GdipSetEffectParameters(
            effect,
            &params as *const _ as *const c_void,
            std::mem::size_of::<BlurParams>() as u32,
        );
        GdipBitmapApplyEffect(gp_bmp, effect, null_mut(), 0, null_mut(), null_mut());
        GdipDeleteEffect(effect);
    }

    // ── Scale alpha in place ───────────────────────────────────────────
    // The source is pure black (R=G=B=0) and stays that way after blur,
    // so PARGB premul is trivially preserved when we only touch A.
    let pixel_count = (width as usize) * (height as usize);
    let buf = std::slice::from_raw_parts_mut(bits as *mut u8, pixel_count * 4);
    for i in 0..pixel_count {
        let a = buf[i * 4 + 3] as f32 * alpha_factor;
        buf[i * 4 + 3] = a.clamp(0.0, 255.0) as u8;
    }

    // ── Composite onto the main overlay bitmap ─────────────────────────
    GdipDrawImageI(main_g, gp_bmp as *mut GpImage, 0, 0);

    // ── Cleanup ────────────────────────────────────────────────────────
    GdipDisposeImage(gp_bmp as *mut GpImage);
    SelectObject(hdc_mem, old_bmp);
    DeleteObject(bmp as HGDIOBJ);
    DeleteDC(hdc_mem);
}

// ─── GDI+ helpers ─────────────────────────────────────────────────────

/// Look up a font family by name, falling back to Segoe UI if the user's
/// configured font isn't installed.
unsafe fn create_font_family(name: &str) -> *mut GpFontFamily {
    let wname = wstr(name);
    let mut family: *mut GpFontFamily = null_mut();
    if GdipCreateFontFamilyFromName(wname.as_ptr(), null_mut(), &mut family) == 0 {
        return family;
    }
    let fallback = wstr("Segoe UI");
    GdipCreateFontFamilyFromName(fallback.as_ptr(), null_mut(), &mut family);
    family
}

unsafe fn create_font(family: *mut GpFontFamily, em_px: f32, style: i32) -> *mut GpFont {
    let mut font: *mut GpFont = null_mut();
    GdipCreateFont(family, em_px, style, UnitPixel, &mut font);
    font
}

/// GenericTypographic + MeasureTrailingSpaces — same as the .NET port. The
/// typographic preset gives tighter measurements (no extra leading); the
/// trailing-spaces flag keeps inter-token gaps from collapsing during measure.
unsafe fn create_string_format() -> *mut GpStringFormat {
    // 0x800 = StringFormatFlagsMeasureTrailingSpaces.
    // 0x1000 = StringFormatFlagsNoWrap — single line, no width-based wrapping.
    // 0x4000 = StringFormatFlagsNoClip — don't clip to layout rect.
    let mut sf: *mut GpStringFormat = null_mut();
    let mut generic: *mut GpStringFormat = null_mut();
    GdipStringFormatGetGenericTypographic(&mut generic);
    GdipCloneStringFormat(generic, &mut sf);
    let mut flags: i32 = 0;
    GdipGetStringFormatFlags(sf, &mut flags);
    GdipSetStringFormatFlags(sf, flags | 0x800 | 0x1000 | 0x4000);
    sf
}

unsafe fn measure_string(
    g: *mut GpGraphics, text: &str, font: *mut GpFont, sf: *mut GpStringFormat,
) -> (f32, f32) {
    let text_wide = wstr(text);
    let layout = RectF { X: 0.0, Y: 0.0, Width: 100_000.0, Height: 100_000.0 };
    let mut bounds = RectF { X: 0.0, Y: 0.0, Width: 0.0, Height: 0.0 };
    let mut cp = 0i32;
    let mut lines = 0i32;
    GdipMeasureString(
        g, text_wide.as_ptr(), (text_wide.len() - 1) as i32,
        font, &layout, sf, &mut bounds, &mut cp, &mut lines,
    );
    (bounds.Width, bounds.Height)
}

/// COLORREF (`0x00BBGGRR`, R in low byte) → GDI+ ARGB i32 (`0xAARRGGBB`).
fn colorref_to_argb(colorref: u32, alpha: u8) -> u32 {
    let r = colorref & 0xff;
    let g = (colorref >> 8) & 0xff;
    let b = (colorref >> 16) & 0xff;
    ((alpha as u32) << 24) | (r << 16) | (g << 8) | b
}

// ─── Window proc ─────────────────────────────────────────────────────

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_LBUTTONDOWN => {
                let pt = lparam_to_point(lp);
                DRAGGING.store(false, Ordering::Relaxed);
                DRAG_START.with(|c| c.set(pt));
                SetCapture(hwnd);
                0
            }
            WM_MOUSEMOVE => {
                if (wp & MK_LBUTTON as usize) != 0 {
                    let pt = lparam_to_point(lp);
                    let start = DRAG_START.with(|c| c.get());
                    if (pt.x - start.x).abs() > 3 || (pt.y - start.y).abs() > 3 {
                        DRAGGING.store(true, Ordering::Relaxed);
                    }
                    if DRAGGING.load(Ordering::Relaxed) {
                        let mut rc: RECT = std::mem::zeroed();
                        GetWindowRect(hwnd, &mut rc);
                        let dx = pt.x - start.x;
                        let dy = pt.y - start.y;
                        let w = rc.right - rc.left;
                        let h = rc.bottom - rc.top;
                        let (cx, cy) = clamp_to_virtual_screen(rc.left + dx, rc.top + dy, w, h);
                        SetWindowPos(hwnd, null_mut(), cx, cy, 0, 0,
                                     SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                        render();
                    }
                }
                0
            }
            WM_LBUTTONUP => {
                ReleaseCapture();
                if DRAGGING.load(Ordering::Relaxed) {
                    let mut rc: RECT = std::mem::zeroed();
                    GetWindowRect(hwnd, &mut rc);
                    let mut cfg = crate::config_store::load();
                    cfg.widget_x = Some(rc.left);
                    cfg.widget_y = Some(rc.top);
                    let _ = crate::config_store::save(&cfg);
                }
                DRAGGING.store(false, Ordering::Relaxed);
                0
            }
            WM_TIMER => {
                if wp == TIMER_TOPMOST {
                    SetWindowPos(hwnd, HWND_TOPMOST,
                        0, 0, 0, 0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
                }
                0
            }
            WM_WINDOWPOSCHANGING => {
                let pos = lp as *mut WINDOWPOS;
                if !pos.is_null() && ((*pos).flags & SWP_NOZORDER) == 0 {
                    (*pos).hwndInsertAfter = HWND_TOPMOST;
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_DESTROY => {
                KillTimer(hwnd, TIMER_TOPMOST);
                set_overlay_hwnd(null_mut());
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

/// Clamp `(x, y)` to the full-monitor area of whichever monitor it's
/// closest to. We use rcMonitor (not rcWork) so the user can drag the
/// overlay over the taskbar's footprint; the topmost re-assertion timer
/// keeps it visually above.
unsafe fn clamp_to_virtual_screen(x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
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
