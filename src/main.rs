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

mod chart;
mod common;
mod config_store;
mod cooldown;
mod credentials;
mod health;
mod dashboard;
mod imgpaste;
mod imgpull;
mod models;
mod oauth_refresh;
mod overlay;
mod paths;
mod poll_service;
mod settings;
mod startup_registry;
mod state_store;
mod tray;
mod tray_registry_patch;
mod update_service;
mod usage_cache;
mod usage_fetcher;
mod usage_history;
mod webview2_install;
mod webview_host;

use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::GdiPlus::{GdiplusStartup, GdiplusStartupInput};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Single-instance guard via a named mutex. Returns false if another instance
/// is already running (after a grace period) — the caller should exit.
///
/// The grace period matters: a self-update relaunch (see update_service.rs)
/// intentionally overlaps old and new processes for a moment — the new
/// instance must WAIT for the old one to exit (releasing the mutex at process
/// teardown) rather than bail immediately. ~10 s covers the slowest teardown;
/// a genuine second instance still shows the message box after it.
unsafe fn acquire_single_instance() -> bool {
    use windows_sys::Win32::System::Threading::CreateMutexW;
    for _ in 0..20 {
        let h = CreateMutexW(std::ptr::null(), 1, w!("ClaudeUsageSystray-SingleInstance"));
        if h.is_null() {
            return true; // can't create a mutex at all — don't block startup over it
        }
        if GetLastError() != ERROR_ALREADY_EXISTS {
            // We own it. Hold the handle for the process lifetime (never closed;
            // the OS releases it at exit, which is exactly the semantics we want).
            return true;
        }
        CloseHandle(h);
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    false
}

fn main() {
    unsafe {
        // Refuse to run twice: two instances mean duplicate tray icons, failed
        // hotkey registrations, and two pollers fighting over the state file.
        if !acquire_single_instance() {
            MessageBoxW(
                null_mut(),
                w!("Claude Usage Systray is already running.\n\nLook for its icon in the system tray."),
                w!("Claude Usage Systray"),
                MB_OK | MB_ICONINFORMATION,
            );
            return;
        }

        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC:  ICC_STANDARD_CLASSES | ICC_UPDOWN_CLASS,
        };
        InitCommonControlsEx(&icc);

        // Initialize GDI+. We use it for overlay text rendering so AA edges
        // get proper alpha (raw GDI on a layered DIB can't write the alpha
        // channel — looks weak/chunky compared to GDI+).
        let mut gdip_token: usize = 0;
        let gdip_input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: 0,
            SuppressExternalCodecs: 0,
        };
        GdiplusStartup(&mut gdip_token, &gdip_input, std::ptr::null_mut());

        // Make sure WebView2 Runtime is available before the tray icon
        // surfaces — the dashboard/settings/chart windows can't open without
        // it. On Windows 11 this returns immediately (WebView2 is built in);
        // on Windows 10 it pops a one-time install dialog.
        let _ = webview2_install::ensure_installed();

        // First-run check: if the Claude CLI hasn't been used on this machine,
        // ~/.claude/.credentials.json won't exist and every reading will be
        // 0%. A silent 0% is confusing; surface the actual cause once.
        if credentials::read_full().is_none() {
            MessageBoxW(
                null_mut(),
                w!("Welcome! This widget shows your Claude usage, but it needs the Claude CLI's authentication first.\n\nPlease run \"claude login\" in a terminal, then restart this app.\n\nThe widget will continue running with 0% values until you do."),
                w!("Claude Usage Systray — Setup needed"),
                MB_OK | MB_ICONINFORMATION,
            );
        }

        common::init_resources();

        // Clean up a leftover exe from a prior self-update, on a background
        // thread so a briefly-locked `.old` file can't delay the tray icon.
        std::thread::spawn(update_service::cleanup_old_exe);

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

        // Register the imgpaste + imgpull global hotkeys against this same host
        // window. Failures are non-fatal — they get logged to imgpaste.log /
        // imgpull.log.
        imgpaste::register_hotkey(host);
        imgpull::register_hotkey(host);

        // Schedule the registry self-patch ~1.5 s after the icon registers,
        // matching the C# port. Windows writes the partial NotifyIconSettings
        // entry lazily; the timer gives it time to land before we patch.
        SetTimer(host, TIMER_PATCH, 1500, None);

        // Kick off the background polling thread. It posts WM_USAGE_UPDATED
        // back to the host window on every successful fetch.
        poll_service::start(host);

        // Start the in-app self-updater. It checks GitHub CI (~20 s after
        // launch, then every 30 min) for a newer build of this architecture and
        // posts WM_UPDATE_RELAUNCH when one is downloaded and staged.
        update_service::start(host);

        // Restore overlay + dashboard if they were open last session.
        overlay::open_if_persisted();
        dashboard::open_if_persisted();

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
            chart::on_data_changed();
            return 0;
        }
        if msg == tray::WM_XFER_STATE {
            tray::on_xfer_message(hwnd, wp);
            return 0;
        }
        if msg == update_service::WM_UPDATE_RELAUNCH {
            // A newer build is staged. install_pending() swaps the exe and
            // spawns the new instance; only if it actually launched do we
            // release our global hotkeys + tray icon (so the new process can
            // claim them) and quit. On failure we keep running unchanged.
            if update_service::install_pending() {
                imgpaste::unregister_hotkey(hwnd);
                imgpull::unregister_hotkey(hwnd);
                tray::remove();
                PostQuitMessage(0);
            }
            return 0;
        }
        match msg {
            WM_HOTKEY => {
                let id = wp as i32;
                if id == imgpaste::HOTKEY_ID {
                    imgpaste::handle_hotkey();
                } else if id == imgpull::HOTKEY_ID {
                    imgpull::handle_hotkey();
                }
                0
            }
            WM_TIMER => {
                if wp == TIMER_PATCH {
                    KillTimer(hwnd, TIMER_PATCH);
                    if let Ok(exe) = std::env::current_exe() {
                        tray_registry_patch::apply(
                            &exe.to_string_lossy(),
                            "Usage",
                        );
                    }
                } else if wp == tray::TIMER_XFER {
                    tray::xfer_tick(hwnd);
                }
                0
            }
            WM_DESTROY => {
                imgpaste::unregister_hotkey(hwnd);
                imgpull::unregister_hotkey(hwnd);
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
