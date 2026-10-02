// SPDX-License-Identifier: Apache-2.0
//! Minimal CLI for the `blazed` daemon binary.
//!
//! blazed is a daemon-first design: all sandbox management is done
//! via the HTTP API. This CLI only provides daemon lifecycle commands.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "blazed",
    version,
    about = "ANOLISA per-host sandbox daemon",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Daemon lifecycle management.
    #[command(subcommand)]
    Daemon(DaemonAction),
}

#[derive(Subcommand, Debug)]
pub enum DaemonAction {
    /// Start the daemon in foreground mode.
    Start {
        /// Path to config.toml.
        #[arg(long, short)]
        config: PathBuf,
    },
    /// Signal a running daemon to reload policies (equivalent to SIGHUP).
    Reload {
        /// UDS socket path of the running daemon.
        #[arg(long, default_value = "/run/blaze/api.sock")]
        socket: PathBuf,
    },
    /// Run local diagnostics (config paths, socket reachability).
    Doctor {
        /// Path to config.toml (optional, uses default if absent).
        #[arg(long, short)]
        config: Option<PathBuf>,
    },
}
