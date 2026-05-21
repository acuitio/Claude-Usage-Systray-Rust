// Read ~/.claude/.credentials.json. Write is deferred to the OAuth refresh
// phase so we don't accidentally clobber the user's real credentials here.

use crate::{models::*, paths};

pub fn read_full() -> Option<CredentialsFile> {
    std::fs::read(paths::credentials())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

pub fn read_access_token() -> Option<String> {
    read_full()?.claude_ai_oauth?.access_token
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
