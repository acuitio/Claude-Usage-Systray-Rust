// Emits the CI commit SHA and target triple as compile-time env vars so the
// in-app self-updater (src/update_service.rs) knows its own version and which
// CI artifact to pull.
//
// BUILD_SHA is taken from GITHUB_SHA, which GitHub Actions sets to the exact
// commit that triggered the workflow — the same value `gh run list` reports as
// `headSha`, so a byte-equal comparison decides "am I current?". Local builds
// (no GITHUB_SHA) get "dev", which the updater treats as "never self-update".

use std::env;

fn main() {
    let sha = env::var("GITHUB_SHA")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".to_string());
    println!("cargo:rustc-env=BUILD_SHA={sha}");

    // cargo provides TARGET to build scripts, e.g. "x86_64-pc-windows-msvc".
    let target = env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=BUILD_TARGET={target}");

    // Re-run whenever the commit changes so BUILD_SHA stays fresh even when
    // Swatinem/rust-cache restores a prior target/ dir. Declaring a rerun-if
    // directive also stops cargo from re-running the script on unrelated
    // source edits.
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
}
