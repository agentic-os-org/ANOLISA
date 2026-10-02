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

use clap::Parser;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::cli::{Cli, Command, DaemonAction};
use crate::error::Result;

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
    let layer = fmt::layer()
        .json()
        .with_target(true)
        .with_current_span(false);
    tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .init();
}
