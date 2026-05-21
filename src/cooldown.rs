// Thin shim over state_store. Loads/saves the `cooldown` slice of the
// combined AppState file. "Longest cooldown wins" merge logic so two
// concurrent rate-limit hits don't shorten each other's wait.

use crate::{models::CooldownState, state_store};

pub const DEFAULT_SEC: i32 = 600;
pub const SHORT_SEC: i32   = 120;
pub const MAX_SEC: i32     = 3600;

pub fn remaining_seconds() -> i32 {
    let state = state_store::load();
    let remain = state.cooldown.cooldown_until - unix_now();
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
    state_store::update(|s| {
        let final_until = new_until.max(s.cooldown.cooldown_until);
        s.cooldown = CooldownState { cooldown_until: final_until };
    })
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
