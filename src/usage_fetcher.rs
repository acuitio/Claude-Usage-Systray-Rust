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
    /// Token rejected (401/403). Actionable: the user must re-authenticate
    /// (e.g. `claude login`). Kept distinct from Failed so the UI can show a
    /// specific "sign-in expired" prompt instead of a generic stale marker.
    AuthFailed { detail: String },
    /// HTTP attempted and failed (network/parse/5xx). Last error in `detail`.
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
    }
    // The cooldown encodes the server's Retry-After — authoritative even for
    // a manual "Refresh Now". `force` bypasses only our local cache-freshness
    // heuristic, never the server's back-off signal.
    let remain = cooldown::remaining_seconds();
    if remain > 0 {
        return FetchOutcome::Cooldown { remaining_seconds: remain };
    }

    // If the refresh-token family is known-dead (a prior 400) and nothing has
    // rewritten the credentials file since, stop calling the API altogether.
    // Each GET would only 401, and a steady drip of them provokes a server 429
    // whose hour-long Retry-After then blocks recovery even after re-auth. This
    // self-clears the moment `claude auth login` rewrites the file, so the next
    // poll unblocks on its own.
    if oauth_refresh::family_is_dead() {
        return FetchOutcome::AuthFailed {
            detail: "Sign-in expired — run: claude auth login".into(),
        };
    }

    if cfg.auto_refresh_token {
        // best-effort; we still attempt the request even if refresh declines
        let _ = oauth_refresh::ensure_fresh(http, false);
    }

    let Some(mut token) = credentials::read_access_token() else {
        // A *missing* token (Claude Code signed out / cleared its creds) needs
        // the same action as a rejected one — re-login — so classify it as
        // AuthFailed, not a generic failure, to get the actionable prompt.
        return FetchOutcome::AuthFailed { detail: "No credentials (signed out)".into() };
    };

    let mut last_error = String::new();
    let mut retried_auth = false;
    let mut attempt = 0;
    while attempt < 3 {
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
                    data.session_metric().0,
                    data.weekly_metric().0,
                    // 4th column is now the scoped-weekly (Fable) percent — the
                    // Sonnet field the API used to fill here is permanently null.
                    data.fable_limit().map(|l| l.percent).unwrap_or(0.0),
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
                    // The token can be revoked ahead of its printed expiry (a
                    // re-login on another machine rotates the family), so
                    // expires_at can't be trusted here. Force ONE refresh-token
                    // attempt and retry with whatever token results — this
                    // turns a dead-until-expiry outage into a seconds-long blip
                    // when the refresh token is still alive. Bounded: one
                    // retry per fetch, and ensure_fresh has its own 60 s
                    // throttle + dead-family latch.
                    if !retried_auth {
                        retried_auth = true;
                        if oauth_refresh::ensure_fresh(http, true) {
                            if let Some(t) = credentials::read_access_token() {
                                token = t;
                                continue; // retry now; doesn't consume an attempt
                            }
                        }
                    }
                    return FetchOutcome::AuthFailed { detail: last_error };
                }
            }
            Err(e) => {
                last_error = e.to_string();
                eprintln!("usage fetch error: {last_error}");
            }
        }
        attempt += 1;
        if attempt < 3 {
            std::thread::sleep(std::time::Duration::from_secs(5 * attempt as u64));
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
