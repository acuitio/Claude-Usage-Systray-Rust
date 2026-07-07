// Read/write ~/.claude/.credentials.json. Atomic write via tempfile+rename
// so a crash mid-refresh never corrupts the user's auth state.

use crate::{models::*, paths};

pub fn read_full() -> Option<CredentialsFile> {
    std::fs::read(paths::credentials())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

pub fn read_access_token() -> Option<String> {
    read_full()?.claude_ai_oauth?.access_token
}

/// Unix mtime (secs) of the credentials file; 0 if missing/unreadable.
/// Used to detect "Claude Code wrote a new token" — the trigger for both the
/// poll loop's fast auth-recovery and clearing the dead-refresh-family latch.
pub fn mtime_secs() -> u64 {
    std::fs::metadata(paths::credentials())
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn write_atomic(creds: &CredentialsFile) -> std::io::Result<()> {
    let path = paths::credentials();
    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(creds)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Friendly plan label for the tray tooltip / dashboard header. Mirrors the
/// C# CredentialsStore.GetPlanLabel logic.
pub fn plan_label(creds: Option<&CredentialsFile>) -> String {
    let oauth = creds.and_then(|c| c.claude_ai_oauth.as_ref());
    let tier  = oauth.and_then(|o| o.rate_limit_tier.as_deref()).unwrap_or("");
    let sub   = oauth.and_then(|o| o.subscription_type.as_deref()).unwrap_or("");

    if tier.contains("max_20x") { return "Max 20x".into(); }
    if tier.contains("max_5x")  { return "Max 5x".into();  }
    match sub {
        "max"  => "Max".into(),
        "pro"  => "Pro".into(),
        "free" => "Free".into(),
        ""     => "Unknown".into(),
        other  => {
            let mut chars = other.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None    => "Unknown".into(),
            }
        }
    }
}
