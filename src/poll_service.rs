// Background polling driver. Spawns one thread; that thread fetches on
// the configured interval and PostMessages the host window so the UI
// thread can repaint everything off cached state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::{config_store, usage_fetcher};

pub const WM_USAGE_UPDATED: u32 = WM_APP + 2;

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
        let http = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build();

        // Fire immediately for first paint, then loop on interval.
        let mut force = false;
        loop {
            match usage_fetcher::fetch(&http, force) {
                usage_fetcher::FetchOutcome::Refreshed | usage_fetcher::FetchOutcome::CacheHit => {
                    if let Some(h) = host_hwnd() {
                        unsafe { PostMessageW(h, WM_USAGE_UPDATED, 0, 0); }
                    }
                }
                usage_fetcher::FetchOutcome::Cooldown { remaining_seconds } => {
                    eprintln!("poll: cooldown {remaining_seconds}s remaining");
                }
                usage_fetcher::FetchOutcome::Failed { detail } => {
                    eprintln!("poll: fetch failed — {detail}");
                }
            }

            // Re-read interval each cycle so changes in Settings take effect
            // without restarting the app.
            let secs = config_store::load().poll_interval_sec.max(1);
            thread::sleep(Duration::from_secs(secs as u64));
            force = false;
        }
    });
}

fn host_hwnd() -> Option<HWND> {
    HOST.lock().ok()?.as_ref().map(|h| h.0 as HWND)
}

/// One-shot force fetch on its own thread. Used by the tray's "Refresh Now"
/// menu item. We don't try to interrupt the polling thread's sleep — just
/// run an independent fetch and post the redraw signal.
pub fn trigger_refresh() {
    let host_addr: Option<isize> = host_hwnd().map(|h| h as isize);
    thread::spawn(move || {
        let http = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build();
        let _ = usage_fetcher::fetch(&http, true);
        if let Some(addr) = host_addr {
            unsafe { PostMessageW(addr as HWND, WM_USAGE_UPDATED, 0, 0); }
        }
    });
}
