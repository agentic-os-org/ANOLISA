// SPDX-License-Identifier: Apache-2.0
//! Blaze daemon runtime and build-time provider composition.

mod api;
mod checkpoint_store;
mod cli;
mod daemon;
mod data_plane;
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

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use blaze_provider_api::{DataPlaneProvider, ProviderDescriptor};
use clap::Parser;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::cli::{Cli, Command, DaemonAction};
use crate::error::Result;

const BUILD_TIME_PROVIDER_STATE_PREFIX: &str = ".provider-state-v";

/// Derive the lifecycle-state root for one build-time data-plane provider.
///
/// The returned directory is a direct, non-UUID child of the configured
/// `daemon.state_dir`. Its name contains the provider contract revision and
/// canonical provider instance UUID. Only that provider selects these records
/// for management. Other providers and the standard daemon inspect them
/// read-only and reject startup if cleanup cannot be proved complete.
pub(crate) fn build_time_provider_state_dir(
    configured_state_dir: &Path,
    descriptor: ProviderDescriptor,
) -> PathBuf {
    configured_state_dir.join(build_time_provider_state_namespace(descriptor))
}

fn build_time_provider_state_namespace(descriptor: ProviderDescriptor) -> String {
    format!(
        "{BUILD_TIME_PROVIDER_STATE_PREFIX}{}-{}",
        descriptor.contract_version, descriptor.provider_instance_id
    )
}

fn parse_build_time_provider_state_namespace(name: &str) -> Option<ProviderDescriptor> {
    let suffix = name.strip_prefix(BUILD_TIME_PROVIDER_STATE_PREFIX)?;
    let (contract_version, provider_instance_id_text) = suffix.split_once('-')?;
    let contract_version = contract_version.parse().ok()?;
    let provider_instance_id: uuid::Uuid = provider_instance_id_text.parse().ok()?;
    if provider_instance_id.to_string() != provider_instance_id_text {
        return None;
    }
    Some(ProviderDescriptor {
        contract_version,
        provider_instance_id,
    })
}

/// Composition root for one data-plane provider compiled with the daemon.
///
/// This is source-level composition, not a stable dynamic-library interface.
/// The provider and `blazed` must use compatible source revisions and one
/// dependency lock when the final binary is built.
pub struct BlazeDaemonBuilder<P> {
    provider: Arc<P>,
}

impl<P: DataPlaneProvider + 'static> BlazeDaemonBuilder<P> {
    /// Select the only primary data-plane provider in this binary.
    pub fn new(provider: P) -> Self {
        Self {
            provider: Arc::new(provider),
        }
    }

    /// Run the daemon with the build-time provider and standard daemon config.
    ///
    /// Provider selection is fixed here. Tenant requests, configuration
    /// values, and filesystem plugin locations cannot replace it at runtime.
    /// An unsuccessful provider probe stops startup without falling back to
    /// the standard file provider. Lifecycle records use the documented
    /// provider-specific child of `daemon.state_dir`; the configured directory
    /// remains the cross-daemon coordination root.
    pub async fn run(self, config_path: &Path) -> anyhow::Result<()> {
        daemon::run_with_provider(config_path, self.provider)
            .await
            .map_err(anyhow::Error::from)
    }
}

/// Install the daemon's structured logging subscriber if none is installed.
pub fn initialize_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let layer = fmt::layer()
        .json()
        .with_target(true)
        .with_current_span(false);
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .try_init();
}

/// Execute the standard command line with the built-in file provider.
pub async fn main_entry() -> ExitCode {
    initialize_tracing();
    failpoint::announce();

    let cli = Cli::parse();
    match run_cli(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("blazed: {error}");
            ExitCode::from(1)
        }
    }
}

async fn run_cli(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Daemon(action) => match action {
            DaemonAction::Start { config } => daemon::run(&config).await,
            DaemonAction::Reload { socket } => {
                println!("Sending reload signal to daemon at {}", socket.display());
                println!("  hint: kill -HUP $(pidof blazed)");
                Ok(())
            }
            DaemonAction::Doctor { config } => {
                let config_path = config.unwrap_or_else(|| "/etc/anolisa/blaze/config.toml".into());
                println!("blazed doctor");
                println!("  config : {}", config_path.display());
                match blaze_core::config::DaemonConfig::load(&config_path) {
                    Ok(_) => println!("  config parse : ok"),
                    Err(error) => println!("  config parse : FAIL ({error})"),
                }
                Ok(())
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use blaze_provider_api::PROVIDER_CONTRACT_VERSION;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn provider_state_directory_rule_is_stable_and_non_uuid() {
        let configured = Path::new("/var/lib/blaze");
        let descriptor = ProviderDescriptor {
            contract_version: PROVIDER_CONTRACT_VERSION,
            provider_instance_id: Uuid::parse_str("87eec43e-310a-4691-a992-55081736be51")
                .expect("provider identity"),
        };

        let derived = build_time_provider_state_dir(configured, descriptor);

        assert_eq!(
            derived,
            configured.join(".provider-state-v1-87eec43e-310a-4691-a992-55081736be51")
        );
        let name = derived
            .file_name()
            .and_then(|name| name.to_str())
            .expect("namespace name");
        assert!(Uuid::parse_str(name).is_err());
        assert_eq!(
            parse_build_time_provider_state_namespace(name),
            Some(descriptor)
        );
    }

    #[test]
    fn provider_identity_and_contract_revision_select_distinct_directories() {
        let configured = Path::new("/srv/blaze-state");
        let first = ProviderDescriptor {
            contract_version: 1,
            provider_instance_id: Uuid::parse_str("87eec43e-310a-4691-a992-55081736be51")
                .expect("provider identity"),
        };
        let another_identity = ProviderDescriptor {
            provider_instance_id: Uuid::parse_str("69ab7aa3-78ef-475a-8770-9af556c5ed35")
                .expect("other provider identity"),
            ..first
        };
        let another_revision = ProviderDescriptor {
            contract_version: 2,
            ..first
        };

        assert_ne!(
            build_time_provider_state_dir(configured, first),
            build_time_provider_state_dir(configured, another_identity)
        );
        assert_ne!(
            build_time_provider_state_dir(configured, first),
            build_time_provider_state_dir(configured, another_revision)
        );
    }
}
