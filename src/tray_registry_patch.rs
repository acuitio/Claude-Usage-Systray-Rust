// Self-heal the Windows 11 NotifyIconSettings registry entry so our tray
// icon appears properly in Settings → Personalization → Taskbar → Other
// system tray icons. Direct port of src/App/TrayRegistryPatch.cs.
//
// Without NIF_GUID, Shell_NotifyIcon only writes IconSnapshot to the
// registry; the Settings page filters entries with no ExecutablePath. We
// scan HKCU\Control Panel\NotifyIconSettings for either (a) an existing
// entry pointing at our exe or (b) a fresh empty entry just written by
// NIM_ADD, and patch in the missing fields.
//
// Called on a 1.5 s timer after the tray icon registers. Idempotent.

use std::ffi::c_void;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::*;

use crate::common::wstr;

const SUBKEY: &str = r"Control Panel\NotifyIconSettings";

pub fn apply(exe_path: &str, friendly_name: &str) {
    unsafe { apply_inner(exe_path, friendly_name) };
}

unsafe fn apply_inner(exe_path: &str, friendly_name: &str) {
    let root_path = wstr(SUBKEY);
    let mut root: HKEY = null_mut();
    if RegOpenKeyExW(HKEY_CURRENT_USER, root_path.as_ptr(), 0,
                     KEY_READ | KEY_WRITE, &mut root) != ERROR_SUCCESS {
        return;
    }

    let mut our_match: Option<Vec<u16>> = None;
    let mut partial: Option<Vec<u16>> = None;

    // Enumerate subkeys
    let mut idx: u32 = 0;
    let mut name_buf: [u16; 256];
    loop {
        name_buf = [0; 256];
        let mut name_len: u32 = name_buf.len() as u32;
        let r = RegEnumKeyExW(root, idx, name_buf.as_mut_ptr(), &mut name_len,
                              null_mut(), null_mut(), null_mut(), null_mut());
        if r != ERROR_SUCCESS { break; }
        idx += 1;

        let name = &name_buf[..name_len as usize];
        let name_null: Vec<u16> = name.iter().copied().chain(std::iter::once(0)).collect();

        let entry_exe = read_string(root, &name_null, "ExecutablePath");
        if entry_exe.as_deref().map(|s| s.eq_ignore_ascii_case(exe_path)).unwrap_or(false) {
            our_match = Some(name_null);
            continue;
        }
        if (entry_exe.is_none() || entry_exe.as_deref() == Some(""))
            && has_binary(root, &name_null, "IconSnapshot")
            && partial.is_none()
        {
            partial = Some(name_null);
        }
    }

    if let Some(name) = our_match {
        // Pass 1: existing entry — always overwrite InitialTooltip so the
        // friendly name can evolve across versions.
        if let Some(sub) = open_subkey_write(root, &name) {
            write_string(sub, "InitialTooltip", friendly_name);
            if read_dword(sub, "UID").is_none() {
                write_dword(sub, "UID", 1);
            }
            RegCloseKey(sub);
        }
    } else if let Some(name) = partial {
        // Pass 2: empty entry just created by NIM_ADD — fill it in.
        if let Some(sub) = open_subkey_write(root, &name) {
            write_string(sub, "ExecutablePath", exe_path);
            write_string(sub, "InitialTooltip", friendly_name);
            write_dword(sub, "UID", 1);
            RegCloseKey(sub);
        }
    }

    RegCloseKey(root);
}

unsafe fn open_subkey_write(root: HKEY, name_null: &[u16]) -> Option<HKEY> {
    let mut sub: HKEY = null_mut();
    let r = RegOpenKeyExW(root, name_null.as_ptr(), 0, KEY_WRITE, &mut sub);
    if r == ERROR_SUCCESS { Some(sub) } else { None }
}

unsafe fn read_string(root: HKEY, name_null: &[u16], value: &str) -> Option<String> {
    let mut sub: HKEY = null_mut();
    if RegOpenKeyExW(root, name_null.as_ptr(), 0, KEY_READ, &mut sub) != ERROR_SUCCESS {
        return None;
    }
    let vname = wstr(value);
    let mut ty: u32 = 0;
    let mut data_len: u32 = 0;
    let r = RegQueryValueExW(sub, vname.as_ptr(), null_mut(), &mut ty, null_mut(), &mut data_len);
    if r != ERROR_SUCCESS || (ty != REG_SZ && ty != REG_EXPAND_SZ) {
        RegCloseKey(sub);
        return None;
    }
    let mut buf: Vec<u16> = vec![0; (data_len as usize) / 2 + 1];
    let mut len2 = data_len;
    let r = RegQueryValueExW(sub, vname.as_ptr(), null_mut(), &mut ty,
                             buf.as_mut_ptr() as *mut u8, &mut len2);
    RegCloseKey(sub);
    if r != ERROR_SUCCESS { return None; }
    let chars = (len2 as usize) / 2;
    let trimmed: Vec<u16> = buf[..chars].iter().copied().take_while(|&c| c != 0).collect();
    Some(String::from_utf16_lossy(&trimmed))
}

unsafe fn has_binary(root: HKEY, name_null: &[u16], value: &str) -> bool {
    let mut sub: HKEY = null_mut();
    if RegOpenKeyExW(root, name_null.as_ptr(), 0, KEY_READ, &mut sub) != ERROR_SUCCESS {
        return false;
    }
    let vname = wstr(value);
    let mut ty: u32 = 0;
    let mut data_len: u32 = 0;
    let r = RegQueryValueExW(sub, vname.as_ptr(), null_mut(), &mut ty, null_mut(), &mut data_len);
    RegCloseKey(sub);
    r == ERROR_SUCCESS && ty == REG_BINARY && data_len > 0
}

unsafe fn read_dword(sub: HKEY, value: &str) -> Option<u32> {
    let vname = wstr(value);
    let mut ty: u32 = 0;
    let mut data: u32 = 0;
    let mut data_len: u32 = std::mem::size_of::<u32>() as u32;
    let r = RegQueryValueExW(sub, vname.as_ptr(), null_mut(), &mut ty,
                             &mut data as *mut u32 as *mut u8, &mut data_len);
    if r == ERROR_SUCCESS && ty == REG_DWORD { Some(data) } else { None }
}

unsafe fn write_string(sub: HKEY, value: &str, text: &str) {
    let vname = wstr(value);
    let val = wstr(text);
    let bytes = val.len() * 2; // includes the null terminator
    RegSetValueExW(sub, vname.as_ptr(), 0, REG_SZ,
                   val.as_ptr() as *const u8, bytes as u32);
}

unsafe fn write_dword(sub: HKEY, value: &str, data: u32) {
    let vname = wstr(value);
    RegSetValueExW(sub, vname.as_ptr(), 0, REG_DWORD,
                   &data as *const u32 as *const u8,
                   std::mem::size_of::<u32>() as u32);
}

// Silence the unused-import warning if windows-sys ever changes its
// re-exports — c_void is referenced via raw pointer casts only.
#[allow(dead_code)]
fn _keep_c_void_in_scope() -> *mut c_void { std::ptr::null_mut() }
