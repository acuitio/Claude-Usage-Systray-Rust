// Pure Win32 + Rust prototype of the full tray-widget app surface.
//
// Goal: prove we can drop .NET entirely while keeping memory safety + clean
// dep management. Mirrors src/App/ — tray, overlay, dashboard, settings.
//
// Wiring:
//   main.rs    — hidden message-only window hosts the tray icon callback;
//                spawns the other windows on demand from menu items.
//   tray.rs    — Shell_NotifyIcon + bar-chart icon rendering + popup menu
//   overlay.rs — WS_EX_LAYERED window with per-pixel-alpha rendering
//   dashboard.rs — owner-drawn window with three usage bars
//   settings.rs  — dialog with controls (BUTTON / COMBOBOX / EDIT / UPDOWN /
//                  ChooseColor)
//   common.rs    — palette, fonts, helpers
//
// Real port would add: HTTP via reqwest, JSON via serde_json, atomic config
// writes via tempfile crate, registry via windows-sys's RegSetValueExW.
// Those don't change the UI calculus, so they're out of scope here.

#![windows_subsystem = "windows"]

mod common;
mod config_store;
mod cooldown;
mod credentials;
mod dashboard;
mod models;
mod oauth_refresh;
mod overlay;
mod paths;
mod poll_service;
mod settings;
mod tray;
mod tray_registry_patch;
mod usage_cache;
mod usage_fetcher;
mod usage_history;

use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn main() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC:  ICC_STANDARD_CLASSES | ICC_UPDOWN_CLASS,
        };
        InitCommonControlsEx(&icc);

        common::init_resources();

        // Hidden message-only window: hosts the tray icon's callback.
        let class_name = w!("Win32AppProtoHost");
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSEXW {
            cbSize:        std::mem::size_of::<WNDCLASSEXW>() as u32,
            style:         0,
            lpfnWndProc:   Some(host_proc),
            cbClsExtra:    0, cbWndExtra: 0,
            hInstance:     instance,
            hIcon:         null_mut(),
            hCursor:       null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName:  std::ptr::null(),
            lpszClassName: class_name,
            hIconSm:       null_mut(),
        };
        RegisterClassExW(&wc);

        let host = CreateWindowExW(
            0, class_name, w!("Win32AppProto-host"),
            0, 0, 0, 0, 0,
            HWND_MESSAGE, null_mut(), instance, std::ptr::null(),
        );

        tray::install(host);

        // Schedule the registry self-patch ~1.5 s after the icon registers,
        // matching the C# port. Windows writes the partial NotifyIconSettings
        // entry lazily; the timer gives it time to land before we patch.
        SetTimer(host, TIMER_PATCH, 1500, None);

        // Kick off the background polling thread. It posts WM_USAGE_UPDATED
        // back to the host window on every successful fetch.
        poll_service::start(host);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        tray::remove();
    }
}

const TIMER_PATCH: usize = 1;

extern "system" fn host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == tray::WM_TRAY_CALLBACK {
            tray::handle_tray_callback(hwnd, lp);
            return 0;
        }
        if msg == poll_service::WM_USAGE_UPDATED {
            tray::refresh();
            overlay::on_data_changed();
            dashboard::on_data_changed();
            return 0;
        }
        match msg {
            WM_TIMER => {
                if wp == TIMER_PATCH {
                    KillTimer(hwnd, TIMER_PATCH);
                    if let Ok(exe) = std::env::current_exe() {
                        tray_registry_patch::apply(
                            &exe.to_string_lossy(),
                            "Usage",
                        );
                    }
                }
                0
            }
            WM_DESTROY => { PostQuitMessage(0); 0 }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
