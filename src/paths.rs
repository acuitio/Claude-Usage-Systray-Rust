// File location helpers. Mirrors src/Shared/Paths.cs in the C# version, so
// both ports read/write the same files and can be swapped freely.

use std::path::PathBuf;

/// The directory the running executable lives in.
pub fn app_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config()        -> PathBuf { app_dir().join("config.json") }
pub fn cache()         -> PathBuf { app_dir().join("usage_cache.json") }
pub fn history()       -> PathBuf { app_dir().join("usage_history.json") }
pub fn cooldown()      -> PathBuf { app_dir().join("usage_ratelimit.json") }

/// `~/.claude/.credentials.json`. Absolute, NOT relative to the exe.
pub fn credentials() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude").join(".credentials.json")
}
