// Clipboard → remote SSH uploader, bound to Alt+Shift+V. Handles two kinds
// of clipboard payload and pastes the resulting remote path(s) into the
// focused window (a terminal running Claude Code over SSH, typically):
//
//   • An image (screenshot / paint app / "copy image") — rendered to PNG and
//     scp'd as `<remote_dir>/imgpaste-<unix-ts>.png`.
//   • Files/folders copied in Explorer with Ctrl+C (CF_HDROP) — scp'd
//     (folders recursively) into a per-paste folder
//     `<remote_dir>/paste-<unix-ts>/`, with the original names preserved.
//
// The path travels through your existing SSH session as synthesized Ctrl+V
// keystrokes; the bytes travel via a separate transient scp session — they
// converge at Claude Code when it opens the path. The original clipboard
// contents (image or file list) are restored after the paste.
//
// Threading: the hotkey handler does a brief clipboard read on the UI
// thread (we already own the message loop), then hands SCP + paste to a
// worker so the UI stays responsive.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::HBITMAP;
// GdiPlus glob can't be used here — it pulls in `Status::Ok` as `const i32 = 0`,
// which shadows `Result::Ok` and breaks every `Ok(...)`/`Err(...)` literal.
use windows_sys::Win32::Graphics::GdiPlus::{
    GdipCreateBitmapFromHBITMAP, GdipDisposeImage, GdipSaveImageToFile, GpBitmap, GpImage,
};
use windows_sys::Win32::System::DataExchange::*;
use windows_sys::Win32::System::Memory::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::{DragQueryFileW, HDROP};

use crate::common::wstr;
use crate::config_store;
use crate::paths;

pub const HOTKEY_ID: i32 = 0x9001;

// CF_* clipboard format codes. windows-sys puts these under Win32_System_Ole,
// which would drag the whole OLE feature set into the build for four u32s.
// Hardcoded here against the stable Win32 numeric assignments.
const CF_BITMAP:      u32 = 2;
const CF_DIB:         u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP:       u32 = 15;

// Suppresses the cmd window flash when shelling out to scp.exe.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// PNG encoder CLSID for GdipSaveImageToFile.
// {557CF406-1A04-11D3-9A73-0000F81EF32E}
const PNG_ENCODER_CLSID: GUID =
    GUID::from_u128(0x557CF406_1A04_11D3_9A73_0000F81EF32E);

// Single-flight guard so a held-down hotkey doesn't queue multiple uploads.
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Register the configured hotkey against `hwnd` (the hidden host window).
/// Returns true if registered or if imgpaste is disabled in config.
pub fn register_hotkey(hwnd: HWND) -> bool {
    let cfg = config_store::load();
    if !cfg.imgpaste_enabled {
        return true;
    }
    let ok = unsafe {
        RegisterHotKey(hwnd, HOTKEY_ID, cfg.imgpaste_hotkey_mods, cfg.imgpaste_hotkey_vk) != 0
    };
    if !ok {
        log("RegisterHotKey failed — another app may own the same chord");
    }
    ok
}

pub fn unregister_hotkey(hwnd: HWND) {
    unsafe { let _ = UnregisterHotKey(hwnd, HOTKEY_ID); }
}

/// Top-level handler for WM_HOTKEY and the tray menu item. Brief clipboard
/// read on the caller's thread, then SCP + paste on a worker.
pub fn handle_hotkey() {
    if IN_FLIGHT.swap(true, Ordering::SeqCst) {
        log("hotkey ignored — previous upload still in flight");
        return;
    }
    let prepared = match unsafe { prepare_upload() } {
        Ok(p) => p,
        Err(e) => {
            log(&format!("prepare_upload: {e}"));
            IN_FLIGHT.store(false, Ordering::SeqCst);
            return;
        }
    };
    std::thread::spawn(move || {
        if let Err(e) = run_upload_and_paste(prepared) {
            log(&format!("upload+paste: {e}"));
        }
        IN_FLIGHT.store(false, Ordering::SeqCst);
    });
}

/// What `prepare_upload` pulled off the clipboard, ready for the worker to
/// ship. Either a single rendered PNG (an image was on the clipboard) or a
/// set of file/folder paths the user copied in Explorer (CF_HDROP).
enum PreparedPayload {
    /// An image was on the clipboard; we rendered it to this temp PNG.
    /// `saved_dib` is the original CF_DIB bytes, restored after the paste.
    Image { local_png: PathBuf, saved_dib: Option<Vec<u8>> },
    /// One or more files/folders were copied in Explorer. `saved_hdrop` is
    /// the original CF_HDROP bytes, restored after the paste so the user's
    /// copy selection survives.
    Paths { sources: Vec<PathBuf>, saved_hdrop: Option<Vec<u8>> },
}

struct PreparedUpload {
    payload:    PreparedPayload,
    host:       String,
    remote_dir: String,
}

unsafe fn prepare_upload() -> Result<PreparedUpload, String> {
    // Env vars win over config so ad-hoc one-shots from a PowerShell shell
    // don't require rewriting state.
    let cfg = config_store::load();
    let host = std::env::var("IMGPASTE_HOST").unwrap_or(cfg.imgpaste_host);
    let remote_dir = std::env::var("IMGPASTE_REMOTE_DIR").unwrap_or(cfg.imgpaste_remote_dir);
    if host.is_empty() {
        return Err("imgpaste_host empty (set IMGPASTE_HOST or edit app_state.json)".into());
    }

    if OpenClipboard(null_mut()) == 0 {
        return Err("OpenClipboard failed".into());
    }

    // Priority 1: files/folders copied in Explorer surface as CF_HDROP.
    // Prefer this over CF_BITMAP — if someone copied an image *file*, we'd
    // rather ship the original bytes than a re-encoded rasterized preview.
    if IsClipboardFormatAvailable(CF_HDROP) != 0 {
        // Snapshot the raw CF_HDROP block so we can put the user's copy
        // selection back after we hijack the clipboard for the path paste.
        // DROPFILES is offset-based (no embedded pointers), so a flat byte
        // copy round-trips into a valid handle.
        let saved_hdrop = snapshot_clipboard_format(CF_HDROP);
        let sources = read_hdrop_paths();
        CloseClipboard();
        if sources.is_empty() {
            return Err("clipboard advertised CF_HDROP but no paths could be read".into());
        }
        return Ok(PreparedUpload {
            payload: PreparedPayload::Paths { sources, saved_hdrop },
            host,
            remote_dir,
        });
    }

    // Priority 2: a raw image (screenshot, paint app, browser "copy image").
    // Snapshot CF_DIB up-front so we can restore the user's clipboard image
    // after the paste. Best-effort: if the clipboard doesn't carry CF_DIB
    // we'll just lose the restore (rare — paint apps put CF_DIB universally).
    let saved_dib = snapshot_clipboard_format(CF_DIB);

    if IsClipboardFormatAvailable(CF_BITMAP) == 0 {
        CloseClipboard();
        return Err("no image or files on clipboard".into());
    }
    let hbm = GetClipboardData(CF_BITMAP) as HBITMAP;
    if hbm.is_null() {
        CloseClipboard();
        return Err("GetClipboardData(CF_BITMAP) returned NULL".into());
    }

    let ts_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let local_png = std::env::temp_dir().join(format!("imgpaste-{ts_ns}.png"));

    let save_result = save_hbitmap_as_png(hbm, &local_png);
    CloseClipboard();
    save_result?;

    Ok(PreparedUpload {
        payload: PreparedPayload::Image { local_png, saved_dib },
        host,
        remote_dir,
    })
}

/// Enumerate the file/folder paths inside the clipboard's CF_HDROP handle.
/// Must be called with the clipboard already open. `DragQueryFileW` reads the
/// DROPFILES block directly, so no GlobalLock dance is needed here.
unsafe fn read_hdrop_paths() -> Vec<PathBuf> {
    let hdrop = GetClipboardData(CF_HDROP) as HDROP;
    if hdrop.is_null() {
        return Vec::new();
    }
    // iFile = 0xFFFFFFFF asks for the file count instead of a path.
    let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, null_mut(), 0);
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        // First call (null buffer) returns the length in chars, sans NUL.
        let len = DragQueryFileW(hdrop, i, null_mut(), 0);
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let got = DragQueryFileW(hdrop, i, buf.as_mut_ptr(), len + 1);
        if got == 0 {
            continue;
        }
        out.push(PathBuf::from(String::from_utf16_lossy(&buf[..got as usize])));
    }
    out
}

/// Snapshot the raw bytes of a clipboard format into an owned Vec, so we can
/// re-publish it after temporarily hijacking the clipboard for the path
/// paste. Must be called with the clipboard already open. Works for any
/// HGLOBAL-backed format whose payload is self-contained (CF_DIB, CF_HDROP).
unsafe fn snapshot_clipboard_format(fmt: u32) -> Option<Vec<u8>> {
    if IsClipboardFormatAvailable(fmt) == 0 { return None; }
    let h = GetClipboardData(fmt) as HGLOBAL;
    if h.is_null() { return None; }
    let size = GlobalSize(h);
    if size == 0 { return None; }
    let p = GlobalLock(h);
    if p.is_null() { return None; }
    let bytes = std::slice::from_raw_parts(p as *const u8, size).to_vec();
    GlobalUnlock(h);
    Some(bytes)
}

unsafe fn save_hbitmap_as_png(hbm: HBITMAP, dest: &Path) -> Result<(), String> {
    let mut bitmap: *mut GpBitmap = null_mut();
    let st = GdipCreateBitmapFromHBITMAP(hbm, null_mut(), &mut bitmap);
    if st != 0 || bitmap.is_null() {
        return Err(format!("GdipCreateBitmapFromHBITMAP status {st}"));
    }
    let wpath = wstr(&dest.to_string_lossy());
    let st = GdipSaveImageToFile(
        bitmap as *mut GpImage,
        wpath.as_ptr(),
        &PNG_ENCODER_CLSID,
        null_mut(),
    );
    GdipDisposeImage(bitmap as *mut GpImage);
    if st != 0 {
        return Err(format!("GdipSaveImageToFile status {st}"));
    }
    Ok(())
}

fn run_upload_and_paste(p: PreparedUpload) -> Result<(), String> {
    let PreparedUpload { payload, host, remote_dir } = p;
    match payload {
        PreparedPayload::Image { local_png, saved_dib } =>
            upload_image_and_paste(&host, &remote_dir, local_png, saved_dib),
        PreparedPayload::Paths { sources, saved_hdrop } =>
            upload_paths_and_paste(&host, &remote_dir, sources, saved_hdrop),
    }
}

fn upload_image_and_paste(
    host: &str,
    remote_dir: &str,
    local_png: PathBuf,
    saved_dib: Option<Vec<u8>>,
) -> Result<(), String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let remote_path = format!("{}/imgpaste-{}.png", remote_dir.trim_end_matches('/'), ts);
    let target = format!("{host}:{remote_path}");

    let status = Command::new("scp")
        .arg("-q")
        .arg(&local_png)
        .arg(&target)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("spawning scp.exe: {e}"))?;

    let _ = std::fs::remove_file(&local_png);

    if !status.success() {
        // Don't paste a path that doesn't exist on the remote. Put the
        // user's screenshot back so they can retry without re-shooting.
        if let Some(dib) = saved_dib {
            let _ = unsafe { restore_clipboard_format(CF_DIB, &dib) };
        }
        return Err(format!("scp exit code {:?}", status.code()));
    }

    unsafe { set_clipboard_text(&remote_path)?; }
    std::thread::sleep(Duration::from_millis(150));
    unsafe { send_ctrl_v(); }
    std::thread::sleep(Duration::from_millis(100));
    if let Some(dib) = saved_dib {
        let _ = unsafe { restore_clipboard_format(CF_DIB, &dib) };
    }
    Ok(())
}

/// Ship one or more copied files/folders to the remote and paste their
/// remote path(s). Everything lands in a per-paste timestamped subfolder
/// (`<remote_dir>/paste-<unix-ts>/`) so original names are preserved, there
/// are no collisions between pastes, and a multi-select stays grouped.
fn upload_paths_and_paste(
    host: &str,
    remote_dir: &str,
    sources: Vec<PathBuf>,
    saved_hdrop: Option<Vec<u8>>,
) -> Result<(), String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let subdir = format!("{}/paste-{}", remote_dir.trim_end_matches('/'), ts);

    // Restore the clipboard file list on any early exit, so a failed transfer
    // doesn't cost the user their Ctrl+C selection.
    let restore = |saved: &Option<Vec<u8>>| {
        if let Some(b) = saved {
            let _ = unsafe { restore_clipboard_format(CF_HDROP, b) };
        }
    };

    // 1) Create the destination subfolder. scp won't make intermediate dirs,
    //    and pre-creating it makes `scp -r src host:subdir/` deterministically
    //    place each item *inside* subdir rather than renaming it. Single-quote
    //    the remote path so a space in remote_dir survives the remote shell.
    let mkdir = Command::new("ssh")
        .arg(host)
        .arg(format!("mkdir -p '{subdir}'"))
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("spawning ssh.exe: {e}"))?;
    if !mkdir.success() {
        restore(&saved_hdrop);
        return Err(format!("ssh mkdir exit code {:?}", mkdir.code()));
    }

    // 2) Copy every source into the subfolder in a single scp connection.
    //    `-r` recurses folders and is harmless for plain files, so one command
    //    form handles a mixed selection.
    let mut cmd = Command::new("scp");
    cmd.arg("-q").arg("-r");
    for src in &sources {
        cmd.arg(src);
    }
    cmd.arg(format!("{host}:'{subdir}/'"));
    let scp = cmd
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("spawning scp.exe: {e}"))?;
    if !scp.success() {
        restore(&saved_hdrop);
        return Err(format!("scp exit code {:?}", scp.code()));
    }

    // 3) Build the remote path(s) to paste, preserving each item's basename.
    //    Quote any path containing a space so it survives being dropped into a
    //    shell or tool that splits on whitespace.
    let joined = sources
        .iter()
        .map(|src| {
            let name = src
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "item".to_string());
            let rp = format!("{subdir}/{name}");
            if rp.contains(' ') { format!("\"{rp}\"") } else { rp }
        })
        .collect::<Vec<_>>()
        .join(" ");

    // 4) Paste the remote path(s), then restore the original file selection.
    unsafe { set_clipboard_text(&joined)?; }
    std::thread::sleep(Duration::from_millis(150));
    unsafe { send_ctrl_v(); }
    std::thread::sleep(Duration::from_millis(100));
    restore(&saved_hdrop);
    Ok(())
}

unsafe fn set_clipboard_text(s: &str) -> Result<(), String> {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * 2;
    let h = GlobalAlloc(GMEM_MOVEABLE, bytes);
    if h.is_null() { return Err("GlobalAlloc failed (set text)".into()); }
    let p = GlobalLock(h);
    if p.is_null() {
        GlobalFree(h);
        return Err("GlobalLock failed (set text)".into());
    }
    std::ptr::copy_nonoverlapping(wide.as_ptr(), p as *mut u16, wide.len());
    GlobalUnlock(h);

    if OpenClipboard(null_mut()) == 0 {
        GlobalFree(h);
        return Err("OpenClipboard failed (set text)".into());
    }
    EmptyClipboard();
    if SetClipboardData(CF_UNICODETEXT, h as HANDLE).is_null() {
        CloseClipboard();
        GlobalFree(h);
        return Err("SetClipboardData(CF_UNICODETEXT) failed".into());
    }
    // Clipboard now owns the HGLOBAL — do NOT GlobalFree.
    CloseClipboard();
    Ok(())
}

/// Re-publish raw bytes previously captured by `snapshot_clipboard_format`
/// under the same format code. Opens/closes the clipboard itself.
unsafe fn restore_clipboard_format(fmt: u32, bytes: &[u8]) -> Result<(), String> {
    if OpenClipboard(null_mut()) == 0 {
        return Err("OpenClipboard failed (restore)".into());
    }
    let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
    if h.is_null() {
        CloseClipboard();
        return Err("GlobalAlloc failed (restore)".into());
    }
    let p = GlobalLock(h);
    if p.is_null() {
        GlobalFree(h);
        CloseClipboard();
        return Err("GlobalLock failed (restore)".into());
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
    GlobalUnlock(h);

    EmptyClipboard();
    if SetClipboardData(fmt, h as HANDLE).is_null() {
        CloseClipboard();
        GlobalFree(h);
        return Err(format!("SetClipboardData(fmt={fmt}) failed (restore)"));
    }
    CloseClipboard();
    Ok(())
}

unsafe fn send_ctrl_v() {
    // Press Ctrl, press V, release V, release Ctrl. All four in a single
    // SendInput call so the OS delivers them atomically.
    let mut inputs: [INPUT; 4] = std::mem::zeroed();
    inputs[0].r#type = INPUT_KEYBOARD;
    inputs[0].Anonymous.ki = KEYBDINPUT {
        wVk: VK_CONTROL, wScan: 0, dwFlags: 0, time: 0, dwExtraInfo: 0,
    };
    inputs[1].r#type = INPUT_KEYBOARD;
    inputs[1].Anonymous.ki = KEYBDINPUT {
        wVk: 0x56 /* 'V' */, wScan: 0, dwFlags: 0, time: 0, dwExtraInfo: 0,
    };
    inputs[2].r#type = INPUT_KEYBOARD;
    inputs[2].Anonymous.ki = KEYBDINPUT {
        wVk: 0x56, wScan: 0, dwFlags: KEYEVENTF_KEYUP, time: 0, dwExtraInfo: 0,
    };
    inputs[3].r#type = INPUT_KEYBOARD;
    inputs[3].Anonymous.ki = KEYBDINPUT {
        wVk: VK_CONTROL, wScan: 0, dwFlags: KEYEVENTF_KEYUP, time: 0, dwExtraInfo: 0,
    };
    SendInput(4, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
}

fn log(msg: &str) {
    use std::io::Write;
    let path = paths::app_dir().join("imgpaste.log");
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
