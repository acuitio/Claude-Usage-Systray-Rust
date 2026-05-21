// Thin shim over state_store. Loads/saves the `cache` slice of the
// combined AppState file.

use crate::{models::*, state_store};

pub struct LoadedCache {
    pub data: Option<UsageResponse>,
    pub age_seconds: f64,
}

pub fn load() -> Option<LoadedCache> {
    let env = state_store::load().cache?;
    let age = unix_now() - env.ts;
    Some(LoadedCache { data: env.data, age_seconds: age })
}

pub fn save(data: Option<&UsageResponse>, refresh_iso: &str) -> std::io::Result<()> {
    let env = CacheEnvelope {
        ts: unix_now(),
        data: data.cloned(),
        refresh: Some(refresh_iso.to_string()),
    };
    state_store::update(|s| s.cache = Some(env))
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
