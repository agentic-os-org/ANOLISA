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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_start_requires_config_long_or_short_flag() {
        let cli = Cli::try_parse_from(["blazed", "daemon", "start", "--config", "/etc/t.toml"])
            .expect("long flag parses");
        match cli.command {
            Command::Daemon(DaemonAction::Start { config }) => {
                assert_eq!(config, PathBuf::from("/etc/t.toml"));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let cli = Cli::try_parse_from(["blazed", "daemon", "start", "-c", "/etc/t.toml"])
            .expect("short flag parses");
        match cli.command {
            Command::Daemon(DaemonAction::Start { config }) => {
                assert_eq!(config, PathBuf::from("/etc/t.toml"));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        assert!(
            Cli::try_parse_from(["blazed", "daemon", "start"]).is_err(),
            "start without --config must be rejected"
        );
    }

    #[test]
    fn daemon_reload_defaults_socket_and_accepts_override() {
        let cli =
            Cli::try_parse_from(["blazed", "daemon", "reload"]).expect("reload without socket");
        match cli.command {
            Command::Daemon(DaemonAction::Reload { socket }) => {
                assert_eq!(socket, PathBuf::from("/run/blaze/api.sock"));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let cli = Cli::try_parse_from(["blazed", "daemon", "reload", "--socket", "/tmp/s.sock"])
            .expect("reload with socket");
        match cli.command {
            Command::Daemon(DaemonAction::Reload { socket }) => {
                assert_eq!(socket, PathBuf::from("/tmp/s.sock"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn daemon_doctor_config_is_optional() {
        let cli =
            Cli::try_parse_from(["blazed", "daemon", "doctor"]).expect("doctor without config");
        match cli.command {
            Command::Daemon(DaemonAction::Doctor { config }) => assert_eq!(config, None),
            other => panic!("unexpected command: {other:?}"),
        }

        let cli = Cli::try_parse_from(["blazed", "daemon", "doctor", "--config", "/etc/t.toml"])
            .expect("doctor with config");
        match cli.command {
            Command::Daemon(DaemonAction::Doctor { config }) => {
                assert_eq!(config, Some(PathBuf::from("/etc/t.toml")));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn subcommand_is_mandatory_and_validated() {
        assert!(
            Cli::try_parse_from(["blazed"]).is_err(),
            "missing subcommand must be rejected"
        );
        assert!(
            Cli::try_parse_from(["blazed", "sandbox", "list"]).is_err(),
            "sandbox management does not belong to the lifecycle CLI"
        );
        assert!(
            Cli::try_parse_from(["blazed", "daemon", "frobnicate"]).is_err(),
            "unknown daemon action must be rejected"
        );
    }
}
