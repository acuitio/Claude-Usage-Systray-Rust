// Append-only-ish history of usage samples, pruned to a rolling time window.
// Each entry is [timestamp, sessionPct, weeklyPct, fablePct]. (The 4th slot
// held the retired Sonnet scoped-weekly metric before 2026-07; older samples
// on disk therefore carry Sonnet% there, newer ones carry Fable%.)

use crate::{models::UsageHistory, paths};

// Keep a rolling 14-day window — the chart's widest view is two weeks.
// Pruning by AGE rather than a fixed count makes the retained span
// independent of poll_interval_sec: 500 samples only spanned ~1.7 days at
// the 5-min default, too short even for a one-week view.
pub const MAX_AGE_SECS: f64 = 14.0 * 86_400.0;

// Hard backstop on entry count, independent of age. Guards the file (and the
// per-append load→save round-trip) against a pathologically short poll
// interval flooding the window with samples. 20k covers 14 days down to a
// ~1-min cadence; below that the count cap trims oldest first, still leaving
// well over a week at any realistic interval.
pub const MAX_ENTRIES: usize = 20_000;

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

// Index of the first entry to keep. History is appended in chronological
// order, so we drop a leading run: first everything older than MAX_AGE_SECS
// relative to the NEWEST sample (measuring against the newest sample, not
// wall-clock now, means a clock jump can't nuke the file), then the count
// backstop. Pure so it can be unit-tested without touching the filesystem.
fn prune_start(history: &[[f64; 4]]) -> usize {
    let mut start = 0usize;
    if let Some(newest) = history.last() {
        let cutoff = newest[0] - MAX_AGE_SECS;
        start = history.iter().position(|e| e[0] >= cutoff).unwrap_or(history.len());
    }
    if history.len() - start > MAX_ENTRIES {
        start = history.len() - MAX_ENTRIES;
    }
    start
}

pub fn save(history: &UsageHistory) -> std::io::Result<()> {
    let path = paths::history();
    let tmp = path.with_extension("json.tmp");

    let trimmed: &[[f64; 4]] = &history[prune_start(history)..];

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

#[cfg(test)]
mod tests {
    use super::*;

    // Build a chronological history spanning `count` samples at `step_secs`
    // apart, ending at t=0 (so timestamps run negative into the past).
    fn series(count: usize, step_secs: f64) -> Vec<[f64; 4]> {
        (0..count)
            .map(|i| {
                let ts = -((count - 1 - i) as f64) * step_secs;
                [ts, 0.0, 0.0, 0.0]
            })
            .collect()
    }

    #[test]
    fn empty_history_keeps_nothing() {
        assert_eq!(prune_start(&[]), 0);
    }

    #[test]
    fn within_window_keeps_all() {
        // 100 samples 10 min apart ≈ 16.6h span — well under 30 days.
        let h = series(100, 600.0);
        assert_eq!(prune_start(&h), 0);
    }

    #[test]
    fn drops_samples_older_than_max_age() {
        // Daily samples over 45 days; only the last 14 days' worth survive.
        let h = series(45, 86_400.0);
        let start = prune_start(&h);
        let newest = h.last().unwrap()[0];
        // Every kept entry is within the age window...
        assert!(h[start..].iter().all(|e| e[0] >= newest - MAX_AGE_SECS));
        // ...and the one before the boundary was correctly excluded.
        assert!(start > 0 && h[start - 1][0] < newest - MAX_AGE_SECS);
    }

    #[test]
    fn count_backstop_caps_dense_series() {
        // 25k samples at 30-sec cadence ≈ 8.7 days — all inside the 14-day age
        // window, so the count backstop (not age) is what bounds retention.
        let h = series(MAX_ENTRIES + 5_000, 30.0);
        assert_eq!(prune_start(&h), h.len() - MAX_ENTRIES);
    }
}
