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
const ID_MENU_IMGPULL:    u16 = 9008;
const ID_MENU_QUIT:      u16 = 9005;

// The hidden host window that receives our tray callback. Set once at
// install() and read by refresh()/remove() — single-thread access in practice,
// but `AtomicPtr` is the cheapest way to satisfy Rust without `static mut`.
static TRAY_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(null_mut());

// Icon cache. Rebuilding the 64×64 icon on every poll wastes GDI cycles when
// the rounded percentage, color tier, and health badge haven't changed. Keyed
// on (pct_int, color, badge); only NIM_MODIFY when those move.
struct IconCache {
    pct_int: i32,
    color:   u32,
    badge:   u8,
    icon:    HICON,
}
thread_local! {
    static ICON_CACHE: RefCell<Option<IconCache>> = const { RefCell::new(None) };
}

// Health badge codes drawn in the icon's top-right corner. 0 = none.
const BADGE_NONE: u8 = 0;
const BADGE_STALE: u8 = 1; // orange — data old (cooldown/network/server)
const BADGE_AUTH:  u8 = 2; // red — token rejected, re-auth needed

/// Map the current health status to an icon badge code.
fn badge_code() -> u8 {
    match crate::health::status() {
        crate::health::Status::Live => BADGE_NONE,
        crate::health::Status::Stale => BADGE_STALE,
        crate::health::Status::Auth  => BADGE_AUTH,
    }
}

// Remembers the last health status seen by refresh() (UI thread only) so we can
// fire a one-time balloon on transitions into/out of the Auth state.
thread_local! {
    static PREV_STATUS: std::cell::Cell<u8> = const { std::cell::Cell::new(BADGE_NONE) };
}

// ─── Transfer indicator state ─────────────────────────────────────────
// While imgpaste/imgpull move bytes, the tray icon temporarily shows a
// directional arrow (pulsing) instead of the usage bars, then flashes a
// check/cross on completion before reverting. Worker threads signal phase
// changes via PostMessage(WM_XFER_STATE) to the host window; everything below
// runs on the UI thread that owns the icon.

/// Posted by imgpaste/imgpull worker threads to drive the indicator.
pub const WM_XFER_STATE: u32 = WM_APP + 3;
// wParam codes for WM_XFER_STATE.
pub const XFER_PUSH_BEGIN: usize = 0;
pub const XFER_PULL_BEGIN: usize = 1;
pub const XFER_END_OK:     usize = 2;
pub const XFER_END_FAIL:   usize = 3;
/// Timer id (on the host window) driving the pulse + auto-revert.
pub const TIMER_XFER: usize = 2;

// Number of ~250ms ticks the success/failure flash stays up before reverting.
const DONE_TICKS: u32 = 7;

#[derive(Clone, Copy)]
enum XferDir { Push, Pull }

#[derive(Clone, Copy)]
enum XferPhase { Active, Done { ok: bool } }

struct Xfer {
    dir:   XferDir,
    phase: XferPhase,
    frame: u32,
    icon:  HICON, // the xfer HICON we currently own; destroyed on replace/clear
}
thread_local! {
    static XFER: RefCell<Option<Xfer>> = const { RefCell::new(None) };
}

pub unsafe fn install(host: HWND) {
    TRAY_HWND.store(host, Ordering::Relaxed);

    let usage = current_snapshot();
    let icon  = get_or_build_icon(usage.session_pct, badge_code());
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

/// The hidden host (message-only) window that owns the tray icon and receives
/// posted messages. Exposed so Settings can re-register hotkeys against it.
pub fn host_hwnd() -> HWND {
    TRAY_HWND.load(Ordering::Relaxed)
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

        // The chords are user-configurable (Settings → Hotkeys), so the menu
        // labels must reflect what's actually registered rather than a
        // hardcoded default.
        let cfg = crate::config_store::load();
        let paste_label = wstr(&format!(
            "Send Clipboard Image/Files\t{}",
            chord_label(cfg.imgpaste_hotkey_mods, cfg.imgpaste_hotkey_vk)));
        let pull_label = wstr(&format!(
            "Get Remote → Clipboard\t{}",
            chord_label(cfg.imgpull_hotkey_mods, cfg.imgpull_hotkey_vk)));
        AppendMenuW(menu, MF_STRING, ID_MENU_IMGPASTE as usize, paste_label.as_ptr());
        AppendMenuW(menu, MF_STRING, ID_MENU_IMGPULL as usize, pull_label.as_ptr());
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
            ID_MENU_REFRESH   => {
                // "Refresh Now" refreshes usage *and* checks for a newer build.
                crate::poll_service::trigger_refresh();
                crate::update_service::trigger_check();
            }
            ID_MENU_CHART     => chart::open(host),
            ID_MENU_SETTINGS  => settings::open(host),
            ID_MENU_IMGPASTE   => crate::imgpaste::handle_hotkey(),
            ID_MENU_IMGPULL    => crate::imgpull::handle_hotkey(),
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
    // A transfer indicator is showing — don't stomp it with the usage icon.
    // The indicator restores the usage icon itself when it reverts to idle.
    if XFER.with(|c| c.borrow().is_some()) { return; }
    let badge = badge_code();
    maybe_notify_health_change(badge);
    let usage = current_snapshot();
    let icon  = get_or_build_icon(usage.session_pct, badge);
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd   = host;
    nid.uID    = 1;
    nid.uFlags = NIF_ICON | NIF_TIP;
    nid.hIcon  = icon;
    write_tooltip(&mut nid.szTip, &usage);
    Shell_NotifyIconW(NIM_MODIFY, &nid);
}

/// Fire a one-time balloon when health crosses into Auth (sign-in needed) or
/// recovers from it. Stale↔Live transitions stay silent — the icon badge and
/// tooltip carry those without nagging.
unsafe fn maybe_notify_health_change(badge: u8) {
    let prev = PREV_STATUS.with(|c| c.get());
    if badge == BADGE_AUTH && prev != BADGE_AUTH {
        notify(
            "Claude sign-in needed",
            "Usage updates are paused. Run `claude login`, then click Refresh Now.",
        );
    } else if prev == BADGE_AUTH && badge != BADGE_AUTH {
        notify("Usage updates resumed", "Sign-in restored — numbers are live again.");
    }
    PREV_STATUS.with(|c| c.set(badge));
}

unsafe fn write_tooltip(buf: &mut [u16], usage: &crate::common::UsageData) {
    let cd = crate::cooldown::remaining_seconds();
    let suffix = if cd > 0 {
        format!(" · Rate-limited {}", crate::cooldown::format(cd))
    } else {
        String::new()
    };
    // A status line makes a stale/auth-blocked reading honest instead of
    // letting an old number masquerade as current.
    let status_line = match crate::health::status() {
        crate::health::Status::Live => String::new(),
        crate::health::Status::Stale =>
            format!("\n⚠ Not updating — last OK {} ago", crate::health::cache_age_label()),
        crate::health::Status::Auth =>
            "\n⚠ Sign-in needed — run: claude login".to_string(),
    };
    let tip_str = format!(
        "Usage: {:.0}% | {:.0}% | {:.0}%\n{}{}{}",
        usage.session_pct, usage.weekly_pct, usage.fable_pct, usage.plan, suffix, status_line,
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

/// Show a tray balloon (NIF_INFO). Best-effort feedback — currently used by
/// imgpull to signal that a fetched file is staged on the clipboard and ready
/// to paste. No-op if the tray icon isn't installed yet.
pub unsafe fn notify(title: &str, body: &str) {
    let host = TRAY_HWND.load(Ordering::Relaxed);
    if host.is_null() { return; }
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd   = host;
    nid.uID    = 1;
    nid.uFlags = NIF_INFO;
    fill_wide(&mut nid.szInfo, body);
    fill_wide(&mut nid.szInfoTitle, title);
    nid.dwInfoFlags = NIIF_INFO;
    Shell_NotifyIconW(NIM_MODIFY, &nid);
}

/// Copy `s` as UTF-16 into a fixed-size buffer, truncating to fit and keeping
/// a trailing NUL.
fn fill_wide(buf: &mut [u16], s: &str) {
    let w = wstr(s); // chars + trailing NUL
    let n = w.len().min(buf.len());
    buf[..n].copy_from_slice(&w[..n]);
    if w.len() > buf.len() && !buf.is_empty() {
        buf[buf.len() - 1] = 0; // force termination when truncated
    }
}

/// Return the cached HICON if (rounded-pct, color-tier, badge) match the last
/// call; otherwise rebuild, destroy the previous handle, and cache the new one.
unsafe fn get_or_build_icon(pct: f64, badge: u8) -> HICON {
    let pct_int = pct.round() as i32;
    let color   = pct_color(pct);
    ICON_CACHE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if let Some(c) = slot.as_ref() {
            if c.pct_int == pct_int && c.color == color && c.badge == badge {
                return c.icon;
            }
            DestroyIcon(c.icon);
        }
        let icon = build_tray_icon(pct, badge);
        *slot = Some(IconCache { pct_int, color, badge, icon });
        icon
    })
}

// Render the bar-chart icon at 64×64, then convert to HICON.
unsafe fn build_tray_icon(pct: f64, badge: u8) -> HICON {
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

    // Health badge in the top-right corner when the data isn't live.
    if badge != BADGE_NONE {
        draw_health_badge(hdc_mem, badge);
    }

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

/// Draw a filled warning circle with a white "!" in the icon's top-right
/// corner. Orange for stale, red for auth — both read as "attention" even
/// shrunk to 16px in the tray.
unsafe fn draw_health_badge(hdc: HDC, badge: u8) {
    let color = if badge == BADGE_AUTH { 0x0030_30E6 } else { 0x000C_A5FF }; // red / orange
    // Circle in the top-right corner (64×64 canvas).
    let (l, t, r, b) = (34, 2, 62, 30);
    let brush = CreateSolidBrush(color);
    let pen   = CreatePen(PS_SOLID, 2, 0x00FF_FFFF); // white rim for contrast
    let ob = SelectObject(hdc, brush as HGDIOBJ);
    let op = SelectObject(hdc, pen as HGDIOBJ);
    Ellipse(hdc, l, t, r, b);
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    DeleteObject(brush as HGDIOBJ);
    DeleteObject(pen as HGDIOBJ);

    // White "!" centered in the circle.
    let font = CreateFontW(
        22, 0, 0, 0, FW_BOLD as i32,
        0, 0, 0, DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32, CLEARTYPE_QUALITY as u32,
        (DEFAULT_PITCH | FF_DONTCARE) as u32, w!("Arial"),
    );
    let old_font = SelectObject(hdc, font as HGDIOBJ);
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, 0x00FF_FFFF);
    let txt = wstr("!");
    let mut rc = RECT { left: l, top: t - 1, right: r, bottom: b };
    DrawTextW(hdc, txt.as_ptr(), -1, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    SelectObject(hdc, old_font);
    DeleteObject(font as HGDIOBJ);
}

// ─── Transfer indicator: signalling + lifecycle ───────────────────────

/// Called from worker threads to post a phase change to the UI thread. Safe to
/// call before the tray icon exists (no-op).
pub fn signal_xfer(code: usize) {
    let host = TRAY_HWND.load(Ordering::Relaxed);
    if !host.is_null() {
        unsafe { PostMessageW(host, WM_XFER_STATE, code, 0); }
    }
}

/// Host-window handler for WM_XFER_STATE (runs on the UI thread).
pub unsafe fn on_xfer_message(host: HWND, code: WPARAM) {
    match code {
        XFER_PUSH_BEGIN => xfer_begin(host, XferDir::Push),
        XFER_PULL_BEGIN => xfer_begin(host, XferDir::Pull),
        XFER_END_OK     => xfer_end(host, true),
        XFER_END_FAIL   => xfer_end(host, false),
        _ => {}
    }
}

unsafe fn xfer_begin(host: HWND, dir: XferDir) {
    clear_xfer_icon();
    XFER.with(|c| {
        *c.borrow_mut() = Some(Xfer { dir, phase: XferPhase::Active, frame: 0, icon: null_mut() });
    });
    let tip = match dir { XferDir::Push => "Sending…", XferDir::Pull => "Fetching…" };
    render_xfer(host, Some(tip));
    SetTimer(host, TIMER_XFER, 250, None);
}

unsafe fn xfer_end(host: HWND, ok: bool) {
    XFER.with(|c| {
        let mut slot = c.borrow_mut();
        match slot.as_mut() {
            Some(x) => { x.phase = XferPhase::Done { ok }; x.frame = 0; }
            // End without a matching begin (e.g. the begin message was missed)
            // — flash the result anyway. Direction is irrelevant for the flash.
            None => *slot = Some(Xfer {
                dir: XferDir::Push, phase: XferPhase::Done { ok }, frame: 0, icon: null_mut(),
            }),
        }
    });
    render_xfer(host, Some(if ok { "Done" } else { "Failed" }));
    SetTimer(host, TIMER_XFER, 250, None); // ensure the auto-revert timer runs
}

/// Host-window WM_TIMER handler for TIMER_XFER.
pub unsafe fn xfer_tick(host: HWND) {
    enum Act { None, Pulse, Revert }
    let act = XFER.with(|c| {
        let mut slot = c.borrow_mut();
        match slot.as_mut() {
            None => Act::Revert, // stray tick — make sure the timer dies
            Some(x) => {
                x.frame += 1;
                match x.phase {
                    XferPhase::Active => Act::Pulse,
                    XferPhase::Done { .. } if x.frame >= DONE_TICKS => Act::Revert,
                    XferPhase::Done { .. } => Act::None,
                }
            }
        }
    });
    match act {
        Act::Pulse  => render_xfer(host, None),
        Act::Revert => {
            clear_xfer_icon();
            KillTimer(host, TIMER_XFER);
            refresh(); // XFER is now empty, so this restores the usage icon
        }
        Act::None => {}
    }
}

/// Destroy and forget the owned xfer HICON, clearing the state.
unsafe fn clear_xfer_icon() {
    XFER.with(|c| {
        if let Some(x) = c.borrow_mut().take() {
            if !x.icon.is_null() { DestroyIcon(x.icon); }
        }
    });
}

/// Render the current xfer state into the tray icon (NIM_MODIFY), replacing
/// and destroying the previously owned HICON.
unsafe fn render_xfer(host: HWND, tip: Option<&str>) {
    XFER.with(|c| {
        let mut slot = c.borrow_mut();
        if let Some(x) = slot.as_mut() {
            let icon = build_xfer_icon(x.dir, x.phase, x.frame);
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd   = host;
            nid.uID    = 1;
            nid.uFlags = NIF_ICON;
            nid.hIcon  = icon;
            if let Some(t) = tip {
                nid.uFlags |= NIF_TIP;
                fill_wide(&mut nid.szTip, t);
            }
            Shell_NotifyIconW(NIM_MODIFY, &nid);
            if !x.icon.is_null() { DestroyIcon(x.icon); }
            x.icon = icon;
        }
    });
}

/// Render a 64×64 indicator icon: a solid phase-colored square with a white
/// arrow (transferring), check (success), or cross (failure).
unsafe fn build_xfer_icon(dir: XferDir, phase: XferPhase, frame: u32) -> HICON {
    let hdc_screen = GetDC(null_mut());
    let hdc_mem    = CreateCompatibleDC(hdc_screen);
    let bmp        = CreateCompatibleBitmap(hdc_screen, 64, 64);
    let old_bmp    = SelectObject(hdc_mem, bmp as HGDIOBJ);

    // Background color by phase (COLORREF = 0x00BBGGRR). Active pulses between
    // two blues on alternating frames.
    let bg = match phase {
        XferPhase::Active => if frame.is_multiple_of(2) { 0x00E6_5F2D } else { 0x0096_3C1E },
        XferPhase::Done { ok: true }  => 0x0046_AA28, // green
        XferPhase::Done { ok: false } => 0x0032_32C8, // red
    };
    let brush_bg = CreateSolidBrush(bg);
    let rc = RECT { left: 0, top: 0, right: 64, bottom: 64 };
    FillRect(hdc_mem, &rc, brush_bg);
    DeleteObject(brush_bg as HGDIOBJ);

    match phase {
        XferPhase::Active => draw_arrow(hdc_mem, matches!(dir, XferDir::Push)),
        XferPhase::Done { ok: true }  => draw_check(hdc_mem),
        XferPhase::Done { ok: false } => draw_cross(hdc_mem),
    }

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

const XFER_WHITE: u32 = 0x00FF_FFFF;

/// Filled white arrow (7-point polygon) pointing up (push) or down (pull).
unsafe fn draw_arrow(hdc: HDC, up: bool) {
    let pts: [POINT; 7] = if up {
        [ POINT { x: 32, y: 6 },  POINT { x: 54, y: 30 }, POINT { x: 42, y: 30 },
          POINT { x: 42, y: 54 }, POINT { x: 22, y: 54 }, POINT { x: 22, y: 30 },
          POINT { x: 10, y: 30 } ]
    } else {
        [ POINT { x: 32, y: 58 }, POINT { x: 54, y: 34 }, POINT { x: 42, y: 34 },
          POINT { x: 42, y: 10 }, POINT { x: 22, y: 10 }, POINT { x: 22, y: 34 },
          POINT { x: 10, y: 34 } ]
    };
    let brush = CreateSolidBrush(XFER_WHITE);
    let pen   = CreatePen(PS_SOLID, 1, XFER_WHITE);
    let ob = SelectObject(hdc, brush as HGDIOBJ);
    let op = SelectObject(hdc, pen as HGDIOBJ);
    Polygon(hdc, pts.as_ptr(), pts.len() as i32);
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    DeleteObject(brush as HGDIOBJ);
    DeleteObject(pen as HGDIOBJ);
}

/// Thick white check mark.
unsafe fn draw_check(hdc: HDC) {
    let pen = CreatePen(PS_SOLID, 9, XFER_WHITE);
    let op = SelectObject(hdc, pen as HGDIOBJ);
    MoveToEx(hdc, 14, 34, null_mut());
    LineTo(hdc, 28, 48);
    LineTo(hdc, 52, 16);
    SelectObject(hdc, op);
    DeleteObject(pen as HGDIOBJ);
}

/// Thick white cross.
unsafe fn draw_cross(hdc: HDC) {
    let pen = CreatePen(PS_SOLID, 9, XFER_WHITE);
    let op = SelectObject(hdc, pen as HGDIOBJ);
    MoveToEx(hdc, 16, 16, null_mut());
    LineTo(hdc, 48, 48);
    MoveToEx(hdc, 48, 16, null_mut());
    LineTo(hdc, 16, 48);
    SelectObject(hdc, op);
    DeleteObject(pen as HGDIOBJ);
}
