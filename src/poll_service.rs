// Background polling driver. Spawns one thread; that thread fetches on
// the configured interval and PostMessages the host window so the UI
// thread can repaint everything off cached state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::{config_store, usage_fetcher};

pub const WM_USAGE_UPDATED: u32 = WM_APP + 2;

/// Build a ureq Agent wired to native-tls (Windows SChannel).
///
/// ureq 2.x deliberately does NOT auto-pick native-tls — only the rustls
/// `tls` feature gets default_tls_config(). With `default-features = false,
/// features = ["json", "native-tls"]` in Cargo.toml, the `native-tls` crate
/// is compiled in but is unused unless we hand AgentBuilder a TlsConnector
/// explicitly. Without this, every HTTPS request fails with "cannot make
/// HTTPS request because no TLS backend is configured" (see ureq lib.rs:
/// 401-404 + 414-432). Discovered after fetches went silent for ~22h
/// post a0d59cb.
fn build_http_agent() -> ureq::Agent {
    let tls = ureq::native_tls::TlsConnector::new()
        .expect("native_tls::TlsConnector::new() failed");
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .tls_connector(Arc::new(tls))
        .build()
}

static RUNNING: AtomicBool = AtomicBool::new(false);
// HWND is a raw pointer — *not* Send by default. Wrap it in our own newtype
// + Mutex so we can safely hand it to the worker thread. The HWND lives as
// long as the process, so dereferencing on the worker is sound.
struct HostHandle(isize);
unsafe impl Send for HostHandle {}
unsafe impl Sync for HostHandle {}
static HOST: Mutex<Option<HostHandle>> = Mutex::new(None);

pub fn start(host: HWND) {
    if RUNNING.swap(true, Ordering::SeqCst) { return; }
    *HOST.lock().unwrap() = Some(HostHandle(host as isize));

    thread::spawn(move || {
        let http = build_http_agent();

        // Short ticks instead of one long interval sleep. Each tick is a few
        // local checks (no network): it lets us (a) fetch when the configured
        // interval has genuinely elapsed, (b) notice a wall-clock jump — the
        // machine was asleep, so refetch right away instead of waiting out the
        // remainder — and (c) while the data is stale/auth-blocked, watch the
        // credentials file so a `claude login` heals the widget within one
        // tick instead of a full poll interval. All three paths go through
        // fetch(force=false), so the cache-first and cooldown guards still
        // decide whether any HTTP actually happens.
        const TICK_SECS: u64 = 30;
        let mut next_fetch_due: f64 = 0.0; // 0 = fetch on the first tick (first paint)
        let mut last_tick_wall = unix_now();
        let mut last_creds_mtime = crate::credentials::mtime_secs();

        loop {
            let now = unix_now();
            let slept = now - last_tick_wall > (TICK_SECS as f64) * 3.0;
            last_tick_wall = now;

            let creds_changed = {
                let m = crate::credentials::mtime_secs();
                if m != last_creds_mtime { last_creds_mtime = m; true } else { false }
            };
            let unhealthy = crate::health::status() != crate::health::Status::Live;

            if now >= next_fetch_due || slept || (unhealthy && creds_changed) {
                use usage_fetcher::FetchOutcome::*;
                // Record the outcome into health so the UI can show whether the
                // numbers are live, stale, or blocked on re-auth.
                match usage_fetcher::fetch(&http, false) {
                    Refreshed | CacheHit => crate::health::note_ok(),
                    Cooldown { remaining_seconds } => {
                        crate::health::note_cooldown();
                        eprintln!("poll: cooldown {remaining_seconds}s remaining");
                    }
                    AuthFailed { detail } => {
                        crate::health::note_auth();
                        eprintln!("poll: auth failed — {detail}");
                    }
                    Failed { detail } => {
                        crate::health::note_error();
                        eprintln!("poll: fetch failed — {detail}");
                    }
                }

                // Always nudge the UI — even on failure — so the tray/overlay can
                // re-render the staleness indicator, not just on a successful fetch.
                if let Some(h) = host_hwnd() {
                    unsafe { PostMessageW(h, WM_USAGE_UPDATED, 0, 0); }
                }

                // Re-read the interval each cycle so Settings changes apply live.
                let interval = config_store::load().poll_interval_sec.max(1) as f64;
                next_fetch_due = unix_now() + interval;
            }

            thread::sleep(Duration::from_secs(TICK_SECS));
        }
    });
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn host_hwnd() -> Option<HWND> {
    HOST.lock().ok()?.as_ref().map(|h| h.0 as HWND)
}

/// Post WM_USAGE_UPDATED to the host without doing a fetch. Use this from
/// Settings::Apply so a font/color/opacity change immediately re-renders
/// the tray icon + open overlay/dashboard/chart against the new config.
pub fn notify_ui_refresh() {
    if let Some(h) = host_hwnd() {
        unsafe { PostMessageW(h, WM_USAGE_UPDATED, 0, 0); }
    }
}

/// One-shot force fetch on its own thread. Used by the tray's "Refresh Now"
/// menu item. We don't try to interrupt the polling thread's sleep — just
/// run an independent fetch and post the redraw signal.
pub fn trigger_refresh() {
    let host_addr: Option<isize> = host_hwnd().map(|h| h as isize);
    thread::spawn(move || {
        let http = build_http_agent();
        use usage_fetcher::FetchOutcome::*;
        match usage_fetcher::fetch(&http, true) {
            Refreshed | CacheHit => crate::health::note_ok(),
            Cooldown { .. }      => crate::health::note_cooldown(),
            AuthFailed { .. }    => crate::health::note_auth(),
            Failed { .. }        => crate::health::note_error(),
        }
        if let Some(addr) = host_addr {
            unsafe { PostMessageW(addr as HWND, WM_USAGE_UPDATED, 0, 0); }
        }
    });
}
