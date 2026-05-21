// Anthropic usage API fetcher. Port of src/Shared/UsageFetcher.cs.
//
// Cache-first: returns immediately if usage_cache.json is fresh enough.
// Cooldown-aware: skips the HTTP call if we're inside a rate-limit window.
// Persists every successful response to usage_cache.json + usage_history.json
// so the UI can re-render across restarts.

use crate::{
    config_store, cooldown, credentials, models::*, oauth_refresh,
    usage_cache, usage_history,
};

const API_URL: &str = "https://api.anthropic.com/api/oauth/usage";

#[derive(Debug)]
pub enum FetchOutcome {
    /// Cache was still warm; no HTTP call made.
    CacheHit,
    /// HTTP succeeded and the cache+history were updated.
    Refreshed,
    /// Skipped because of an active cooldown.
    Cooldown { remaining_seconds: i32 },
    /// HTTP attempted and failed (auth/network/parse). Last error in `detail`.
    Failed { detail: String },
}

pub fn fetch(http: &ureq::Agent, force: bool) -> FetchOutcome {
    let cfg = config_store::load();

    if !force {
        if let Some(cached) = usage_cache::load() {
            if cached.age_seconds < cfg.poll_interval_sec as f64 && cached.data.is_some() {
                return FetchOutcome::CacheHit;
            }
        }
        let remain = cooldown::remaining_seconds();
        if remain > 0 {
            return FetchOutcome::Cooldown { remaining_seconds: remain };
        }
    }

    if cfg.auto_refresh_token {
        // best-effort; we still attempt the request even if refresh declines
        let _ = oauth_refresh::ensure_fresh(http);
    }

    let Some(token) = credentials::read_access_token() else {
        return FetchOutcome::Failed { detail: "No credentials".into() };
    };

    let mut last_error = String::new();
    for attempt in 0..3 {
        match http
            .get(API_URL)
            .set("Authorization", &format!("Bearer {token}"))
            .set("Accept", "application/json")
            .set("anthropic-beta", "oauth-2025-04-20")
            .call()
        {
            Ok(resp) => {
                let data: UsageResponse = match resp.into_json() {
                    Ok(d) => d,
                    Err(e) => return FetchOutcome::Failed { detail: format!("parse: {e}") },
                };
                let _ = usage_cache::save(Some(&data), &iso_now_utc());
                let _ = usage_history::append(
                    data.five_hour.as_ref().map(|m| m.utilization).unwrap_or(0.0),
                    data.seven_day.as_ref().map(|m| m.utilization).unwrap_or(0.0),
                    data.seven_day_sonnet.as_ref().map(|m| m.utilization).unwrap_or(0.0),
                );
                return FetchOutcome::Refreshed;
            }
            Err(ureq::Error::Status(code, resp)) => {
                let detail = match code {
                    429 => "Too Many Requests",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    500 => "Server Error",
                    502 => "Bad Gateway",
                    503 => "Service Unavailable",
                    _   => "HTTP error",
                };
                last_error = format!("{code} {detail}");
                eprintln!("usage fetch: {last_error}");

                let retry_after = resp.header("Retry-After").map(String::from);
                if code == 429 {
                    let _ = cooldown::engage(retry_after.as_deref(), cooldown::DEFAULT_SEC);
                    return FetchOutcome::Failed { detail: last_error };
                }
                if code == 500 || code == 502 || code == 503 {
                    let _ = cooldown::engage(retry_after.as_deref(), cooldown::SHORT_SEC);
                }
                if code == 401 || code == 403 {
                    return FetchOutcome::Failed { detail: last_error };
                }
            }
            Err(e) => {
                last_error = e.to_string();
                eprintln!("usage fetch error: {last_error}");
            }
        }
        if attempt < 2 {
            let delay = 5 * (attempt + 1);
            std::thread::sleep(std::time::Duration::from_secs(delay));
        }
    }

    FetchOutcome::Failed { detail: last_error }
}

// ─── ISO 8601 timestamp ───────────────────────────────────────────────
// epoch-secs → "YYYY-MM-DDTHH:MM:SSZ". Avoids pulling in chrono for just
// this one helper.

fn iso_now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mn, s) = ymdhms_from_epoch(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mn:02}:{s:02}Z")
}

fn ymdhms_from_epoch(epoch: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = (epoch % 60) as u32;
    let m = ((epoch / 60) % 60) as u32;
    let h = ((epoch / 3600) % 24) as u32;
    let mut days = epoch / 86400;
    let mut year: u32 = 1970;
    loop {
        let days_in = if is_leap(year as i32) { 366 } else { 365 };
        if days < days_in { break; }
        days -= days_in;
        year += 1;
    }
    let mdays = if is_leap(year as i32) {
        [31u64, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31u64, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month: u32 = 1;
    for md in mdays.iter() {
        if days < *md { break; }
        days -= md;
        month += 1;
    }
    (year, month, days as u32 + 1, h, m, s)
}

fn is_leap(y: i32) -> bool { (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 }
