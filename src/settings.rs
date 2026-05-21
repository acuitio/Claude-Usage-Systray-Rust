// Settings dialog — refactored from the original prototype.

use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::Dialogs::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::*;

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
    let class_name = w!("Win32SettingsProtoRustClass");
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

    HWND_SETTINGS = CreateWindowExW(
        0, class_name, w!("Settings"),
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT, CW_USEDEFAULT, 600, 760,
        null_mut(), null_mut(), instance, std::ptr::null(),
    );
    ShowWindow(HWND_SETTINGS, SW_SHOWNORMAL);
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
    for (label, id, init) in [
        (w!("Launch widget on Windows startup"),                       ID_CB_STARTUP,     false),
        (w!("Dashboard always on top"),                                ID_CB_ONTOP,       true),
        (w!("Collect usage data in the background (every 10 min)"),    ID_CB_BACKGROUND,  false),
        (w!("Auto-refresh OAuth token (Path 1)"),                      ID_CB_AUTOREFRESH, true),
    ] {
        let cb = create_child(parent, w!("BUTTON"), label,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            x, y, w, 22, id);
        set_font(cb, FONT_REG);
        SendMessageW(cb, BM_SETCHECK,
            if init { BST_CHECKED } else { BST_UNCHECKED } as WPARAM, 0);
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
    SendMessageW(combo, CB_SETCURSEL, 0, 0);
    set_font(combo, FONT_REG);
    y += 36;
    sep!();

    header!("Refresh interval (seconds, 1-1800)");
    let edit = create_child(parent, w!("EDIT"), w!("300"),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER
            | ES_NUMBER as u32 | ES_LEFT as u32,
        x, y, 80, 26, ID_EDIT_POLLSEC);
    set_font(edit, FONT_REG);
    let ud = create_child(parent, UPDOWN_CLASSW, std::ptr::null(),
        WS_CHILD | WS_VISIBLE | UDS_SETBUDDYINT | UDS_ALIGNRIGHT | UDS_ARROWKEYS,
        0, 0, 0, 0, ID_UD_POLLSEC);
    SendMessageW(ud, UDM_SETBUDDY, edit as WPARAM, 0);
    SendMessageW(ud, UDM_SETRANGE32, 1, 1800);
    SendMessageW(ud, UDM_SETPOS32, 0, 300);
    y += 36;
    sep!();

    header!("Overlay format");
    let fmt = create_child(parent, w!("EDIT"), w!("{session}  |  {weekly}  |  {sonnet}"),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER
            | ES_AUTOHSCROLL as u32 | ES_LEFT as u32,
        x, y, w, 26, ID_EDIT_OVERLAYFMT);
    set_font(fmt, FONT_REG);
    y += 36;
    sep!();

    header!("Colors");
    for (label, id, color_ptr, hwnd_slot) in [
        (w!("Background"), ID_BTN_BG_COLOR,   &raw mut BG_COLOR,   &raw mut HWND_BG_BTN),
        (w!("Text"),       ID_BTN_TEXT_COLOR, &raw mut TEXT_COLOR, &raw mut HWND_TEXT_BTN),
    ] {
        let lbl = create_child(parent, w!("STATIC"), label,
            WS_CHILD | WS_VISIBLE, x, y + 6, 150, 22, 0);
        set_font(lbl, FONT_REG);
        let hex = color_hex(*color_ptr);
        let btn = create_child(parent, w!("BUTTON"), hex.as_ptr(),
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

unsafe fn handle_command(hwnd: HWND, wp: WPARAM) {
    let id = (wp & 0xffff) as u16;
    match id {
        ID_BTN_APPLY => {
            MessageBoxW(hwnd,
                w!("Apply pressed.\n(Prototype — would persist config.json)"),
                w!("Settings"), MB_OK);
        }
        ID_BTN_SAVEEXIT => {
            PostMessageW(hwnd, WM_CLOSE, 0, 0);
        }
        ID_BTN_RESET => {
            if MessageBoxW(hwnd, w!("Reset all settings to defaults?"),
                           w!("Reset"), MB_YESNO | MB_ICONINFORMATION) == IDYES {
                SendMessageW(GetDlgItem(hwnd, ID_CB_STARTUP as i32),
                    BM_SETCHECK, BST_UNCHECKED as WPARAM, 0);
                SendMessageW(GetDlgItem(hwnd, ID_CB_ONTOP as i32),
                    BM_SETCHECK, BST_CHECKED as WPARAM, 0);
            }
        }
        ID_BTN_CANCEL => { PostMessageW(hwnd, WM_CLOSE, 0, 0); }
        ID_BTN_BG_COLOR => {
            if pick_color(hwnd, &mut BG_COLOR) {
                let hex = color_hex(BG_COLOR);
                SetWindowTextW(HWND_BG_BTN, hex.as_ptr());
            }
        }
        ID_BTN_TEXT_COLOR => {
            if pick_color(hwnd, &mut TEXT_COLOR) {
                let hex = color_hex(TEXT_COLOR);
                SetWindowTextW(HWND_TEXT_BTN, hex.as_ptr());
            }
        }
        _ => {}
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => { build_controls(hwnd); 0 }
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
