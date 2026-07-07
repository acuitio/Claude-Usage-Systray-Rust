// OAuth token refresh. Port of src/Shared/OAuthRefresh.cs.
//
// Anthropic's policy states OAuth tokens are intended for Claude Code and
// Claude.ai. We piggyback on the same refresh-token grant. If the user
// disables auto-refresh (config.auto_refresh_token = false), we just read
// whatever access token Claude Code last wrote — passive mode.
//
// Refresh tokens rotate. If Claude Code refreshes between our read and
// POST, our refresh_token is dead and we get a 400; bail silently and
// next pass will pick up Claude Code's fresh token.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::{credentials, models::OauthRefreshResponse};

const TOKEN_URL:          &str = "https://console.anthropic.com/v1/oauth/token";
const TOKEN_URL_FALLBACK: &str = "https://claude.ai/v1/oauth/token";
const CLIENT_ID:          &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const BUFFER_SEC: f64 = 300.0;

static LAST_ATTEMPT: AtomicU64 = AtomicU64::new(0);

// Set to the credentials file's mtime when a refresh grant came back 400
// (refresh token family revoked). While the file hasn't changed since, all
// further refresh attempts are skipped — only Claude Code writing new
// credentials (mtime change) can revive the family. 0 = not latched.
static DEAD_FAMILY_MTIME: AtomicU64 = AtomicU64::new(0);

/// Returns true if the credentials file holds a valid (fresh-enough) access
/// token after this call. Either:
///   - it was already fresh (skipped when `force` is true),
///   - we successfully refreshed and persisted, or
///   - false if anything went wrong (caller falls back to passive mode).
///
/// `force` skips the "already fresh?" check: a 401 from the API means the
/// token is dead right now regardless of what expires_at claims (e.g. the
/// refresh-token family was rotated by a re-login on another machine). The
/// 60 s LAST_ATTEMPT throttle still applies either way, so a caller can't
/// hammer the token endpoint just by passing force=true repeatedly.
pub fn ensure_fresh(http: &ureq::Agent, force: bool) -> bool {
    let mtime = credentials::mtime_secs();
    let dead = DEAD_FAMILY_MTIME.load(Ordering::Relaxed);
    if dead != 0 {
        if mtime == dead {
            return false; // family known-dead and nothing has rewritten the file
        }
        DEAD_FAMILY_MTIME.store(0, Ordering::Relaxed); // new file — try again
    }

    let Some(creds) = credentials::read_full() else { return false; };
    let Some(oauth) = creds.claude_ai_oauth.clone() else { return false; };
    let Some(refresh_token) = oauth.refresh_token.clone() else {
        eprintln!("no refreshToken in credentials — skipping refresh");
        return false;
    };

    // Already fresh? (skipped under force — see doc comment above)
    let now_secs = unix_now_secs();
    if !force && (oauth.expires_at as f64) > (now_secs + BUFFER_SEC) * 1000.0 {
        return true;
    }

    // Throttle to one attempt per 60 s
    let last = LAST_ATTEMPT.load(Ordering::Relaxed);
    let now_u = now_secs as u64;
    if now_u.saturating_sub(last) < 60 {
        eprintln!("refresh throttled (last attempt <60s ago)");
        return false;
    }
    LAST_ATTEMPT.store(now_u, Ordering::Relaxed);

    let body = serde_json::json!({
        "grant_type":    "refresh_token",
        "refresh_token": refresh_token,
        "client_id":     CLIENT_ID,
    });
    eprintln!(
        "OAuth refresh — token expires in {}s",
        (oauth.expires_at as f64 / 1000.0 - now_secs) as i64
    );

    for url in [TOKEN_URL, TOKEN_URL_FALLBACK] {
        match http
            .post(url)
            .set("User-Agent", "claude-code/2.0.0")
            .set("Content-Type", "application/json")
            .send_json(&body)
        {
            Ok(resp) => {
                let data: OauthRefreshResponse = match resp.into_json() {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("refresh response parse error: {e}");
                        return false;
                    }
                };
                let Some(access) = data.access_token else {
                    eprintln!("refresh response missing access_token");
                    return false;
                };

                // Re-read in case Claude Code raced us during the POST.
                let mut latest = credentials::read_full().unwrap_or(creds);
                let mut o = latest.claude_ai_oauth.unwrap_or_default();
                o.access_token = Some(access);
                if let Some(new_rt) = data.refresh_token {
                    o.refresh_token = Some(new_rt);
                }
                let expires_in = if data.expires_in > 0 { data.expires_in as i64 } else { 28800 };
                o.expires_at = ((now_secs + expires_in as f64) * 1000.0) as i64;
                if let Some(scope) = data.scope {
                    o.scopes = Some(scope.split(' ').map(String::from).collect());
                }
                latest.claude_ai_oauth = Some(o);

                if credentials::write_atomic(&latest).is_ok() {
                    DEAD_FAMILY_MTIME.store(0, Ordering::Relaxed);
                    eprintln!("refresh ok via {url} — fresh for {expires_in}s");
                    return true;
                }
                return false;
            }
            Err(ureq::Error::Status(code, _)) => {
                eprintln!("refresh {url} returned {code}");
                // 400 = refresh token already invalidated — don't retry the
                // fallback URL with the same dead token, and latch the family
                // as dead (keyed on the current file mtime) so subsequent
                // cycles skip refresh entirely until Claude Code rewrites the
                // credentials file. .max(1) keeps a 0-mtime edge (file
                // unreadable) from being mistaken for "not latched".
                if code == 400 {
                    DEAD_FAMILY_MTIME.store(credentials::mtime_secs().max(1), Ordering::Relaxed);
                    return false;
                }
            }
            Err(e) => eprintln!("refresh POST failed for {url}: {e}"),
        }
    }
    false
}

fn unix_now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
