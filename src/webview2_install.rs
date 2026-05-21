// WebView2 Runtime detection + first-run install. The dashboard, settings,
// and chart windows all rely on WebView2 (via the `wry` crate); we don't
// want users on Windows 10 to hit a cryptic "Failed to initialize WebView2"
// dialog the first time they open one. So at startup we check the registry
// for an existing install, and if missing, extract Microsoft's bundled
// Evergreen Bootstrapper to %TEMP% and run it.

use std::ptr::null_mut;
use windows_sys::w;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::wstr;

// WebView2 Runtime's product GUID. The bootstrapper writes a `pv` value
// (version string) under this key. Three possible homes depending on
// per-user vs system install + 32/64-bit:
const SYSTEM_KEY:    &str = r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
const SYSTEM_WOW6432: &str = r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
const USER_KEY:      &str = r"Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

// The Evergreen Bootstrapper itself, ~1.7 MB. Embedded in the binary so a
// fresh-installed app needs no network round-trip before running.
const BOOTSTRAPPER: &[u8] = include_bytes!("../assets/MicrosoftEdgeWebview2Setup.exe");

pub fn is_installed() -> bool {
    has_pv(HKEY_LOCAL_MACHINE, SYSTEM_KEY)
        || has_pv(HKEY_LOCAL_MACHINE, SYSTEM_WOW6432)
        || has_pv(HKEY_CURRENT_USER, USER_KEY)
}

fn has_pv(root: HKEY, sub: &str) -> bool {
    unsafe {
        let path = wstr(sub);
        let mut h: HKEY = null_mut();
        if RegOpenKeyExW(root, path.as_ptr(), 0, KEY_READ, &mut h) != ERROR_SUCCESS {
            return false;
        }
        let value = wstr("pv");
        let mut ty: u32 = 0;
        let mut buf = [0u16; 64];
        let mut len: u32 = (buf.len() * 2) as u32;
        let rc = RegQueryValueExW(
            h, value.as_ptr(), null_mut(), &mut ty,
            buf.as_mut_ptr() as *mut u8, &mut len,
        );
        RegCloseKey(h);
        // pv is a REG_SZ; len > 2 means at least one real wchar (plus the
        // wide-string null terminator). buf[0] != 0 catches the corner case
        // where the value exists but is empty.
        rc == ERROR_SUCCESS && len > 2 && buf[0] != 0
    }
}

/// Ensure the WebView2 Runtime is available. If already installed, returns
/// `true` immediately. Otherwise extracts the bundled bootstrapper to TEMP
/// and runs it, blocking until the install completes. Returns `false` if
/// the user cancels or the install fails — caller should warn the user
/// that dashboard/settings/chart features won't work.
pub fn ensure_installed() -> bool {
    if is_installed() { return true; }

    let proceed = unsafe {
        MessageBoxW(
            null_mut(),
            w!("Claude Usage Systray needs the Microsoft Edge WebView2 Runtime to show its dashboard, settings, and chart windows.\n\nClick OK to install it now (one-time, ~1 minute). The tray icon and overlay will continue to work even if you skip this."),
            w!("Claude Usage Systray — First-run install"),
            MB_OKCANCEL | MB_ICONINFORMATION,
        )
    };
    if proceed != IDOK { return false; }

    let installer_path = std::env::temp_dir().join("ClaudeUsageSystray_WV2.exe");
    if std::fs::write(&installer_path, BOOTSTRAPPER).is_err() { return false; }

    // `/silent /install` runs the bootstrapper without UI and waits until
    // the runtime finishes installing.
    let status = std::process::Command::new(&installer_path)
        .args(["/silent", "/install"])
        .status();
    let _ = std::fs::remove_file(&installer_path);

    let ok = matches!(status, Ok(s) if s.success()) && is_installed();
    if !ok {
        unsafe {
            MessageBoxW(
                null_mut(),
                w!("WebView2 install failed. Dashboard, settings, and chart windows will not open.\n\nYou can install manually from:\nhttps://developer.microsoft.com/microsoft-edge/webview2/"),
                w!("Claude Usage Systray"),
                MB_OK | MB_ICONWARNING,
            );
        }
    }
    ok
}
