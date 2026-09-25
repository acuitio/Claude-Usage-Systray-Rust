// GX10 quota feed fetcher and its separate, nullable history file.

use std::io::Read;
use std::sync::Mutex;
use std::time::Duration;

use crate::models::{QuotaFeedEnvelope, QuotaFeedResponse, QuotaSeries};

pub type QuotaFeedHistory = Vec<(f64, Option<f64>, Option<f64>, Option<f64>, Option<f64>)>;
pub type QuotaFeedHistoryRow = (f64, Option<f64>, Option<f64>, Option<f64>, Option<f64>);

static HISTORY_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug)]
pub enum FetchOutcome {
    Refreshed,
    Failed(String),
}

pub fn build_http_agent() -> ureq::Agent {
    let tls = ureq::native_tls::TlsConnector::new()
        .expect("native_tls::TlsConnector::new() failed");
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(5))
        .redirects(0)
        .tls_connector(std::sync::Arc::new(tls))
        .build()
}

pub fn parse(bytes: &[u8]) -> Result<QuotaFeedResponse, String> {
    serde_json::from_slice(bytes).map_err(|e| format!("invalid JSON: {e}"))
}

pub fn find<'a>(resp: &'a QuotaFeedResponse, meter: &str, window: &str) -> Option<&'a QuotaSeries> {
    resp.series.iter().find(|s| s.meter == meter && s.window == window)
}

pub fn next_delay(consecutive_failures: u32, poll_interval: f64, max_backoff: f64) -> f64 {
    let base = if poll_interval.is_finite() && poll_interval > 0.0 { poll_interval } else { 300.0 };
    let cap = if max_backoff.is_finite() && max_backoff > 0.0 { max_backoff } else { 7200.0 };
    let multiplier = 2_f64.powi(consecutive_failures.min(52) as i32);
    (base * multiplier).min(cap)
}

pub fn is_stale(observed_at: Option<f64>, now: f64, poll_interval: f64) -> bool {
    let Some(observed_at) = observed_at.filter(|v| v.is_finite()) else { return true; };
    let interval = if poll_interval.is_finite() && poll_interval > 0.0 { poll_interval } else { 300.0 };
    now - observed_at > interval * 3.0
}

pub fn poll_interval(resp: Option<&QuotaFeedResponse>) -> f64 {
    resp.and_then(|r| r.freshness.as_ref())
        .and_then(|f| f.poll_interval_seconds)
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(300.0)
}

pub fn max_backoff(resp: Option<&QuotaFeedResponse>) -> f64 {
    resp.and_then(|r| r.freshness.as_ref())
        .and_then(|f| f.max_backoff_seconds)
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(7200.0)
}

pub fn fetch(url: &str) -> FetchOutcome {
    let now = unix_now();
    if url.contains('?') {
        return failure(now, "feed URL must not contain a query string".into());
    }
    let agent = build_http_agent();
    let response = match agent.get(url).set("Accept", "application/json").call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, _)) => return failure(now, format!("HTTP {code}")),
        Err(e) => return failure(now, e.to_string()),
    };
    // Redirects are off, so a 3xx arrives here as Ok; only a plain 200 counts.
    if response.status() != 200 {
        return failure(now, format!("HTTP {}", response.status()));
    }
    let mut bytes = Vec::new();
    if let Err(e) = response.into_reader().take(1 << 20).read_to_end(&mut bytes) {
        return failure(now, format!("read: {e}"));
    }
    let data = match parse(&bytes) {
        Ok(data) if data.error.is_none() => data,
        Ok(data) => return failure(now, data.error.unwrap_or_else(|| "feed error".into())),
        Err(e) => return failure(now, e),
    };

    let history_row = history_row(&data);
    let saved = data.clone();
    let _ = crate::state_store::update(|state| {
        state.quota_feed = Some(QuotaFeedEnvelope {
            data: Some(saved),
            fetched_at: now,
            last_attempt_at: now,
            last_error: None,
        });
    });
    if let Some(row) = history_row {
        let _ = append_history(row);
    }
    FetchOutcome::Refreshed
}

fn failure(now: f64, detail: String) -> FetchOutcome {
    let _ = crate::state_store::update(|state| {
        let envelope = state.quota_feed.get_or_insert_with(QuotaFeedEnvelope::default);
        envelope.last_attempt_at = now;
        envelope.last_error = Some(detail.clone());
    });
    FetchOutcome::Failed(detail)
}

fn history_row(data: &QuotaFeedResponse) -> Option<QuotaFeedHistoryRow> {
    let alt_session = find(data, "claude_alt", "five_hour");
    let alt_weekly = find(data, "claude_alt", "seven_day");
    let alt_fable = find(data, "claude_alt", "seven_day_fable");
    let codex = find(data, "codex", "codex_primary");
    let ts = [alt_session, alt_weekly, alt_fable, codex].into_iter()
        .flatten().filter_map(|s| s.observed_at).filter(|v| v.is_finite())
        .fold(None, |latest: Option<f64>, value| Some(latest.map_or(value, |old| old.max(value))))?;
    Some((ts, alt_session.and_then(|s| s.pct), alt_weekly.and_then(|s| s.pct),
        alt_fable.and_then(|s| s.pct), codex.and_then(|s| s.pct)))
}

pub fn append_pruned(history: &mut QuotaFeedHistory, row: QuotaFeedHistoryRow) -> bool {
    if history.last().map(|last| row.0 <= last.0).unwrap_or(false) { return false; }
    history.push(row);
    let newest = history.last().map(|r| r.0).unwrap_or(0.0);
    let cutoff = newest - crate::usage_history::MAX_AGE_SECS;
    let age_start = history.iter().position(|r| r.0 >= cutoff).unwrap_or(history.len());
    let count_start = history.len().saturating_sub(crate::usage_history::MAX_ENTRIES);
    history.drain(..age_start.max(count_start));
    true
}

fn load_history() -> QuotaFeedHistory {
    std::fs::read(crate::paths::quota_feed_history()).ok()
        .and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn append_history(row: QuotaFeedHistoryRow) -> std::io::Result<()> {
    let _guard = HISTORY_LOCK.lock().unwrap();
    let mut history = load_history();
    if !append_pruned(&mut history, row) { return Ok(()); }
    let path = crate::paths::quota_feed_history();
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec(&history)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, path)
}

fn unix_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"freshness":{"poll_interval_seconds":10.5,"max_backoff_seconds":80.0},"series":[{"meter":"codex","window":"codex_primary","pct":null,"resets_at":null,"observed_at":20.5,"projection":null},{"meter":"unknown","window":"pair","pct":1.0},{"meter":"claude_alt","window":"seven_day","pct":50.0,"resets_at":30.2,"observed_at":19.5,"projection":{"pct":80.0,"at":30.2,"label":"proj 80%"}},{"meter":"claude_alt","window":"five_hour","pct":0.0,"resets_at":null,"observed_at":18.5,"projection":null},{"meter":"claude_alt","window":"seven_day_fable","pct":43.0,"resets_at":30.2,"observed_at":19.5,"projection":null}]}"#;

    #[test]
    fn parses_float_null_and_unordered_pairs() {
        let response = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(find(&response, "claude_alt", "five_hour").unwrap().pct, Some(0.0));
        assert_eq!(find(&response, "codex", "codex_primary").unwrap().pct, None);
        assert!(find(&response, "unknown", "pair").is_some());
        let row = history_row(&response).unwrap();
        assert_eq!(row, (20.5, Some(0.0), Some(50.0), Some(43.0), None));
        assert_eq!(response.freshness.unwrap().poll_interval_seconds, Some(10.5));
    }

    #[test]
    fn parses_error_response_and_rejects_non_json() {
        let response = parse(br#"{"error":"ledger unavailable","series":[]}"#).unwrap();
        assert_eq!(response.error.as_deref(), Some("ledger unavailable"));
        assert!(parse(b"<html>login</html>").is_err());
    }

    #[test]
    fn lookup_uses_pair() {
        let response = parse(SAMPLE.as_bytes()).unwrap();
        assert!(find(&response, "claude_alt", "codex_primary").is_none());
    }

    #[test]
    fn delay_doubles_and_caps() {
        assert_eq!(next_delay(0, 300.0, 7200.0), 300.0);
        assert_eq!(next_delay(1, 300.0, 7200.0), 600.0);
        assert_eq!(next_delay(8, 300.0, 7200.0), 7200.0);
    }

    #[test]
    fn stale_starts_after_three_intervals() {
        assert!(!is_stale(Some(100.0), 130.0, 10.0));
        assert!(is_stale(Some(100.0), 130.001, 10.0));
        assert!(is_stale(None, 100.0, 10.0));
    }

    #[test]
    fn history_skips_old_observation_and_prunes() {
        let mut history = vec![(0.0, Some(1.0), None, None, None)];
        assert!(!append_pruned(&mut history, (0.0, Some(2.0), None, None, None)));
        assert!(append_pruned(&mut history, (crate::usage_history::MAX_AGE_SECS + 1.0, None, None, None, Some(3.0))));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].4, Some(3.0));
    }
}
