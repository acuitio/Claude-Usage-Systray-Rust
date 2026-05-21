// Load/save the most-recent Anthropic usage API response, so the UI has
// something to show on launch before the first live fetch.

use crate::{models::*, paths};

pub struct LoadedCache {
    pub data: Option<UsageResponse>,
    pub age_seconds: f64,
}

pub fn load() -> Option<LoadedCache> {
    let path = paths::cache();
    if !path.exists() { return None; }
    let bytes = std::fs::read(&path).ok()?;
    let env: CacheEnvelope = serde_json::from_slice(&bytes).ok()?;
    let age = unix_now() - env.ts;
    Some(LoadedCache { data: env.data, age_seconds: age })
}

pub fn save(data: Option<&UsageResponse>, refresh_iso: &str) -> std::io::Result<()> {
    let env = CacheEnvelope {
        ts: unix_now(),
        data: data.cloned(),
        refresh: Some(refresh_iso.to_string()),
    };
    let path = paths::cache();
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(&env)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
