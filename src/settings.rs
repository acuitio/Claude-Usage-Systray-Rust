// Settings dialog — wired to config_store. Loads config.json on open,
// writes it back atomically on Apply. Schema identical to the C# version
// (src/Shared/Models.cs) so config files are swappable between ports.
//
// Layout sections (top→bottom):
//   General      — 4 checkboxes (startup, on-top, bg-collect, auto-refresh)
//   Display      — display mode combo, overlay opacity, display scale
//   Refresh      — poll interval, refresh display mode combo
//   Overlay      — overlay format edit
//   Display Opts — 5 show-* checkboxes + font family combo
//   Colors       — bg, text, sufficient, partial, depleted (5 picker buttons)
//   Buttons      — Reset / Cancel / Apply / Save & Exit

use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::Dialogs::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;
use crate::{config_store, models::AppConfig};

// ─── Statics ──────────────────────────────────────────────────────────
static mut HWND_SETTINGS: HWND = null_mut();

// Color picker buttons need HWND tracked so we can update their label.
static mut HWND_BG_BTN:   HWND = null_mut();
static mut HWND_TEXT_BTN: HWND = null_mut();
static mut HWND_SUF_BTN:  HWND = null_mut();
static mut HWND_PART_BTN: HWND = null_mut();
static mut HWND_DEPL_BTN: HWND = null_mut();

// Live colors in COLORREF form for the dialog session. populate_from_config
// seeds them from disk; the picker updates them; collect_into_config writes
// them back to the AppConfig.
static mut BG_COLOR:   u32 = 0x002e_1e1e;
static mut TEXT_COLOR: u32 = 0x00ff_ffff;
static mut SUF_COLOR:  u32 = 0x0064_c864;
static mut PART_COLOR: u32 = 0x0032_c8e6;
static mut DEPL_COLOR: u32 = 0x0050_50e6;

// ─── Control IDs ──────────────────────────────────────────────────────
const ID_CB_STARTUP: u16          = 7100;
const ID_CB_ONTOP: u16            = 7101;
const ID_CB_BACKGROUND: u16       = 7102;
const ID_CB_AUTOREFRESH: u16      = 7103;
const ID_COMBO_MODE: u16          = 7104;
const ID_EDIT_POLLSEC: u16        = 7105;
const ID_UD_POLLSEC: u16          = 7106;
const ID_EDIT_OVERLAYFMT: u16     = 7107;
const ID_BTN_BG_COLOR: u16        = 7108;
const ID_BTN_TEXT_COLOR: u16      = 7109;
const ID_BTN_APPLY: u16           = 7110;
const ID_BTN_SAVEEXIT: u16        = 7111;
const ID_BTN_RESET: u16           = 7112;
const ID_BTN_CANCEL: u16          = 7113;
const ID_EDIT_OPACITY: u16        = 7114;
const ID_UD_OPACITY: u16          = 7115;
const ID_EDIT_SCALE: u16          = 7116;
const ID_UD_SCALE: u16            = 7117;
const ID_COMBO_REFRESH_MODE: u16  = 7118;
const ID_COMBO_FONT: u16          = 7119;
const ID_CB_SHOW_SESSION: u16     = 7120;
const ID_CB_SHOW_WEEKLY: u16      = 7121;
const ID_CB_SHOW_SONNET: u16      = 7122;
const ID_CB_SHOW_DEPLETION: u16   = 7123;
const ID_CB_SHOW_LAST_REFRESH: u16= 7124;
const ID_BTN_SUF_COLOR: u16       = 7125;
const ID_BTN_PART_COLOR: u16      = 7126;
const ID_BTN_DEPL_COLOR: u16      = 7127;

// ─── Font family choices ──────────────────────────────────────────────
// Same set as the C# SettingsForm.cs.
const FONT_FAMILIES: &[&str] = &[
    "Segoe UI", "Segoe UI Variable", "Bahnschrift", "Cascadia Mono",
    "Calibri", "Tahoma", "Verdana", "Arial", "Consolas", "Cambria",
];

// ─── Open / build ─────────────────────────────────────────────────────

pub unsafe fn open(_owner: HWND) {
    if !HWND_SETTINGS.is_null() && IsWindow(HWND_SETTINGS) != 0 {
        SetForegroundWindow(HWND_SETTINGS);
        return;
    }
    let class_name = w!("Win32SettingsRustClass");
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

    // Sized to fit all sections without scrolling on typical laptop screens.
    // Sizable border lets users resize/maximize if they need to.
    HWND_SETTINGS = CreateWindowExW(
        0, class_name, w!("Settings"),
        WS_OVERLAPPEDWINDOW | WS_VISIBLE,
        CW_USEDEFAULT, CW_USEDEFAULT, 620, 1100,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    SetWindowPos(HWND_SETTINGS, HWND_TOP, 0, 0, 0, 0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
    UpdateWindow(HWND_SETTINGS);
    SetForegroundWindow(HWND_SETTINGS);
}

unsafe fn build_controls(parent: HWND) {
    let (x, mut y, w) = (24, 18, 560);

    macro_rules! header {
        ($t:expr) => {{
            let h = create_child(parent, w!("STATIC"), w!($t),
                WS_CHILD | WS_VISIBLE, x, y, w, 22, 0);
            set_font(h, FONT_BOLD);
            y += 28;
        }};
    }
    macro_rules! sep {
        () => {{
            create_child(parent, w!("STATIC"), std::ptr::null(),
                WS_CHILD | WS_VISIBLE | SS_ETCHEDFRAME, x, y, w, 1, 0);
            y += 14;
        }};
    }
    macro_rules! checkbox {
        ($label:expr, $id:expr) => {{
            let cb = create_child(parent, w!("BUTTON"), w!($label),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX as u32,
                x, y, w, 22, $id);
            set_font(cb, FONT_REG);
            y += 26;
        }};
    }
    macro_rules! sublabel {
        ($t:expr) => {{
            let h = create_child(parent, w!("STATIC"), w!($t),
                WS_CHILD | WS_VISIBLE, x, y, w, 18, 0);
            set_font(h, FONT_REG);
            y += 22;
        }};
    }

    // ── General ──
    header!("General");
    checkbox!("Launch widget on Windows startup", ID_CB_STARTUP);
    checkbox!("Dashboard always on top",          ID_CB_ONTOP);
    checkbox!("Auto-refresh OAuth token (Path 1)", ID_CB_AUTOREFRESH);
    y += 4;
    sep!();

    // ── Display Mode ──
    header!("Display Mode");
    let combo = create_child(parent, w!("COMBOBOX"), std::ptr::null(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL
            | CBS_DROPDOWNLIST as u32 | CBS_HASSTRINGS as u32,
        x, y, 220, 200, ID_COMBO_MODE);
    SendMessageW(combo, CB_ADDSTRING, 0, w!("tray") as LPARAM);
    SendMessageW(combo, CB_ADDSTRING, 0, w!("overlay") as LPARAM);
    set_font(combo, FONT_REG);
    y += 36;

    sublabel!("Overlay opacity (0-100)");
    spinner(parent, ID_EDIT_OPACITY, ID_UD_OPACITY, 0, 100, x, y);
    y += 32;

    sublabel!("Display scale (1-400)");
    spinner(parent, ID_EDIT_SCALE, ID_UD_SCALE, 1, 400, x, y);
    y += 32;
    sep!();

    // ── Refresh ──
    header!("Refresh");
    sublabel!("Interval (seconds, 1-1800)");
    spinner(parent, ID_EDIT_POLLSEC, ID_UD_POLLSEC, 1, 1800, x, y);
    y += 32;

    sublabel!("Display mode (exact vs approximate)");
    let combo = create_child(parent, w!("COMBOBOX"), std::ptr::null(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL
            | CBS_DROPDOWNLIST as u32 | CBS_HASSTRINGS as u32,
        x, y, 220, 200, ID_COMBO_REFRESH_MODE);
    SendMessageW(combo, CB_ADDSTRING, 0, w!("exact") as LPARAM);
    SendMessageW(combo, CB_ADDSTRING, 0, w!("approximate") as LPARAM);
    set_font(combo, FONT_REG);
    y += 36;
    sep!();

    // ── Overlay format ──
    header!("Overlay format");
    let fmt = create_child(parent, w!("EDIT"), w!(""),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER
            | ES_AUTOHSCROLL as u32 | ES_LEFT as u32,
        x, y, w, 26, ID_EDIT_OVERLAYFMT);
    set_font(fmt, FONT_REG);
    y += 36;
    sep!();

    // ── Display Options ──
    header!("Display Options");
    checkbox!("Show Session (5-hour)",                       ID_CB_SHOW_SESSION);
    checkbox!("Show Weekly (All Models)",                    ID_CB_SHOW_WEEKLY);
    checkbox!("Show Weekly (Sonnet)",                        ID_CB_SHOW_SONNET);
    checkbox!("Show depletion estimates under each bar",     ID_CB_SHOW_DEPLETION);
    checkbox!("Display \"Time since last refresh\"",         ID_CB_SHOW_LAST_REFRESH);

    sublabel!("Font family");
    let font_combo = create_child(parent, w!("COMBOBOX"), std::ptr::null(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL
            | CBS_DROPDOWNLIST as u32 | CBS_HASSTRINGS as u32,
        x, y, 260, 240, ID_COMBO_FONT);
    set_font(font_combo, FONT_REG);
    for family in FONT_FAMILIES {
        let s = wstr(family);
        SendMessageW(font_combo, CB_ADDSTRING, 0, s.as_ptr() as LPARAM);
    }
    y += 36;
    sep!();

    // ── Colors ──
    header!("Colors");
    for (label, id, hwnd_slot) in [
        (w!("Background"),       ID_BTN_BG_COLOR,   &raw mut HWND_BG_BTN),
        (w!("Text"),             ID_BTN_TEXT_COLOR, &raw mut HWND_TEXT_BTN),
        (w!("Sufficient (<50%)"),ID_BTN_SUF_COLOR,  &raw mut HWND_SUF_BTN),
        (w!("Partial (50-90%)"), ID_BTN_PART_COLOR, &raw mut HWND_PART_BTN),
        (w!("Depleted (>=90%)"), ID_BTN_DEPL_COLOR, &raw mut HWND_DEPL_BTN),
    ] {
        let lbl = create_child(parent, w!("STATIC"), label,
            WS_CHILD | WS_VISIBLE, x, y + 6, 200, 22, 0);
        set_font(lbl, FONT_REG);
        let btn = create_child(parent, w!("BUTTON"), w!(""),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
            x + w - 140, y, 140, 30, id);
        set_font(btn, FONT_REG);
        *hwnd_slot = btn;
        y += 36;
    }
    sep!();

    // ── Buttons ──
    let by = y + 10;
    for (label, id, bx, bw, def) in [
        (w!("Reset to Defaults"), ID_BTN_RESET,    x,            150, false),
        (w!("Cancel"),            ID_BTN_CANCEL,   x + w - 220,  100, false),
        (w!("Apply"),             ID_BTN_APPLY,    x + w - 320,   90, false),
        (w!("Save && Exit"),      ID_BTN_SAVEEXIT, x + w - 120,  120, true),
    ] {
        let style = WS_CHILD | WS_VISIBLE | WS_TABSTOP
            | if def { BS_DEFPUSHBUTTON } else { BS_PUSHBUTTON } as u32;
        let btn = create_child(parent, w!("BUTTON"), label, style, bx, by, bw, 32, id);
        set_font(btn, FONT_REG);
    }
}

unsafe fn spinner(parent: HWND, edit_id: u16, ud_id: u16, lo: i32, hi: i32, x: i32, y: i32) {
    let edit = create_child(parent, w!("EDIT"), w!(""),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER
            | ES_NUMBER as u32 | ES_LEFT as u32,
        x, y, 80, 26, edit_id);
    set_font(edit, FONT_REG);
    let ud = create_child(parent, UPDOWN_CLASSW, std::ptr::null(),
        WS_CHILD | WS_VISIBLE | UDS_SETBUDDYINT | UDS_ALIGNRIGHT | UDS_ARROWKEYS,
        0, 0, 0, 0, ud_id);
    SendMessageW(ud, UDM_SETBUDDY, edit as WPARAM, 0);
    SendMessageW(ud, UDM_SETRANGE32, lo as WPARAM, hi as LPARAM);
}

// ─── Config ↔ controls ──────────────────────────────────────────────

unsafe fn populate_from_config(parent: HWND, cfg: &AppConfig) {
    set_check(parent, ID_CB_STARTUP,           cfg.start_on_startup);
    set_check(parent, ID_CB_ONTOP,             cfg.dashboard_on_top);
    set_check(parent, ID_CB_AUTOREFRESH,       cfg.auto_refresh_token);
    set_check(parent, ID_CB_SHOW_SESSION,      cfg.show_session);
    set_check(parent, ID_CB_SHOW_WEEKLY,       cfg.show_weekly);
    set_check(parent, ID_CB_SHOW_SONNET,       cfg.show_sonnet);
    set_check(parent, ID_CB_SHOW_DEPLETION,    cfg.show_depletion_estimates);
    set_check(parent, ID_CB_SHOW_LAST_REFRESH, cfg.show_last_refresh);

    // Display mode combo
    let combo = GetDlgItem(parent, ID_COMBO_MODE as i32);
    SendMessageW(combo, CB_SETCURSEL,
        if cfg.display_mode == "overlay" { 1 } else { 0 } as WPARAM, 0);

    // Refresh display mode combo
    let combo = GetDlgItem(parent, ID_COMBO_REFRESH_MODE as i32);
    SendMessageW(combo, CB_SETCURSEL,
        if cfg.refresh_display_mode == "approximate" { 1 } else { 0 } as WPARAM, 0);

    // Font combo
    let combo = GetDlgItem(parent, ID_COMBO_FONT as i32);
    let idx = FONT_FAMILIES.iter().position(|f| *f == cfg.font_family).unwrap_or(0) as i32;
    SendMessageW(combo, CB_SETCURSEL, idx as WPARAM, 0);

    // Numeric spinners
    set_spinner(parent, ID_UD_POLLSEC, cfg.poll_interval_sec);
    set_spinner(parent, ID_UD_OPACITY, cfg.overlay_opacity);
    set_spinner(parent, ID_UD_SCALE,   cfg.scale_pct);

    set_edit_text(parent, ID_EDIT_OVERLAYFMT, &cfg.overlay_format);

    // Color buttons
    BG_COLOR   = hex_to_colorref(&cfg.bg_color).unwrap_or(0x002e_1e1e);
    TEXT_COLOR = hex_to_colorref(&cfg.color_text).unwrap_or(0x00ff_ffff);
    SUF_COLOR  = hex_to_colorref(&cfg.color_sufficient).unwrap_or(0x0064_c864);
    PART_COLOR = hex_to_colorref(&cfg.color_partial).unwrap_or(0x0032_c8e6);
    DEPL_COLOR = hex_to_colorref(&cfg.color_depleted).unwrap_or(0x0050_50e6);
    SetWindowTextW(HWND_BG_BTN,   color_hex(BG_COLOR).as_ptr());
    SetWindowTextW(HWND_TEXT_BTN, color_hex(TEXT_COLOR).as_ptr());
    SetWindowTextW(HWND_SUF_BTN,  color_hex(SUF_COLOR).as_ptr());
    SetWindowTextW(HWND_PART_BTN, color_hex(PART_COLOR).as_ptr());
    SetWindowTextW(HWND_DEPL_BTN, color_hex(DEPL_COLOR).as_ptr());
}

unsafe fn collect_into_config(parent: HWND, cfg: &mut AppConfig) {
    cfg.start_on_startup         = get_check(parent, ID_CB_STARTUP);
    cfg.dashboard_on_top         = get_check(parent, ID_CB_ONTOP);
    cfg.auto_refresh_token       = get_check(parent, ID_CB_AUTOREFRESH);
    cfg.show_session             = get_check(parent, ID_CB_SHOW_SESSION);
    cfg.show_weekly              = get_check(parent, ID_CB_SHOW_WEEKLY);
    cfg.show_sonnet              = get_check(parent, ID_CB_SHOW_SONNET);
    cfg.show_depletion_estimates = get_check(parent, ID_CB_SHOW_DEPLETION);
    cfg.show_last_refresh        = get_check(parent, ID_CB_SHOW_LAST_REFRESH);

    let combo = GetDlgItem(parent, ID_COMBO_MODE as i32);
    let idx = SendMessageW(combo, CB_GETCURSEL, 0, 0) as i32;
    cfg.display_mode = if idx == 1 { "overlay".into() } else { "tray".into() };

    let combo = GetDlgItem(parent, ID_COMBO_REFRESH_MODE as i32);
    let idx = SendMessageW(combo, CB_GETCURSEL, 0, 0) as i32;
    cfg.refresh_display_mode = if idx == 1 { "approximate".into() } else { "exact".into() };

    let combo = GetDlgItem(parent, ID_COMBO_FONT as i32);
    let idx = SendMessageW(combo, CB_GETCURSEL, 0, 0) as i32;
    if idx >= 0 && (idx as usize) < FONT_FAMILIES.len() {
        cfg.font_family = FONT_FAMILIES[idx as usize].into();
    }

    cfg.poll_interval_sec = get_spinner(parent, ID_UD_POLLSEC).clamp(1, 1800);
    cfg.overlay_opacity   = get_spinner(parent, ID_UD_OPACITY).clamp(0, 100);
    cfg.scale_pct         = get_spinner(parent, ID_UD_SCALE).clamp(1, 400);

    cfg.overlay_format = get_edit_text(parent, ID_EDIT_OVERLAYFMT)
        .unwrap_or_else(|| cfg.overlay_format.clone());

    cfg.bg_color         = colorref_to_hex(BG_COLOR);
    cfg.color_text       = colorref_to_hex(TEXT_COLOR);
    cfg.color_sufficient = colorref_to_hex(SUF_COLOR);
    cfg.color_partial    = colorref_to_hex(PART_COLOR);
    cfg.color_depleted   = colorref_to_hex(DEPL_COLOR);
}

unsafe fn set_check(parent: HWND, id: u16, on: bool) {
    let h = GetDlgItem(parent, id as i32);
    SendMessageW(h, BM_SETCHECK,
        if on { BST_CHECKED } else { BST_UNCHECKED } as WPARAM, 0);
}

unsafe fn get_check(parent: HWND, id: u16) -> bool {
    let h = GetDlgItem(parent, id as i32);
    SendMessageW(h, BM_GETCHECK, 0, 0) == BST_CHECKED as LRESULT
}

unsafe fn set_edit_text(parent: HWND, id: u16, text: &str) {
    let h = GetDlgItem(parent, id as i32);
    let s = wstr(text);
    SetWindowTextW(h, s.as_ptr());
}

unsafe fn get_edit_text(parent: HWND, id: u16) -> Option<String> {
    let h = GetDlgItem(parent, id as i32);
    let len = GetWindowTextLengthW(h);
    if len < 0 { return None; }
    let mut buf = vec![0u16; (len as usize) + 1];
    let copied = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
    buf.truncate(copied as usize);
    Some(String::from_utf16_lossy(&buf))
}

unsafe fn set_spinner(parent: HWND, ud_id: u16, value: i32) {
    let h = GetDlgItem(parent, ud_id as i32);
    SendMessageW(h, UDM_SETPOS32, 0, value as LPARAM);
}

unsafe fn get_spinner(parent: HWND, ud_id: u16) -> i32 {
    let h = GetDlgItem(parent, ud_id as i32);
    SendMessageW(h, UDM_GETPOS32, 0, 0) as i32
}

// ─── Color hex (#RRGGBB) ↔ COLORREF (0x00BBGGRR) ───────────────────

pub fn hex_to_colorref(s: &str) -> Option<u32> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 { return None; }
    let r = u32::from_str_radix(&s[0..2], 16).ok()?;
    let g = u32::from_str_radix(&s[2..4], 16).ok()?;
    let b = u32::from_str_radix(&s[4..6], 16).ok()?;
    Some(r | (g << 8) | (b << 16))
}

pub fn colorref_to_hex(c: u32) -> String {
    let r = c & 0xff;
    let g = (c >> 8) & 0xff;
    let b = (c >> 16) & 0xff;
    format!("#{r:02x}{g:02x}{b:02x}")
}

// ─── Color picker ────────────────────────────────────────────────────

unsafe fn pick_color(owner: HWND, inout: &mut u32) -> bool {
    static mut CUSTOM: [u32; 16] = [0; 16];
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        hInstance: null_mut(),
        rgbResult: *inout,
        lpCustColors: (&raw mut CUSTOM) as *mut u32,
        Flags: CC_FULLOPEN | CC_RGBINIT | CC_ANYCOLOR,
        lCustData: 0,
        lpfnHook: None,
        lpTemplateName: std::ptr::null(),
    };
    if ChooseColorW(&mut cc) != 0 {
        *inout = cc.rgbResult;
        true
    } else {
        false
    }
}

// ─── Command handling ────────────────────────────────────────────────

unsafe fn apply_settings(hwnd: HWND) {
    let old_cfg = config_store::load();
    let mut cfg = old_cfg.clone();
    collect_into_config(hwnd, &mut cfg);

    // Sync the HKCU Run key if the startup-on-login toggle changed.
    if cfg.start_on_startup != old_cfg.start_on_startup {
        if cfg.start_on_startup {
            if let Ok(exe) = std::env::current_exe() {
                crate::startup_registry::set_enabled(Some(&exe.to_string_lossy()));
            }
        } else {
            crate::startup_registry::set_enabled(None);
        }
    }

    if let Err(e) = config_store::save(&cfg) {
        let msg = wstr(&format!("Failed to save settings:\n{e}"));
        MessageBoxW(hwnd, msg.as_ptr(), w!("Settings"), MB_OK | MB_ICONERROR);
        return;
    }

    // Push the new config out to all live UI surfaces so font / color /
    // opacity / overlay_format changes are visible immediately.
    crate::poll_service::notify_ui_refresh();
}

unsafe fn handle_command(hwnd: HWND, wp: WPARAM) {
    let id = (wp & 0xffff) as u16;
    match id {
        ID_BTN_APPLY    => { apply_settings(hwnd); }
        ID_BTN_SAVEEXIT => { apply_settings(hwnd); PostMessageW(hwnd, WM_CLOSE, 0, 0); }
        ID_BTN_RESET => {
            if MessageBoxW(hwnd, w!("Reset all settings to defaults?"),
                           w!("Reset"), MB_YESNO | MB_ICONINFORMATION) == IDYES {
                populate_from_config(hwnd, &AppConfig::default());
            }
        }
        ID_BTN_CANCEL => { PostMessageW(hwnd, WM_CLOSE, 0, 0); }

        ID_BTN_BG_COLOR   => if pick_color(hwnd, &mut BG_COLOR)   { SetWindowTextW(HWND_BG_BTN,   color_hex(BG_COLOR).as_ptr()); }
        ID_BTN_TEXT_COLOR => if pick_color(hwnd, &mut TEXT_COLOR) { SetWindowTextW(HWND_TEXT_BTN, color_hex(TEXT_COLOR).as_ptr()); }
        ID_BTN_SUF_COLOR  => if pick_color(hwnd, &mut SUF_COLOR)  { SetWindowTextW(HWND_SUF_BTN,  color_hex(SUF_COLOR).as_ptr()); }
        ID_BTN_PART_COLOR => if pick_color(hwnd, &mut PART_COLOR) { SetWindowTextW(HWND_PART_BTN, color_hex(PART_COLOR).as_ptr()); }
        ID_BTN_DEPL_COLOR => if pick_color(hwnd, &mut DEPL_COLOR) { SetWindowTextW(HWND_DEPL_BTN, color_hex(DEPL_COLOR).as_ptr()); }

        _ => {}
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                build_controls(hwnd);
                let cfg = config_store::load();
                populate_from_config(hwnd, &cfg);
                0
            }
            WM_COMMAND => { handle_command(hwnd, wp); 0 }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                SetBkColor(wp as HDC, BG_DARK);
                SetTextColor(wp as HDC, FG_LIGHT);
                HBR_BG as LRESULT
            }
            WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
                SetBkColor(wp as HDC, SURFACE);
                SetTextColor(wp as HDC, FG_LIGHT);
                HBR_SURFACE as LRESULT
            }
            WM_CLOSE => { DestroyWindow(hwnd); 0 }
            WM_DPICHANGED => { handle_dpi_changed(hwnd, lp); 0 }
            WM_DESTROY => { HWND_SETTINGS = null_mut(); 0 }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
