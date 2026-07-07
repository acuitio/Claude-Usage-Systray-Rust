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

/// Load current state for display purposes. Degrades to defaults on any
/// error (transient read failure or parse failure) — callers that only read
/// (tray tooltip, overlay render, dashboard snapshot) never write, so there's
/// no risk of a transient blip getting persisted back over real settings.
pub fn load() -> AppState {
    let _g = FILE_LOCK.lock().unwrap();
    load_locked().unwrap_or_default()
}

/// Read-modify-write under a single critical section. Always use this to
/// mutate state — concurrent load() + save() pairs can lose each other's
/// changes.
pub fn update<F: FnOnce(&mut AppState)>(f: F) -> io::Result<()> {
    let _g = FILE_LOCK.lock().unwrap();
    // Propagating the read error here is the whole point — never mutate-and
    // -save on top of defaults we only got because the file was temporarily
    // unreadable (e.g. locked by another process, disk hiccup). A load()-side
    // fallback to defaults is harmless for a read-only display; here it would
    // silently wipe the user's real config.
    let mut state = load_locked()?;
    f(&mut state);
    save_locked(&state)
}

fn load_locked() -> io::Result<AppState> {
    migrate_if_needed();
    let path = paths::state();
    if !path.exists() { return Ok(AppState::default()); }
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(AppState::default()),
        Err(e) => {
            log_line(&format!("read failed (state preserved, save skipped): {e}"));
            return Err(e);
        }
    };
    // Tolerate a UTF-8 BOM. serde_json rejects a leading EF BB BF, and any
    // editor or PowerShell `Set-Content -Encoding utf8` that touches this
    // file adds one — which would otherwise silently reset every setting.
    let slice = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(stripped) => stripped,
        None           => &bytes[..],
    };
    match serde_json::from_slice(slice) {
        Ok(state) => Ok(state),
        Err(e) => {
            // Never silently discard the user's settings on a parse error:
            // the immediate fallback-to-default would be saved straight back
            // over the file on the next write, destroying any chance of
            // recovery. Preserve the offending file (the .bad rename) and
            // leave a breadcrumb, then hand back defaults — safe here because
            // the original bytes are no longer at risk of being overwritten.
            log_parse_error(&path, &e);
            Ok(AppState::default())
        }
    }
}

/// Move an unparseable state file aside (so the next save can't clobber it)
/// and append a one-line note. Best-effort — failures here are non-fatal.
fn log_parse_error(path: &std::path::Path, err: &serde_json::Error) {
    let _ = std::fs::rename(path, path.with_extension("json.bad"));
    log_line(&format!("parse failed, saved as app_state.json.bad: {err}"));
}

/// Append one line to state_store.log. Best-effort — failures here are
/// non-fatal.
fn log_line(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true)
        .open(paths::app_dir().join("state_store.log"))
    {
        let _ = writeln!(f, "{msg}");
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
