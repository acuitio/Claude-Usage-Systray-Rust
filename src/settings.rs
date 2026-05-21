// Settings dialog — wired to config_store. Loads config.json on open,
// writes it back atomically on Apply. Schema identical to the C# version
// (src/Shared/Models.cs) so config files are swappable between ports.

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

static mut HWND_SETTINGS: HWND = null_mut();
static mut HWND_BG_BTN: HWND = null_mut();
static mut HWND_TEXT_BTN: HWND = null_mut();
static mut BG_COLOR: u32 = 0x002e_1e1e;
static mut TEXT_COLOR: u32 = 0x00ff_ffff;

const ID_CB_STARTUP: u16       = 7100;
const ID_CB_ONTOP: u16         = 7101;
const ID_CB_BACKGROUND: u16    = 7102;
const ID_CB_AUTOREFRESH: u16   = 7103;
const ID_COMBO_MODE: u16       = 7104;
const ID_EDIT_POLLSEC: u16     = 7105;
const ID_UD_POLLSEC: u16       = 7106;
const ID_EDIT_OVERLAYFMT: u16  = 7107;
const ID_BTN_BG_COLOR: u16     = 7108;
const ID_BTN_TEXT_COLOR: u16   = 7109;
const ID_BTN_APPLY: u16        = 7110;
const ID_BTN_SAVEEXIT: u16     = 7111;
const ID_BTN_RESET: u16        = 7112;
const ID_BTN_CANCEL: u16       = 7113;

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

    // WS_VISIBLE in the initial style + SetWindowPos avoids the same
    // "shown-but-never-paints" race we hit on the dashboard window when
    // open() is called from inside the tray-menu message handler.
    HWND_SETTINGS = CreateWindowExW(
        0, class_name, w!("Settings"),
        WS_OVERLAPPEDWINDOW | WS_VISIBLE,
        CW_USEDEFAULT, CW_USEDEFAULT, 600, 760,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    SetWindowPos(HWND_SETTINGS, HWND_TOP, 0, 0, 0, 0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
    UpdateWindow(HWND_SETTINGS);
    SetForegroundWindow(HWND_SETTINGS);
}

unsafe fn build_controls(parent: HWND) {
    let (x, mut y, w) = (24, 18, 540);

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

    header!("General");
    for (label, id) in [
        (w!("Launch widget on Windows startup"),                       ID_CB_STARTUP),
        (w!("Dashboard always on top"),                                ID_CB_ONTOP),
        (w!("Collect usage data in the background (every 10 min)"),    ID_CB_BACKGROUND),
        (w!("Auto-refresh OAuth token (Path 1)"),                      ID_CB_AUTOREFRESH),
    ] {
        let cb = create_child(parent, w!("BUTTON"), label,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            x, y, w, 22, id);
        set_font(cb, FONT_REG);
        y += 26;
    }
    y += 4;
    sep!();

    header!("Display Mode");
    let combo = create_child(parent, w!("COMBOBOX"), std::ptr::null(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL
            | CBS_DROPDOWNLIST as u32 | CBS_HASSTRINGS as u32,
        x, y, 220, 200, ID_COMBO_MODE);
    SendMessageW(combo, CB_ADDSTRING, 0, w!("tray") as LPARAM);
    SendMessageW(combo, CB_ADDSTRING, 0, w!("overlay") as LPARAM);
    set_font(combo, FONT_REG);
    y += 36;
    sep!();

    header!("Refresh interval (seconds, 1-1800)");
    let edit = create_child(parent, w!("EDIT"), w!(""),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER
            | ES_NUMBER as u32 | ES_LEFT as u32,
        x, y, 80, 26, ID_EDIT_POLLSEC);
    set_font(edit, FONT_REG);
    let ud = create_child(parent, UPDOWN_CLASSW, std::ptr::null(),
        WS_CHILD | WS_VISIBLE | UDS_SETBUDDYINT | UDS_ALIGNRIGHT | UDS_ARROWKEYS,
        0, 0, 0, 0, ID_UD_POLLSEC);
    SendMessageW(ud, UDM_SETBUDDY, edit as WPARAM, 0);
    SendMessageW(ud, UDM_SETRANGE32, 1, 1800);
    y += 36;
    sep!();

    header!("Overlay format");
    let fmt = create_child(parent, w!("EDIT"), w!(""),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER
            | ES_AUTOHSCROLL as u32 | ES_LEFT as u32,
        x, y, w, 26, ID_EDIT_OVERLAYFMT);
    set_font(fmt, FONT_REG);
    y += 36;
    sep!();

    header!("Colors");
    for (label, id, hwnd_slot) in [
        (w!("Background"), ID_BTN_BG_COLOR,   &raw mut HWND_BG_BTN),
        (w!("Text"),       ID_BTN_TEXT_COLOR, &raw mut HWND_TEXT_BTN),
    ] {
        let lbl = create_child(parent, w!("STATIC"), label,
            WS_CHILD | WS_VISIBLE, x, y + 6, 150, 22, 0);
        set_font(lbl, FONT_REG);
        let btn = create_child(parent, w!("BUTTON"), w!(""),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
            x + w - 140, y, 140, 30, id);
        set_font(btn, FONT_REG);
        *hwnd_slot = btn;
        y += 36;
    }
    sep!();

    // Button row
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

// ─── Config ↔ controls ──────────────────────────────────────────────

unsafe fn populate_from_config(parent: HWND, cfg: &AppConfig) {
    set_check(parent, ID_CB_STARTUP,     cfg.start_on_startup);
    set_check(parent, ID_CB_ONTOP,       cfg.dashboard_on_top);
    set_check(parent, ID_CB_BACKGROUND,  cfg.background_collection);
    set_check(parent, ID_CB_AUTOREFRESH, cfg.auto_refresh_token);

    let combo = GetDlgItem(parent, ID_COMBO_MODE as i32);
    let idx = if cfg.display_mode == "overlay" { 1 } else { 0 };
    SendMessageW(combo, CB_SETCURSEL, idx as WPARAM, 0);

    let ud = GetDlgItem(parent, ID_UD_POLLSEC as i32);
    SendMessageW(ud, UDM_SETPOS32, 0, cfg.poll_interval_sec as LPARAM);

    set_edit_text(parent, ID_EDIT_OVERLAYFMT, &cfg.overlay_format);

    BG_COLOR   = hex_to_colorref(&cfg.bg_color).unwrap_or(0x002e_1e1e);
    TEXT_COLOR = hex_to_colorref(&cfg.color_text).unwrap_or(0x00ff_ffff);
    SetWindowTextW(HWND_BG_BTN,   color_hex(BG_COLOR).as_ptr());
    SetWindowTextW(HWND_TEXT_BTN, color_hex(TEXT_COLOR).as_ptr());
}

unsafe fn collect_into_config(parent: HWND, cfg: &mut AppConfig) {
    cfg.start_on_startup       = get_check(parent, ID_CB_STARTUP);
    cfg.dashboard_on_top       = get_check(parent, ID_CB_ONTOP);
    cfg.background_collection  = get_check(parent, ID_CB_BACKGROUND);
    cfg.auto_refresh_token     = get_check(parent, ID_CB_AUTOREFRESH);

    let combo = GetDlgItem(parent, ID_COMBO_MODE as i32);
    let idx = SendMessageW(combo, CB_GETCURSEL, 0, 0) as i32;
    cfg.display_mode = if idx == 1 { "overlay".into() } else { "tray".into() };

    let ud = GetDlgItem(parent, ID_UD_POLLSEC as i32);
    let pos = SendMessageW(ud, UDM_GETPOS32, 0, 0) as i32;
    cfg.poll_interval_sec = pos.clamp(1, 1800);

    cfg.overlay_format = get_edit_text(parent, ID_EDIT_OVERLAYFMT)
        .unwrap_or_else(|| cfg.overlay_format.clone());

    cfg.bg_color   = colorref_to_hex(BG_COLOR);
    cfg.color_text = colorref_to_hex(TEXT_COLOR);
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
    let mut cfg = config_store::load();
    collect_into_config(hwnd, &mut cfg);
    if let Err(e) = config_store::save(&cfg) {
        let msg = wstr(&format!("Failed to save settings:\n{e}"));
        MessageBoxW(hwnd, msg.as_ptr(), w!("Settings"), MB_OK | MB_ICONERROR);
    }
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
        ID_BTN_BG_COLOR => {
            if pick_color(hwnd, &mut BG_COLOR) {
                SetWindowTextW(HWND_BG_BTN, color_hex(BG_COLOR).as_ptr());
            }
        }
        ID_BTN_TEXT_COLOR => {
            if pick_color(hwnd, &mut TEXT_COLOR) {
                SetWindowTextW(HWND_TEXT_BTN, color_hex(TEXT_COLOR).as_ptr());
            }
        }
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
            WM_DESTROY => { HWND_SETTINGS = null_mut(); 0 }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
