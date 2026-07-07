// In-app self-updater.
//
// ~20 s after launch and every 30 min thereafter, the app asks GitHub — via the
// already-authenticated `gh` CLI — whether a newer *successful* CI build of its
// own architecture exists on `main`. If so it downloads that artifact, swaps the
// running exe using the Windows rename-replace dance, and relaunches.
//
// Two design points worth remembering:
//   * The swap + relaunch run on the UI thread (posted via WM_UPDATE_RELAUNCH),
//     not the worker thread, so the old instance releases its global hotkeys and
//     tray icon *before* the new instance boots and tries to claim them. Doing it
//     off-thread would leave imgpaste/imgpull dead until the next restart.
//   * app_state.json lives beside the exe (see paths::state) and is never
//     touched here, so window positions survive an update — same guarantee a
//     manual in-place swap relies on.
//
// Auth is delegated to `gh` on purpose: the repo is private, so downloads need a
// credential. Reusing gh's stored login avoids persisting a PAT on disk and
// avoids hand-rolling the GitHub REST + signed-blob + unzip dance. If gh is
// absent or unauthenticated every step degrades to a logged no-op — the app is
// never blocked or broken by the updater.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::config_store;

/// Posted to the host window once a new build is downloaded, validated, and
/// staged. Handled on the UI thread so hotkeys/tray release cleanly first.
pub const WM_UPDATE_RELAUNCH: u32 = WM_APP + 4;

const REPO: &str = "paulmah79/Claude-Usage-Systray-Rust";
const CHECK_INTERVAL_SECS: u64 = 1800; // 30 min
const STARTUP_DELAY_SECS:  u64 = 20;   // let first paint + poll settle first

// Suppresses the console window flash when shelling out to gh.exe.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Commit this binary was built from (full 40-char SHA), or "dev" for local
/// builds — which never self-update. Set by build.rs from GITHUB_SHA.
const BUILD_SHA: &str = env!("BUILD_SHA");
/// Target triple this binary was built for, e.g. "x86_64-pc-windows-msvc".
const BUILD_TARGET: &str = env!("BUILD_TARGET");

static RUNNING: AtomicBool = AtomicBool::new(false);
// True while a check (download included) is in flight, so a manual "Refresh
// Now" can't race the periodic loop into a concurrent download of the same
// staging dir.
static CHECKING: AtomicBool = AtomicBool::new(false);

// HWND is a raw pointer (not Send); wrap it so the worker thread can hold it.
// The host window lives for the whole process, so the pointer stays valid.
struct HostHandle(isize);
unsafe impl Send for HostHandle {}
unsafe impl Sync for HostHandle {}
static HOST: Mutex<Option<HostHandle>> = Mutex::new(None);

/// Staged new-exe path, set once download + validation succeed and taken by the
/// UI thread when it installs. Some(_) also means "don't start another check".
static PENDING: Mutex<Option<PathBuf>> = Mutex::new(None);
/// The last SHA we tried to install this session — guards against re-downloading
/// the same build every interval if an install fails. A restart re-attempts.
static LAST_ATTEMPT: Mutex<Option<String>> = Mutex::new(None);

/// Remove a leftover `<exe>.old` (and the staging dir) from a prior self-update.
/// Call once on boot — the freshly-launched build cleans up after its parent.
pub fn cleanup_old_exe() {
    if let Ok(exe) = std::env::current_exe() {
        // The parent may still be exiting; a couple of retries covers the race.
        for _ in 0..5 {
            if std::fs::remove_file(old_exe_path(&exe)).is_ok() { break; }
            if !old_exe_path(&exe).exists() { break; }
            thread::sleep(Duration::from_millis(200));
        }
        let _ = std::fs::remove_dir_all(staging_dir(&exe));
    }
}

/// Spawn the background checker thread. Idempotent.
pub fn start(host: HWND) {
    if RUNNING.swap(true, Ordering::SeqCst) { return; }
    // Log the running version on every launch — makes it easy to confirm from
    // the captured stderr which build is live (e.g. after a self-update).
    eprintln!("update_service: build {BUILD_SHA} ({BUILD_TARGET}) — auto-update polling started");
    *HOST.lock().unwrap() = Some(HostHandle(host as isize));
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(STARTUP_DELAY_SECS));
        loop {
            check_once();
            thread::sleep(Duration::from_secs(CHECK_INTERVAL_SECS));
        }
    });
}

/// Run one check on the current thread, serialized against any other in-flight
/// check so two callers can't download into the same staging dir at once.
fn check_once() {
    if CHECKING.swap(true, Ordering::SeqCst) { return; }
    check_once_inner();
    CHECKING.store(false, Ordering::SeqCst);
}

/// Immediately check for a newer build on a background thread. Wired to the
/// tray's "Refresh Now" so a manual click checks for an update as well as
/// refreshing usage. Clears the once-per-session guard so an explicit click
/// retries even a SHA already attempted; still honours the `auto_update`
/// setting and the dev-build skip. The gh calls + download must not run on the
/// UI thread, hence the spawn.
pub fn trigger_check() {
    thread::spawn(|| {
        if let Ok(mut la) = LAST_ATTEMPT.lock() { *la = None; }
        check_once();
    });
}

fn check_once_inner() {
    // Local/dev builds carry no CI SHA and must never replace themselves.
    if BUILD_SHA == "dev" || BUILD_SHA.is_empty() { return; }
    if !config_store::load().auto_update { return; }
    // An update is already staged and waiting for the UI thread.
    if PENDING.lock().map(|p| p.is_some()).unwrap_or(true) { return; }

    let Some((sha, run_id)) = latest_successful() else { return; };
    if sha == BUILD_SHA { return; } // already current

    // Attempt each distinct SHA at most once per session.
    {
        let mut la = LAST_ATTEMPT.lock().unwrap();
        if la.as_deref() == Some(sha.as_str()) { return; }
        *la = Some(sha.clone());
    }

    match download_and_stage(&sha, &run_id) {
        Ok(staged) => {
            *PENDING.lock().unwrap() = Some(staged);
            eprintln!("update: staged {sha} — requesting relaunch");
            if let Some(h) = host_hwnd() {
                unsafe { PostMessageW(h, WM_UPDATE_RELAUNCH, 0, 0); }
            }
        }
        Err(e) => eprintln!("update: download failed for {sha}: {e}"),
    }
}

/// Latest *successful* CI run on main → (headSha, runId), via `gh`.
fn latest_successful() -> Option<(String, String)> {
    let out = Command::new("gh")
        .args([
            "run", "list", "-R", REPO, "--workflow", "CI", "--branch", "main",
            "--status", "success", "--limit", "1", "--json", "headSha,databaseId",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() { return None; }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let first = json.as_array()?.first()?;
    let sha    = first.get("headSha")?.as_str()?.to_string();
    let run_id = first.get("databaseId")?.as_i64()?.to_string();
    Some((sha, run_id))
}

/// Download this arch's artifact for `sha` into a staging dir; return its exe.
fn download_and_stage(sha: &str, run_id: &str) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let staging = staging_dir(&exe);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;

    let artifact = format!("ClaudeUsageSystray-{BUILD_TARGET}-{sha}");
    let status = Command::new("gh")
        .args(["run", "download", run_id, "-R", REPO, "--name", &artifact, "--dir"])
        .arg(&staging)
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    if !status.success() {
        return Err(io_err("gh run download failed"));
    }
    let staged_exe = staging.join("ClaudeUsageSystray.exe");
    validate_exe(&staged_exe)?;
    Ok(staged_exe)
}

/// Sanity-check a staged download before it's allowed to replace the running
/// exe: plausible size + a PE "MZ" header. Stops a truncated download or an
/// HTML error page from bricking the app.
fn validate_exe(p: &Path) -> std::io::Result<()> {
    let meta = std::fs::metadata(p)?;
    if meta.len() < 100_000 {
        return Err(io_err("staged exe implausibly small"));
    }
    let mut sig = [0u8; 2];
    {
        use std::io::Read;
        std::fs::File::open(p)?.read_exact(&mut sig)?;
    }
    if &sig != b"MZ" {
        return Err(io_err("staged exe missing PE header"));
    }
    Ok(())
}

/// Install the staged update: rename the running exe aside, move the new one
/// into place, and launch it. Runs on the UI thread. Returns true iff the new
/// process was launched — the caller then releases hotkeys/tray and quits.
///
/// On a mid-swap failure it rolls back so the current process stays launchable.
/// If only the final spawn fails, the new exe is already in place, so the next
/// logon starts it — no rollback needed there.
pub fn install_pending() -> bool {
    let Some(staged) = PENDING.lock().ok().and_then(|mut p| p.take()) else { return false; };
    let Ok(exe) = std::env::current_exe() else { return false; };
    let Some(dir) = exe.parent().map(|d| d.to_path_buf()) else { return false; };
    let old = old_exe_path(&exe);

    let _ = std::fs::remove_file(&old);
    // Windows allows renaming a *running* exe (but not overwriting it).
    if let Err(e) = std::fs::rename(&exe, &old) {
        eprintln!("update: could not move running exe aside: {e}");
        return false;
    }
    if let Err(e) = std::fs::rename(&staged, &exe) {
        eprintln!("update: could not move new exe into place: {e}; rolling back");
        let _ = std::fs::rename(&old, &exe);
        return false;
    }
    match Command::new(&exe).current_dir(&dir).spawn() {
        Ok(_) => { eprintln!("update: relaunched new build"); true }
        Err(e) => {
            // New exe is already in place; next logon will run it. Keep serving
            // from the renamed .old file until then.
            eprintln!("update: relaunch spawn failed: {e}; will apply on next start");
            false
        }
    }
}

fn host_hwnd() -> Option<HWND> {
    HOST.lock().ok()?.as_ref().map(|h| h.0 as HWND)
}

/// `…\ClaudeUsageSystray-x64.exe` → `…\ClaudeUsageSystray-x64.exe.old`
fn old_exe_path(exe: &Path) -> PathBuf {
    let mut s = exe.as_os_str().to_os_string();
    s.push(".old");
    PathBuf::from(s)
}

fn staging_dir(exe: &Path) -> PathBuf {
    exe.parent()
        .map(|d| d.join("update-staging"))
        .unwrap_or_else(|| PathBuf::from("update-staging"))
}

fn io_err(msg: &'static str) -> std::io::Error {
    // `Error::other` (not `Error::new(ErrorKind::Other, …)`) — the latter trips
    // clippy::io_other_error under -D warnings on recent stable toolchains.
    std::io::Error::other(msg)
}
