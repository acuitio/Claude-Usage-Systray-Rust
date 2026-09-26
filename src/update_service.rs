// In-app self-updater.
//
// ~20 s after launch and every 30 min thereafter, the app asks GitHub's public
// Releases API whether a newer build of its own architecture exists on `main`.
// If so it downloads that release asset without a credential, swaps the running
// exe using the Windows rename-replace dance, and relaunches.
//
// Two design points worth remembering:
//   * The swap + relaunch run on the UI thread (posted via WM_UPDATE_RELAUNCH),
//     not the worker thread, so the old instance releases its global hotkeys and
//     tray icon *before* the new instance boots and tries to claim them. Doing it
//     off-thread would leave imgpaste/imgpull dead until the next restart.
//   * app_state.json lives beside the exe (see paths::state) and is never
//     touched here, so window positions survive an update, the same guarantee a
//     manual in-place swap relies on.

use std::io::{Read, Write};
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

// Moved from paulmah79 on 2026-09-25. Builds still pointing at the old name
// keep upgrading through GitHub's transfer redirect, so never recreate a repo
// under the old name.
const REPO: &str = "acuitio/Claude-Usage-Systray-Rust";
const CHECK_INTERVAL_SECS: u64 = 1800; // 30 min
const STARTUP_DELAY_SECS:  u64 = 20;   // let first paint + poll settle first

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
/// setting and the dev-build skip. The release request + download must not run on the
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

    let Some((sha, url, size)) = latest_release() else { return; };
    if sha == BUILD_SHA { return; } // already current

    // Attempt each distinct SHA at most once per session.
    {
        let mut la = LAST_ATTEMPT.lock().unwrap();
        if la.as_deref() == Some(sha.as_str()) { return; }
        *la = Some(sha.clone());
    }

    match download_and_stage(&url, size) {
        Ok(staged) => {
            *PENDING.lock().unwrap() = Some(staged);
            eprintln!("update: staged {sha} — requesting relaunch");
            if let Some(h) = host_hwnd() {
                unsafe { PostMessageW(h, WM_UPDATE_RELAUNCH, 0, 0); }
            }
        }
        Err(e) => {
            // A failed transfer must not block the periodic retry for this SHA.
            eprintln!("update: download failed for {sha}: {e}");
            if let Ok(mut la) = LAST_ATTEMPT.lock() { *la = None; }
        }
    }
}

/// HTTPS-only agent for the release API and asset download. ureq 2.x never
/// picks native-tls on its own (see poll_service::build_http_agent), so an
/// agent built without `tls_connector` fails every request with "no TLS
/// backend is configured". Builds 1-4 shipped that way and could not update.
fn https_agent(read_secs: u64) -> Option<ureq::Agent> {
    let tls = match ureq::native_tls::TlsConnector::new() {
        Ok(tls) => tls,
        Err(e) => {
            eprintln!("update: TLS connector unavailable: {e}");
            return None;
        }
    };
    Some(ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(read_secs))
        .https_only(true)
        .tls_connector(std::sync::Arc::new(tls))
        .build())
}

/// Latest public release asset for this architecture -> (commit SHA, download URL, size).
fn latest_release() -> Option<(String, String, u64)> {
    let api_url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let agent = https_agent(60)?;
    let response = match agent
        .get(&api_url)
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .set("User-Agent", &format!("ClaudeUsageSystray/{BUILD_SHA}"))
        .call()
    {
        Ok(response) => response,
        Err(e) => {
            eprintln!("update: latest release request failed: {e}");
            return None;
        }
    };
    if response.status() != 200 {
        eprintln!("update: latest release returned HTTP {}", response.status());
        return None;
    }
    let release: serde_json::Value = match serde_json::from_reader(response.into_reader()) {
        Ok(release) => release,
        Err(e) => {
            eprintln!("update: could not parse latest release: {e}");
            return None;
        }
    };
    let asset = pick_asset(&release, BUILD_TARGET);
    if asset.is_none() { eprintln!("update: latest release has no matching asset"); }
    asset
}

/// Select an exact, trusted release asset for one build target.
fn pick_asset(release: &serde_json::Value, target: &str) -> Option<(String, String, u64)> {
    let prefix = format!("ClaudeUsageSystray-{target}-");
    let trusted_url_prefix = format!("https://github.com/{REPO}/releases/download/");
    for asset in release.get("assets")?.as_array()? {
        let Some(name) = asset.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(sha) = name.strip_prefix(&prefix).and_then(|name| name.strip_suffix(".exe")) else {
            continue;
        };
        if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')) {
            continue;
        }
        let Some(url) = asset
            .get("browser_download_url")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Some(size) = asset.get("size").and_then(serde_json::Value::as_u64) else {
            continue;
        };
        if url.starts_with(&trusted_url_prefix) {
            return Some((sha.to_owned(), url.to_owned(), size));
        }
    }
    None
}

/// Download a release asset into a staging dir; return its exe. `size` is the
/// byte count the Releases API reported, so a cleanly truncated body is caught.
fn download_and_stage(url: &str, size: u64) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let staging = staging_dir(&exe);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;

    let agent = https_agent(300).ok_or_else(|| io_err("no TLS connector"))?;
    let response = agent
        .get(url)
        .set("User-Agent", &format!("ClaudeUsageSystray/{BUILD_SHA}"))
        .call()
        .map_err(|e| std::io::Error::other(format!("release download request failed: {e}")))?;
    if response.status() != 200 {
        return Err(std::io::Error::other(format!(
            "release download returned HTTP {}",
            response.status()
        )));
    }
    let staged_exe = staging.join("ClaudeUsageSystray.exe");
    let mut body = response.into_reader();
    let mut file = std::fs::File::create(&staged_exe)?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = body.read(&mut buffer)?;
        if read == 0 { break; }
        total += read as u64;
        if total > 64 * 1024 * 1024 {
            return Err(io_err("release download exceeds 64 MiB"));
        }
        file.write_all(&buffer[..read])?;
    }
    if total != size {
        return Err(io_err("release download size does not match the release asset"));
    }
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
    std::fs::File::open(p)?.read_exact(&mut sig)?;
    if &sig != b"MZ" {
        return Err(io_err("staged exe missing PE header"));
    }
    Ok(())
}

/// Install the staged update: rename the running exe aside, move the new one
/// into place, and launch it. Runs on the UI thread. Returns true iff the new
/// process was launched — the caller then releases hotkeys/tray and quits.
///
/// On any failure, including the final spawn, it rolls back so the original exe
/// is back at the startup path and the current process keeps running.
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
            // Windows could not start the new exe: put the original back so the
            // next logon does not try to launch a build that cannot run.
            eprintln!("update: relaunch spawn failed: {e}; rolling back");
            if let Err(e) = std::fs::rename(&exe, &staged) {
                eprintln!("update: could not move new exe aside: {e}; deleting it");
                if let Err(e) = std::fs::remove_file(&exe) {
                    eprintln!("update: ROLLBACK FAILED, restore {} by hand: {e}", old.display());
                    return false;
                }
            }
            if let Err(e) = std::fs::rename(&old, &exe) {
                eprintln!("update: ROLLBACK FAILED, restore {} by hand: {e}", old.display());
            }
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{pick_asset, REPO};

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const X64: &str = "x86_64-pc-windows-msvc";
    const ARM64: &str = "aarch64-pc-windows-msvc";

    fn asset(target: &str, sha: &str, url: &str) -> serde_json::Value {
        json!({
            "name": format!("ClaudeUsageSystray-{target}-{sha}.exe"),
            "browser_download_url": url,
            "size": 123_456,
        })
    }

    #[test]
    fn picks_matching_x64_asset() {
        let url = format!("https://github.com/{REPO}/releases/download/build-7/x64.exe");
        let release = json!({ "assets": [asset(X64, SHA, &url)] });

        assert_eq!(pick_asset(&release, X64), Some((SHA.to_owned(), url, 123_456)));
    }

    #[test]
    fn ignores_other_target_before_matching_asset() {
        let arm_url = format!("https://github.com/{REPO}/releases/download/build-7/arm64.exe");
        let x64_url = format!("https://github.com/{REPO}/releases/download/build-7/x64.exe");
        let release = json!({
            "assets": [asset(ARM64, SHA, &arm_url), asset(X64, SHA, &x64_url)],
        });

        assert_eq!(pick_asset(&release, X64), Some((SHA.to_owned(), x64_url, 123_456)));
    }

    #[test]
    fn rejects_non_hex_sha() {
        let url = format!("https://github.com/{REPO}/releases/download/build-7/x64.exe");
        let release = json!({ "assets": [asset(X64, "0123456789abcdef0123456789abcdef0123456G", &url)] });

        assert_eq!(pick_asset(&release, X64), None);
    }

    #[test]
    fn rejects_download_urls_outside_the_release_repo() {
        let other_host = asset(X64, SHA, "https://example.com/download/x64.exe");
        let other_repo = asset(
            X64,
            SHA,
            "https://github.com/acuitio/another-repo/releases/download/build-7/x64.exe",
        );
        let release = json!({ "assets": [other_host, other_repo] });

        assert_eq!(pick_asset(&release, X64), None);
    }

    #[test]
    fn rejects_uppercase_sha() {
        let url = format!("https://github.com/{REPO}/releases/download/build-7/x64.exe");
        let release = json!({ "assets": [asset(X64, &SHA.to_uppercase(), &url)] });

        assert_eq!(pick_asset(&release, X64), None);
    }

    #[test]
    fn returns_none_when_assets_are_missing() {
        assert_eq!(pick_asset(&json!({}), X64), None);
    }
}
