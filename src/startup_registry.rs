// Per-user "launch on Windows startup" toggle. Writes our exe path under
// HKCU\Software\Microsoft\Windows\CurrentVersion\Run. No admin prompt;
// also visible in Task Manager → Startup.
//
// Port of src/Shared/StartupRegistry.cs.

use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::*;

use crate::common::wstr;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const APP_NAME: &str = "ClaudeUsageSystray";

/// `Some(path)` to enable (writes `"<path>"` to the Run key), `None` to disable
/// (deletes the value). Best-effort; failures are logged to stderr but don't
/// crash the app.
pub fn set_enabled(exe_path: Option<&str>) {
    unsafe {
        let key_path = wstr(RUN_KEY);
        let mut h: HKEY = null_mut();
        // Try open writable; if missing, create.
        let mut r = RegOpenKeyExW(HKEY_CURRENT_USER, key_path.as_ptr(), 0, KEY_WRITE, &mut h);
        if r != ERROR_SUCCESS {
            let mut disp: u32 = 0;
            r = RegCreateKeyExW(
                HKEY_CURRENT_USER, key_path.as_ptr(), 0,
                null_mut(), 0, KEY_WRITE, null_mut(),
                &mut h, &mut disp,
            );
        }
        if r != ERROR_SUCCESS {
            eprintln!("startup registry open/create failed: {r}");
            return;
        }
        let vname = wstr(APP_NAME);
        match exe_path {
            Some(p) => {
                let quoted = format!("\"{p}\"");
                let val = wstr(&quoted);
                let bytes = val.len() * 2;
                let rc = RegSetValueExW(h, vname.as_ptr(), 0, REG_SZ,
                    val.as_ptr() as *const u8, bytes as u32);
                if rc != ERROR_SUCCESS {
                    eprintln!("startup registry write failed: {rc}");
                }
            }
            None => {
                // Best-effort delete; ignore "value not found"
                let _ = RegDeleteValueW(h, vname.as_ptr());
            }
        }
        RegCloseKey(h);
    }
}
