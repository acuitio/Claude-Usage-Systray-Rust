// Multi-process safe shared cooldown. Both the (future) app and collector
// read/write usage_ratelimit.json; "longest cooldown wins" merge logic.

use crate::{models::CooldownState, paths};

pub const DEFAULT_SEC: i32 = 600;
pub const SHORT_SEC: i32   = 120;
pub const MAX_SEC: i32     = 3600;

pub fn remaining_seconds() -> i32 {
    let path = paths::cooldown();
    if !path.exists() { return 0; }
    let state: CooldownState = match std::fs::read(&path) {
        Ok(b) => match serde_json::from_slice(&b) {
            Ok(s)  => s,
            Err(_) => return 0,
        },
        Err(_) => return 0,
    };
    let remain = state.cooldown_until - unix_now();
    if remain <= 0.0 { 0 } else { remain as i32 }
}

pub fn engage(retry_after: Option<&str>, default_sec: i32) -> std::io::Result<()> {
    let mut wait = default_sec;
    if let Some(s) = retry_after {
        if let Ok(parsed) = s.parse::<i32>() {
            wait = wait.max(parsed);
        }
    }
    wait = wait.min(MAX_SEC);

    let new_until = unix_now() + wait as f64;
    let existing = match std::fs::read(paths::cooldown()) {
        Ok(b) => serde_json::from_slice::<CooldownState>(&b)
            .map(|s| s.cooldown_until)
            .unwrap_or(0.0),
        Err(_) => 0.0,
    };
    let final_until = new_until.max(existing);

    let path = paths::cooldown();
    let tmp = path.with_extension("json.tmp");
    let state = CooldownState { cooldown_until: final_until };
    let json = serde_json::to_vec_pretty(&state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn format(seconds: i32) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
