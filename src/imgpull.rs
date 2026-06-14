// Remote → local puller, bound to Alt+Shift+D. The reverse of imgpaste:
//
//   1. In your SSH terminal, select a remote file/dir path and Ctrl+C it
//      (the path lands on the Windows clipboard as plain text).
//   2. Press Alt+Shift+D — this reads that path, `scp -r` pulls it from the
//      configured host into a local temp dir, then loads the local copy onto
//      the clipboard as CF_HDROP (with DropEffect=Copy) and pops a tray
//      "ready" balloon.
//   3. Ctrl+V in File Explorer, on the Desktop, or in any open folder — the
//      real file/folder is pasted there.
//
// Why the middle hotkey is unavoidable: Explorer reads CF_HDROP off the
// clipboard directly on Ctrl+V — we can't hook that paste — and a copied path
// is just text, with nothing marking it as "fetch me". So an explicit trigger
// must do the fetch and convert the clipboard from text → files.
//
// Path resolution: absolute paths (`/...`) fetch exactly. A leading `~/` is
// stripped so the rest resolves against the remote home — which is also where
// scp resolves bare relative paths (the SFTP start dir). The terminal's actual
// CWD isn't visible to us, so relative-to-CWD paths won't resolve; copy an
// absolute path for anything outside home.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::DataExchange::*;
use windows_sys::Win32::System::Memory::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows_sys::Win32::UI::Shell::DROPFILES;

use crate::common::wstr;
use crate::config_store;
use crate::paths;

pub const HOTKEY_ID: i32 = 0x9002;

// CF_* clipboard format codes (see imgpaste.rs for why they're hardcoded).
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP:       u32 = 15;

// DROPEFFECT_COPY — tells Explorer to copy (not move) the staged temp file.
const DROPEFFECT_COPY: u32 = 1;

// Suppresses the console window flash when shelling out to scp.exe.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// Single-flight guard so a held-down hotkey doesn't queue multiple fetches.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Register the configured hotkey against `hwnd` (the hidden host window).
/// Returns true if registered or if imgpull is disabled in config.
pub fn register_hotkey(hwnd: HWND) -> bool {
    let cfg = config_store::load();
    if !cfg.imgpull_enabled {
        return true;
    }
    let ok = unsafe {
        RegisterHotKey(hwnd, HOTKEY_ID, cfg.imgpull_hotkey_mods, cfg.imgpull_hotkey_vk) != 0
    };
    if !ok {
        log("RegisterHotKey failed — another app may own the same chord");
    }
    ok
}

pub fn unregister_hotkey(hwnd: HWND) {
    unsafe { let _ = UnregisterHotKey(hwnd, HOTKEY_ID); }
}

/// Top-level handler for WM_HOTKEY and the tray menu item. Reads the clipboard
/// path on the caller's thread, then hands the scp pull + clipboard write to a
/// worker so the UI stays responsive.
pub fn handle_hotkey() {
    if IN_FLIGHT.swap(true, Ordering::SeqCst) {
        log("hotkey ignored — previous fetch still in flight");
        return;
    }
    let raw = match unsafe { read_clipboard_text() } {
        Some(s) => s,
        None => {
            log("no text on clipboard to interpret as a remote path");
            unsafe {
                crate::tray::notify(
                    "Nothing to fetch",
                    "Copy a remote file path in your terminal, then press Alt+Shift+D.",
                );
            }
            IN_FLIGHT.store(false, Ordering::SeqCst);
            return;
        }
    };
    let path = sanitize_remote_path(&raw);
    if path.is_empty() {
        log("clipboard path empty after sanitize");
        IN_FLIGHT.store(false, Ordering::SeqCst);
        return;
    }

    std::thread::spawn(move || {
        crate::tray::signal_xfer(crate::tray::XFER_PULL_BEGIN);
        match run_pull(&path) {
            Ok(name) => {
                crate::tray::signal_xfer(crate::tray::XFER_END_OK);
                unsafe {
                    crate::tray::notify(
                        "Ready to paste",
                        &format!("{name} — press Ctrl+V in Explorer, the Desktop, or any folder"),
                    );
                }
            }
            Err(e) => {
                log(&format!("pull: {e}"));
                crate::tray::signal_xfer(crate::tray::XFER_END_FAIL);
                unsafe { crate::tray::notify("Fetch failed", &e); }
            }
        }
        IN_FLIGHT.store(false, Ordering::SeqCst);
    });
}

/// scp-pull `remote_path` from the configured host into a fresh temp dir and
/// put the local copy on the clipboard as CF_HDROP. Returns the basename on
/// success (for the notification).
fn run_pull(remote_path: &str) -> Result<String, String> {
    let cfg = config_store::load();
    let host = std::env::var("IMGPASTE_HOST").unwrap_or(cfg.imgpaste_host);
    if host.is_empty() {
        return Err("imgpaste_host empty (set IMGPASTE_HOST or edit app_state.json)".into());
    }

    // Best-effort cleanup of leftover staging dirs from earlier pulls.
    sweep_stale_temp();

    // scp/SFTP doesn't expand `~`; strip a leading `~/` so the remainder
    // resolves against the remote home (the SFTP default), matching bare
    // relative paths. Absolute paths pass through untouched.
    let remote = remote_path.strip_prefix("~/").unwrap_or(remote_path);
    let basename = remote_basename(remote);
    if basename.is_empty() {
        return Err("could not derive a name from the path".into());
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let local_dir = std::env::temp_dir().join(format!("imgpull-{ts}"));
    std::fs::create_dir_all(&local_dir).map_err(|e| format!("create temp dir: {e}"))?;

    // `scp -r host:remote localdir` — localdir exists, so scp drops the item
    // inside it as localdir/basename. Remote path is unquoted: SFTP uses it
    // literally (no remote shell), so spaces are fine and quotes would break
    // realpath (see imgpaste.rs for the same lesson).
    let status = Command::new("scp")
        .arg("-q")
        .arg("-r")
        .arg(format!("{host}:{remote}"))
        .arg(&local_dir)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("spawning scp.exe: {e}"))?;
    if !status.success() {
        return Err(format!("scp exit code {:?} (does the path exist on {host}?)", status.code()));
    }

    let staged = local_dir.join(&basename);
    if !staged.exists() {
        return Err(format!("scp reported success but {} is missing", staged.display()));
    }

    unsafe { set_clipboard_files(&[staged])?; }
    Ok(basename)
}

/// Read CF_UNICODETEXT off the clipboard as an owned String. Opens/closes the
/// clipboard itself.
unsafe fn read_clipboard_text() -> Option<String> {
    if OpenClipboard(null_mut()) == 0 {
        return None;
    }
    if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
        CloseClipboard();
        return None;
    }
    let h = GetClipboardData(CF_UNICODETEXT) as HGLOBAL;
    if h.is_null() {
        CloseClipboard();
        return None;
    }
    let p = GlobalLock(h) as *const u16;
    if p.is_null() {
        CloseClipboard();
        return None;
    }
    let mut len = 0usize;
    while *p.add(len) != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
    GlobalUnlock(h);
    CloseClipboard();
    Some(s)
}

/// Trim surrounding whitespace (incl. a trailing newline from the terminal
/// copy) and a single layer of wrapping quotes.
fn sanitize_remote_path(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .to_string()
}

/// Last path component of a remote (forward-slash) path.
fn remote_basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

/// Put local file/folder paths on the clipboard as CF_HDROP + a
/// "Preferred DropEffect" = Copy, so a normal Ctrl+V in Explorer pastes them.
unsafe fn set_clipboard_files(paths: &[PathBuf]) -> Result<(), String> {
    // Build the double-NUL-terminated wide path list.
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.as_os_str().encode_wide());
        list.push(0);
    }
    list.push(0); // final list terminator

    // CF_HDROP global: DROPFILES header followed by the path list.
    let df_size = std::mem::size_of::<DROPFILES>();
    let total = df_size + list.len() * 2;
    let h = GlobalAlloc(GMEM_MOVEABLE, total);
    if h.is_null() {
        return Err("GlobalAlloc CF_HDROP failed".into());
    }
    let base = GlobalLock(h);
    if base.is_null() {
        GlobalFree(h);
        return Err("GlobalLock CF_HDROP failed".into());
    }
    let df = base as *mut DROPFILES;
    (*df).pFiles = df_size as u32; // offset from struct start to the list
    (*df).pt = POINT { x: 0, y: 0 };
    (*df).fNC = 0;
    (*df).fWide = 1; // paths are wide chars
    let list_dst = (base as *mut u8).add(df_size) as *mut u16;
    std::ptr::copy_nonoverlapping(list.as_ptr(), list_dst, list.len());
    GlobalUnlock(h);

    // Preferred DropEffect = Copy (a 4-byte DWORD global).
    let cf_pde = RegisterClipboardFormatW(wstr("Preferred DropEffect").as_ptr());
    let he = GlobalAlloc(GMEM_MOVEABLE, 4);
    if he.is_null() {
        GlobalFree(h);
        return Err("GlobalAlloc DropEffect failed".into());
    }
    let ep = GlobalLock(he);
    if ep.is_null() {
        GlobalFree(h);
        GlobalFree(he);
        return Err("GlobalLock DropEffect failed".into());
    }
    *(ep as *mut u32) = DROPEFFECT_COPY;
    GlobalUnlock(he);

    if OpenClipboard(null_mut()) == 0 {
        GlobalFree(h);
        GlobalFree(he);
        return Err("OpenClipboard failed (set files)".into());
    }
    EmptyClipboard();
    if SetClipboardData(CF_HDROP, h as HANDLE).is_null() {
        CloseClipboard();
        GlobalFree(h);
        GlobalFree(he);
        return Err("SetClipboardData(CF_HDROP) failed".into());
    }
    // Clipboard now owns `h` — do NOT free it. The DropEffect is best-effort:
    // if the format set succeeds the clipboard owns `he`, otherwise free it.
    let pde_set = cf_pde != 0 && !SetClipboardData(cf_pde, he as HANDLE).is_null();
    if !pde_set {
        GlobalFree(he);
    }
    CloseClipboard();
    Ok(())
}

/// Remove `imgpull-*` staging dirs older than ~6h. Recent ones are left alone
/// because the clipboard's CF_HDROP may still point into them.
fn sweep_stale_temp() {
    let tmp = std::env::temp_dir();
    let now = SystemTime::now();
    let Ok(entries) = std::fs::read_dir(&tmp) else { return };
    for e in entries.flatten() {
        if !e.file_name().to_string_lossy().starts_with("imgpull-") {
            continue;
        }
        let stale = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age.as_secs() > 6 * 3600);
        if stale {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn log(msg: &str) {
    use std::io::Write;
    let path = paths::app_dir().join("imgpull.log");
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true).open(&path)
    {
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}
