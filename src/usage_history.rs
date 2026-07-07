// Append-only-ish history of usage samples, bounded at 500 entries.
// Each entry is [timestamp, sessionPct, weeklyPct, fablePct]. (The 4th slot
// held the retired Sonnet scoped-weekly metric before 2026-07; older samples
// on disk therefore carry Sonnet% there, newer ones carry Fable%.)

use crate::{models::UsageHistory, paths};

pub const MAX_ENTRIES: usize = 500;

// append() is called from the poll thread AND ad-hoc "Refresh Now" threads;
// unserialized, both write the same .json.tmp and can publish interleaved
// garbage through the atomic rename (or drop a sample).
static APPEND_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn load() -> UsageHistory {
    let path = paths::history();
    if !path.exists() { return Vec::new(); }
    std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(history: &UsageHistory) -> std::io::Result<()> {
    let path = paths::history();
    let tmp = path.with_extension("json.tmp");
    let trimmed: &[[f64; 4]] = if history.len() > MAX_ENTRIES {
        &history[history.len() - MAX_ENTRIES..]
    } else {
        history
    };
    let json = serde_json::to_vec(&trimmed)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn append(session_pct: f64, weekly_pct: f64, fable_pct: f64) -> std::io::Result<()> {
    let _g = APPEND_LOCK.lock().unwrap();
    let mut hist = load();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    hist.push([ts, session_pct, weekly_pct, fable_pct]);
    save(&hist)
}
