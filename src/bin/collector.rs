// Headless background collector. Polls the Anthropic usage API on the
// configured interval and writes the same usage_cache.json /
// usage_history.json that the tray app reads. Mirrors the C# Collector
// project in src/Collector/.
//
// Designed to be spawned by the tray app when config.background_collection
// is true, but can also be run standalone. Lives in src/bin/ so cargo
// builds it as a separate binary (`ClaudeUsageCollector.exe`).
//
// Shares source files with the tray app via `#[path = "..."]` to avoid
// duplicating the data-layer modules. No UI, no Win32 dependency — just
// HTTP + JSON + the registered file shapes.

// Re-export the shared modules from src/ — each is compiled fresh into
// this binary's crate, but the files themselves stay single-sourced.
#[path = "../models.rs"]        mod models;
#[path = "../paths.rs"]         mod paths;
#[path = "../config_store.rs"]  mod config_store;
#[path = "../usage_cache.rs"]   mod usage_cache;
#[path = "../usage_history.rs"] mod usage_history;
#[path = "../cooldown.rs"]      mod cooldown;
#[path = "../credentials.rs"]   mod credentials;
#[path = "../oauth_refresh.rs"] mod oauth_refresh;
#[path = "../usage_fetcher.rs"] mod usage_fetcher;

use std::thread;
use std::time::Duration;

fn main() {
    eprintln!("ClaudeUsageCollector starting (PID {})", std::process::id());
    write_pid_file();

    let http = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build();

    loop {
        let cfg = config_store::load();
        if !cfg.background_collection {
            eprintln!("background_collection disabled in config — exiting");
            break;
        }

        match usage_fetcher::fetch(&http, false) {
            usage_fetcher::FetchOutcome::Refreshed =>
                eprintln!("collector: refreshed cache + history"),
            usage_fetcher::FetchOutcome::CacheHit =>
                eprintln!("collector: cache still warm; skipped HTTP"),
            usage_fetcher::FetchOutcome::Cooldown { remaining_seconds } =>
                eprintln!("collector: cooldown — {remaining_seconds}s remaining"),
            usage_fetcher::FetchOutcome::Failed { detail } =>
                eprintln!("collector: fetch failed — {detail}"),
        }

        // collector_interval_sec; clamp to a minimum of 60s so a bad config
        // doesn't melt the API.
        let interval = cfg.collector_interval_sec.max(60);
        thread::sleep(Duration::from_secs(interval as u64));
    }

    remove_pid_file();
}

fn write_pid_file() {
    let path = paths::collector_pid();
    let _ = std::fs::write(&path, std::process::id().to_string());
}

fn remove_pid_file() {
    let _ = std::fs::remove_file(paths::collector_pid());
}
