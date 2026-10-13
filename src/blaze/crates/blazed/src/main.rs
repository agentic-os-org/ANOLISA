// SPDX-License-Identifier: Apache-2.0
//! `blazed` binary entry point.
//!
//! blazed is daemon-only. All sandbox management operations are
//! exposed via the HTTP API; this binary only handles daemon lifecycle.

mod api;
mod checkpoint_store;
mod cli;
mod daemon;
mod error;
#[cfg(feature = "test-failpoints")]
mod failpoint;
#[cfg(not(feature = "test-failpoints"))]
#[path = "failpoint_disabled.rs"]
mod failpoint;
mod file_provider;
mod guest;
mod metrics;
mod sandbox;
mod spawner;
mod state;
mod state_store;

use std::process::ExitCode;
use std::sync::OnceLock;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;

use crate::cli::{Cli, Command, DaemonAction};
use crate::error::Result;

/// Handle to the installed subscriber's filter, so `daemon.log_level` from
/// the config file can be applied once the config is loaded.
static LOG_FILTER_HANDLE: OnceLock<reload::Handle<EnvFilter, tracing_subscriber::Registry>> =
    OnceLock::new();

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    failpoint::announce();

    let cli = Cli::parse();
    let outcome = run(cli).await;
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("blazed: {err}");
            ExitCode::from(1)
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Daemon(action) => match action {
            DaemonAction::Start { config } => daemon::run(&config).await,
            DaemonAction::Reload { socket } => {
                println!("Sending reload signal to daemon at {}", socket.display());
                // In v0.1 just print guidance; actual signal delivery deferred.
                println!("  hint: kill -HUP $(pidof blazed)");
                Ok(())
            }
            DaemonAction::Doctor { config } => {
                let config_path = config.unwrap_or_else(|| "/etc/anolisa/blaze/config.toml".into());
                println!("blazed doctor");
                println!("  config : {}", config_path.display());
                match blaze_core::config::DaemonConfig::load(&config_path) {
                    Ok(_) => println!("  config parse : ok"),
                    Err(e) => println!("  config parse : FAIL ({e})"),
                }
                Ok(())
            }
        },
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter_layer, handle) = reload::Layer::new(filter);
    let layer = fmt::layer()
        .json()
        .with_target(true)
        .with_current_span(false);
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(layer)
        .init();
    let _ = LOG_FILTER_HANDLE.set(handle);
}

/// Apply the configured default verbosity once the daemon config has been
/// loaded. An explicit `RUST_LOG` keeps priority, so the documented env
/// override still wins over `daemon.log_level`.
pub(crate) fn apply_configured_log_level(level: &str) {
    if EnvFilter::try_from_default_env().is_ok() {
        return;
    }
    let level = normalize_log_level(level);
    if let Some(handle) = LOG_FILTER_HANDLE.get() {
        if let Err(error) = handle.reload(EnvFilter::new(level)) {
            eprintln!("blazed: could not apply daemon.log_level={level}: {error}");
        }
    }
}

fn normalize_log_level(level: &str) -> &'static str {
    match level.trim() {
        "trace" => "trace",
        "debug" => "debug",
        "info" => "info",
        "warn" => "warn",
        "error" => "error",
        other => {
            eprintln!("blazed: unsupported daemon.log_level {other:?}; falling back to \"info\"");
            "info"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_log_level;

    #[test]
    fn normalize_maps_documented_levels_and_falls_back() {
        assert_eq!(normalize_log_level("trace"), "trace");
        assert_eq!(normalize_log_level("debug"), "debug");
        assert_eq!(normalize_log_level("info"), "info");
        assert_eq!(normalize_log_level("warn"), "warn");
        assert_eq!(normalize_log_level("error"), "error");
        // Whitespace around the value is tolerated (TOML string edge case).
        assert_eq!(normalize_log_level("  debug  "), "debug");
        // Anything else falls back to the documented default instead of
        // producing an EnvFilter parse failure at swap time.
        assert_eq!(normalize_log_level("verbose"), "info");
        assert_eq!(normalize_log_level(""), "info");
    }
}
