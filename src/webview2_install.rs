// WebView2 Runtime detection + first-run install. The dashboard, settings,
// and chart windows all rely on WebView2 (via the `wry` crate); we don't
// want users on Windows 10 to hit a cryptic "Failed to initialize WebView2"
// dialog the first time they open one. So at startup we check the registry
// for an existing install, and if missing, download Microsoft's Evergreen
// Bootstrapper from the official URL and run it.
//
// We download rather than bundle because the bootstrapper itself fetches
// ~80 MB of runtime from Microsoft's CDN — i.e. an internet connection is
// already required for the install. Bundling the 1.7 MB bootstrapper into
// our binary would just embed it permanently on every machine, including
// the (majority) Windows 11 install base that never needs it at all.

use std::path::Path;
use std::ptr::null_mut;
use std::time::Duration;
use windows_sys::w;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::common::wstr;

// WebView2 Runtime's product GUID. The bootstrapper writes a `pv` value
// (version string) under this key in one of three locations depending on
// per-user vs system + 32/64-bit install.
const SYSTEM_KEY:     &str = r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
const SYSTEM_WOW6432: &str = r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
const USER_KEY:       &str = r"Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

// Official forwarder for the Evergreen Bootstrapper. Microsoft redirects this
// to whatever the current version's URL is, so we never go stale.
const BOOTSTRAPPER_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

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
        rc == ERROR_SUCCESS && len > 2 && buf[0] != 0
    }
}

/// Ensure WebView2 is available. If already installed, returns `true`
/// immediately. Otherwise prompts the user, downloads + runs the
/// bootstrapper, and waits for the install to finish.
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

    // Download → write to %TEMP% → run → delete. Microsoft's bootstrapper
    // shows its own progress UI when run without /silent, which is nicer
    // than a 1-2 minute period of no visible feedback.
    let installer_path = std::env::temp_dir().join("ClaudeUsageSystray_WV2.exe");
    if let Err(detail) = download_bootstrapper(&installer_path) {
        show_failure(&format!("Download failed: {detail}"));
        return false;
    }

    let status = std::process::Command::new(&installer_path)
        .args(["/silent", "/install"])
        .status();
    let _ = std::fs::remove_file(&installer_path);

    let ok = matches!(status, Ok(s) if s.success()) && is_installed();
    if !ok { show_failure("the installer exited with an error"); }
    ok
}

fn download_bootstrapper(dest: &Path) -> Result<(), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .build();
    let resp = agent.get(BOOTSTRAPPER_URL).call()
        .map_err(|e| format!("HTTP error: {e}"))?;
    let mut out = std::fs::File::create(dest)
        .map_err(|e| format!("create temp file: {e}"))?;
    let mut reader = resp.into_reader();
    std::io::copy(&mut reader, &mut out)
        .map_err(|e| format!("write: {e}"))?;
    Ok(())
}

fn show_failure(detail: &str) {
    let msg = wstr(&format!(
        "WebView2 install failed ({detail}).\n\n\
         Dashboard, settings, and chart windows will not open.\n\n\
         You can install manually from:\n\
         https://developer.microsoft.com/microsoft-edge/webview2/"
    ));
    unsafe {
        MessageBoxW(
            null_mut(),
            msg.as_ptr(),
            w!("Claude Usage Systray"),
            MB_OK | MB_ICONWARNING,
        );
    }
}
