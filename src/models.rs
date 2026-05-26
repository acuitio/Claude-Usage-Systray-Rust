// Data shapes for config, cache, history, cooldown, credentials, and
// the Anthropic usage/profile API responses. Field names are snake_case so
// the JSON layout matches src/Shared/Models.cs from the C# port exactly —
// the two implementations can coexist and share files.

use serde::{Deserialize, Serialize};

// ─── App configuration ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]                         pub start_on_startup: bool,
    #[serde(default = "default_true")]        pub auto_refresh_token: bool,
    #[serde(default = "default_scale_pct")]   pub scale_pct: i32,
    #[serde(default = "default_poll_sec")]    pub poll_interval_sec: i32,
    #[serde(default = "default_true")]        pub show_last_refresh: bool,
    #[serde(default = "default_true")]        pub show_depletion_estimates: bool,
    #[serde(default = "default_true")]        pub show_session: bool,
    #[serde(default = "default_true")]        pub show_weekly: bool,
    #[serde(default = "default_true")]        pub show_sonnet: bool,
    #[serde(default = "default_true")]        pub dashboard_on_top: bool,
    #[serde(default = "default_tray")]        pub display_mode: String,
    #[serde(default = "default_overlay_fmt")] pub overlay_format: String,
    #[serde(default = "default_opacity")]     pub overlay_opacity: i32,
    #[serde(default = "default_font")]        pub font_family: String,
    #[serde(default)]                         pub widget_x: Option<i32>,
    #[serde(default)]                         pub widget_y: Option<i32>,
    #[serde(default = "default_bg")]          pub bg_color: String,
    #[serde(default = "default_suf")]         pub color_sufficient: String,
    #[serde(default = "default_part")]        pub color_partial: String,
    #[serde(default = "default_depl")]        pub color_depleted: String,
    #[serde(default = "default_text")]        pub color_text: String,
    // Dashboard window — persisted size, position, and last-open state so
    // the next launch can restore them.
    #[serde(default)]                         pub dashboard_open: bool,
    #[serde(default)]                         pub dashboard_x: Option<i32>,
    #[serde(default)]                         pub dashboard_y: Option<i32>,
    #[serde(default)]                         pub dashboard_w: Option<i32>,
    #[serde(default)]                         pub dashboard_h: Option<i32>,
    // imgpaste — Alt+Shift+V scps the clipboard image to the remote and
    // pastes the resulting path into the focused window. See src/imgpaste.rs.
    #[serde(default = "default_true")]        pub imgpaste_enabled: bool,
    #[serde(default = "default_imgpaste_host")] pub imgpaste_host: String,
    #[serde(default = "default_imgpaste_dir")] pub imgpaste_remote_dir: String,
    #[serde(default = "default_imgpaste_mods")] pub imgpaste_hotkey_mods: u32,
    #[serde(default = "default_imgpaste_vk")]  pub imgpaste_hotkey_vk: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            start_on_startup: false,
            auto_refresh_token: true,
            scale_pct: 100,
            poll_interval_sec: 300,
            show_last_refresh: true,
            show_depletion_estimates: true,
            show_session: true,
            show_weekly: true,
            show_sonnet: true,
            dashboard_on_top: true,
            display_mode: "tray".into(),
            overlay_format: "5-hour: {session} {s_reset}  |  Weekly: {weekly} {w_reset}  |  Sonnet: {sonnet}".into(),
            overlay_opacity: 85,
            font_family: "Segoe UI".into(),
            widget_x: None,
            widget_y: None,
            bg_color: "#1e1e2e".into(),
            color_sufficient: "#64c864".into(),
            color_partial: "#e6c832".into(),
            color_depleted: "#e65050".into(),
            color_text: "#ffffff".into(),
            dashboard_open: false,
            dashboard_x: None,
            dashboard_y: None,
            dashboard_w: None,
            dashboard_h: None,
            imgpaste_enabled: true,
            imgpaste_host: "gx10".into(),
            imgpaste_remote_dir: "/tmp".into(),
            imgpaste_hotkey_mods: 0x0005,  // MOD_ALT | MOD_SHIFT
            imgpaste_hotkey_vk:   0x56,    // 'V'
        }
    }
}

// Serde `default = "fn"` requires named functions (closures don't qualify).
fn default_true()                  -> bool   { true }
fn default_scale_pct()             -> i32    { 100 }
fn default_poll_sec()              -> i32    { 300 }
fn default_tray()                  -> String { "tray".into() }
fn default_overlay_fmt()           -> String { "5-hour: {session} {s_reset}  |  Weekly: {weekly} {w_reset}  |  Sonnet: {sonnet}".into() }
fn default_opacity()               -> i32    { 85 }
fn default_font()                  -> String { "Segoe UI".into() }
fn default_bg()                    -> String { "#1e1e2e".into() }
fn default_suf()                   -> String { "#64c864".into() }
fn default_part()                  -> String { "#e6c832".into() }
fn default_depl()                  -> String { "#e65050".into() }
fn default_text()                  -> String { "#ffffff".into() }
fn default_imgpaste_host()          -> String { "gx10".into() }
fn default_imgpaste_dir()           -> String { "/tmp".into() }
fn default_imgpaste_mods()          -> u32    { 0x0005 }  // MOD_ALT | MOD_SHIFT
fn default_imgpaste_vk()            -> u32    { 0x56 }    // 'V'

// ─── Anthropic usage API response shape ──────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageResponse {
    #[serde(default)] pub five_hour:        Option<UsageMetric>,
    #[serde(default)] pub seven_day:        Option<UsageMetric>,
    #[serde(default)] pub seven_day_sonnet: Option<UsageMetric>,
    #[serde(default)] pub extra_usage:      Option<ExtraUsage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageMetric {
    #[serde(default)] pub utilization: f64,
    #[serde(default)] pub resets_at:   Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtraUsage {
    #[serde(default)] pub is_enabled:    bool,
    #[serde(default)] pub used_credits:  f64,
    #[serde(default)] pub monthly_limit: f64,
}

// ─── Credentials file (~/.claude/.credentials.json) ──────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CredentialsFile {
    #[serde(rename = "claudeAiOauth", default)]
    pub claude_ai_oauth: Option<OauthBlock>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OauthBlock {
    #[serde(default)] pub access_token:     Option<String>,
    #[serde(default)] pub refresh_token:    Option<String>,
    #[serde(default)] pub expires_at:       i64,
    #[serde(default)] pub scopes:           Option<Vec<String>>,
    #[serde(default)] pub subscription_type:Option<String>,
    #[serde(default)] pub rate_limit_tier:  Option<String>,
}

// ─── OAuth refresh + profile (used by the future HTTP layer) ─────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OauthRefreshResponse {
    #[serde(default)] pub access_token:  Option<String>,
    #[serde(default)] pub refresh_token: Option<String>,
    #[serde(default)] pub expires_in:    i32,
    #[serde(default)] pub scope:         Option<String>,
}

// ─── Cache / cooldown envelopes ──────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheEnvelope {
    #[serde(default)] pub ts:      f64,
    #[serde(default)] pub data:    Option<UsageResponse>,
    #[serde(default)] pub refresh: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CooldownState {
    #[serde(default)] pub cooldown_until: f64,
}

/// Single on-disk file combining settings, last API response, and rate-limit
/// cooldown. History is kept separate because it grows over time and can be
/// safely deleted without affecting app behaviour.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppState {
    #[serde(default)] pub config:   AppConfig,
    #[serde(default)] pub cache:    Option<CacheEnvelope>,
    #[serde(default)] pub cooldown: CooldownState,
}

// History is stored as `[[ts, sp, wp, snp], ...]` — a plain Vec<[f64; 4]>
// serializes/deserializes to exactly that shape via serde.
pub type UsageHistory = Vec<[f64; 4]>;
