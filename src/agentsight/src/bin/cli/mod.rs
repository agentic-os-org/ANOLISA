//! CLI subcommand modules for agentsight binary
//!
//! Linux-only subcommands: token, audit, discover, interruption,
//! metrics, skill-metrics, summary, dashboard
//! Cross-platform: serve, trace (branches internally on OS)

#[cfg(target_os = "linux")]
pub mod audit;
#[cfg(all(feature = "server", target_os = "linux"))]
pub mod dashboard;
#[cfg(target_os = "linux")]
pub mod discover;
#[cfg(target_os = "linux")]
pub mod interruption;
#[cfg(target_os = "linux")]
pub mod metrics;
#[cfg(feature = "server")]
pub mod serve;
#[cfg(target_os = "linux")]
pub mod skill_metrics;
#[cfg(target_os = "linux")]
pub mod summary;
#[cfg(target_os = "linux")]
pub mod token;
#[cfg(any(target_os = "linux", feature = "server"))]
pub mod trace;

/// Print `value` as pretty JSON to stdout, the `--json` output contract shared
/// by the machine-facing subcommands (`discover`, `token`, …).
///
/// The payloads are plain `#[derive(Serialize)]` structs over `String` / `Vec`
/// / integers with no non-string map keys and no custom `Serialize` impls, so
/// `to_string_pretty` is infallible here — we assert that invariant with
/// `expect` rather than carry an unreachable error arm.
pub fn print_json<T: serde::Serialize>(value: &T) {
    let json =
        serde_json::to_string_pretty(value).expect("agent-facing JSON payload must serialize");
    println!("{json}");
}

/// Default configuration file path (shared by trace / serve / dashboard).
#[cfg(all(feature = "server", target_os = "linux"))]
pub const DEFAULT_CONFIG_PATH: &str = "/etc/agentsight/config.json";

/// Load `ServerAuthConfig` from the agentsight config file.
///
/// Falls back to defaults if the file cannot be read or parsed.
#[cfg(all(feature = "server", target_os = "linux"))]
pub fn load_server_auth_config(config_path: &str) -> agentsight::config::ServerAuthConfig {
    load_server_config(config_path).server_auth
}

/// Loads the server configuration, falling back to safe defaults.
#[cfg(all(feature = "server", target_os = "linux"))]
pub fn load_server_config(config_path: &str) -> agentsight::config::AgentsightConfig {
    use agentsight::config::{AgentsightConfig, ensure_default_agents_config};

    let path = std::path::Path::new(config_path);
    let mut config = AgentsightConfig::new();

    if let Err(e) = ensure_default_agents_config(path) {
        log::warn!("Failed to ensure default config at {config_path:?}: {e}, using defaults");
        return config;
    }

    if let Err(e) = config.load_from_file(path) {
        log::warn!("Failed to load config from {config_path:?}: {e}, using defaults");
    }

    config
}

/// Parse period string into TimePeriod
#[cfg(target_os = "linux")]
pub fn parse_period(s: &str) -> agentsight::TimePeriod {
    match s {
        "today" => agentsight::TimePeriod::Today,
        "yesterday" => agentsight::TimePeriod::Yesterday,
        "week" => agentsight::TimePeriod::Week,
        "last_week" => agentsight::TimePeriod::LastWeek,
        "month" => agentsight::TimePeriod::Month,
        "last_month" => agentsight::TimePeriod::LastMonth,
        _ => agentsight::TimePeriod::Today,
    }
}

/// Calculate nanosecond timestamp for N hours ago
#[cfg(target_os = "linux")]
pub fn hours_ago_ns(hours: u64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    now.saturating_sub(hours * 3600 * 1_000_000_000)
}
