// Thin shim over state_store. Loads/saves the `config` slice of the
// combined AppState file. See state_store.rs for the actual I/O.

use crate::{models::AppConfig, state_store};

pub fn load() -> AppConfig {
    state_store::load().config
}

pub fn save(cfg: &AppConfig) -> std::io::Result<()> {
    state_store::update(|s| s.config = cfg.clone())
}
