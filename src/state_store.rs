// Single source of truth for the combined on-disk state file. All three of
// {config, cache, cooldown} now live in one `app_state.json`; this module
// owns reads, writes, and the in-process Mutex that serializes them.
//
// Migration: if `app_state.json` doesn't exist on first load but the
// pre-consolidation files do, we read them, merge into AppState, save the
// new file, and delete the old ones.

use std::io;
use std::sync::Mutex;

use crate::models::{AppConfig, AppState, CacheEnvelope, CooldownState};
use crate::paths;

// Serializes load/save across threads. The atomic-rename pattern handles
// process-level correctness already; this prevents two threads from racing
// load → mutate → save and clobbering each other's changes.
static FILE_LOCK: Mutex<()> = Mutex::new(());

pub fn load() -> AppState {
    let _g = FILE_LOCK.lock().unwrap();
    load_locked()
}

/// Read-modify-write under a single critical section. Always use this to
/// mutate state — concurrent load() + save() pairs can lose each other's
/// changes.
pub fn update<F: FnOnce(&mut AppState)>(f: F) -> io::Result<()> {
    let _g = FILE_LOCK.lock().unwrap();
    let mut state = load_locked();
    f(&mut state);
    save_locked(&state)
}

fn load_locked() -> AppState {
    migrate_if_needed();
    let path = paths::state();
    if !path.exists() { return AppState::default(); }
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_)    => AppState::default(),
    }
}

fn save_locked(state: &AppState) -> io::Result<()> {
    let path = paths::state();
    let tmp  = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// One-time migration from the four-file layout to the combined file.
/// Runs at most once per process — repeated calls are cheap (single
/// stat check + early return).
fn migrate_if_needed() {
    let state_path = paths::state();
    if state_path.exists() { return; }

    // No new file. If none of the legacy files exist either, this is a
    // fresh install — let the default AppState be written on first save.
    let lc = paths::legacy_config();
    let lh = paths::legacy_cache();
    let lr = paths::legacy_cooldown();
    if !lc.exists() && !lh.exists() && !lr.exists() { return; }

    let mut state = AppState::default();
    if let Ok(b) = std::fs::read(&lc) {
        if let Ok(c) = serde_json::from_slice::<AppConfig>(&b) {
            state.config = c;
        }
    }
    if let Ok(b) = std::fs::read(&lh) {
        if let Ok(c) = serde_json::from_slice::<CacheEnvelope>(&b) {
            state.cache = Some(c);
        }
    }
    if let Ok(b) = std::fs::read(&lr) {
        if let Ok(c) = serde_json::from_slice::<CooldownState>(&b) {
            state.cooldown = c;
        }
    }

    if save_locked(&state).is_ok() {
        let _ = std::fs::remove_file(&lc);
        let _ = std::fs::remove_file(&lh);
        let _ = std::fs::remove_file(&lr);
    }
}
