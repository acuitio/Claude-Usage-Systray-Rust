// Single source of truth for "is the displayed usage data actually live?"
//
// The fetcher can fail in ways that leave a stale-but-plausible number on
// screen — most importantly a 401 when Claude Code's OAuth token gets
// invalidated (expiry, or a re-login on another machine rotating the refresh
// token). Before this, such failures only hit eprintln!, which goes nowhere in
// a windows_subsystem app, so the widget silently froze. This module records
// the last fetch outcome and combines it with the cache age so every UI surface
// (tray icon, tooltip, overlay) can show the truth and point the user at the
// fix.
//
// poll_service calls the note_* setters; the UI calls status().

use std::sync::atomic::{AtomicU8, Ordering};

use crate::{config_store, usage_cache};

// Below this, a brief blip / short cooldown won't raise the alarm; above it the
// data is old enough that we should say so. Auth failures bypass this entirely.
const STALE_FLOOR_SEC: f64 = 900.0;

/// What the UI should communicate about the freshness of the numbers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Fresh — last fetch succeeded recently.
    Live,
    /// No successful fetch in a while (cooldown, network, server). Show but flag.
    Stale,
    /// Token rejected (401/403) — actionable: the user must re-authenticate.
    Auth,
}

// Last fetch outcome class. Auth is sticky until a later success clears it.
const O_OK: u8 = 0;
const O_COOLDOWN: u8 = 1;
const O_AUTH: u8 = 2;
const O_ERROR: u8 = 3;

static LAST: AtomicU8 = AtomicU8::new(O_OK);

pub fn note_ok()       { LAST.store(O_OK, Ordering::Relaxed); }
pub fn note_cooldown() { LAST.store(O_COOLDOWN, Ordering::Relaxed); }
pub fn note_auth()     { LAST.store(O_AUTH, Ordering::Relaxed); }
pub fn note_error()    { LAST.store(O_ERROR, Ordering::Relaxed); }

/// Combine the last outcome with the cache age into a UI-facing status.
pub fn status() -> Status {
    // A rejected token is definitive — surface it immediately, regardless of
    // how recently the cache happened to be written.
    if LAST.load(Ordering::Relaxed) == O_AUTH {
        return Status::Auth;
    }
    let interval = config_store::load().poll_interval_sec.max(1) as f64;
    // Tolerate a couple of missed polls before crying stale.
    let threshold = (interval * 3.0).max(STALE_FLOOR_SEC);
    let stale = match usage_cache::load() {
        Some(c) => !c.age_seconds.is_finite() || c.age_seconds > threshold,
        None => true, // no cache at all → nothing live to show
    };
    if stale { Status::Stale } else { Status::Live }
}

/// "3h 12m" / "45m" / "30s" — compact age for the tooltip.
pub fn cache_age_label() -> String {
    let secs = usage_cache::load().map(|c| c.age_seconds).unwrap_or(f64::INFINITY);
    if !secs.is_finite() {
        return "never".into();
    }
    let s = secs.max(0.0) as i64;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86_400, (s % 86_400) / 3600)
    }
}
