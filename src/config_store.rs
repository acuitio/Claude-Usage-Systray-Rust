// Load/save config.json. Atomic write via temp file + rename so a crash
// mid-write never corrupts the user's settings. Same on-disk shape as
// src/Shared/ConfigStore.cs in the C# version.

use crate::{models::AppConfig, paths};

pub fn load() -> AppConfig {
    let path = paths::config();
    if !path.exists() {
        return AppConfig::default();
    }
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            eprintln!("config parse error: {e}, falling back to defaults");
            AppConfig::default()
        }),
        Err(e) => {
            eprintln!("config read error: {e}, falling back to defaults");
            AppConfig::default()
        }
    }
}

pub fn save(cfg: &AppConfig) -> std::io::Result<()> {
    let path = paths::config();
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
