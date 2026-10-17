#![forbid(unsafe_code)]
#![allow(
    clippy::result_large_err,
    reason = "crate APIs share public CoshError; per-function allows obscure one API trade-off"
)]
//! cosh CLI — Computable Operating System Harness.
//!
//! Deterministic OS capability interface for Agents.

use std::time::Instant;

use clap::{Parser, Subcommand};

mod cmd;

use cosh_platform::detect::Distro;
use cosh_types::output::{CoshResponse, ResponseMeta};

#[derive(Parser)]
#[command(
    name = "cosh-cli",
    version,
    about = "Computable Operating System Harness — deterministic OS capability interface"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(name = "__login-shell-access", hide = true)]
    LoginShellAccess(cmd::login_shell::AccessProbeArgs),
    /// Explicit login-shell registration and local account selection (Linux)
    LoginShell(cmd::login_shell::LoginShellArgs),
    /// Package management (cross-distro: dnf/apt/zypper)
    Pkg {
        #[command(subcommand)]
        action: cmd::pkg::PkgCommands,
    },
    /// Service management (systemd)
    Svc {
        #[command(subcommand)]
        action: cmd::svc::SvcCommands,
    },
    /// Workspace checkpoints (ws-ckpt integration)
    Checkpoint {
        #[command(subcommand)]
        action: cmd::checkpoint::CheckpointCommands,
    },
    /// Security audit
    Audit {
        #[command(subcommand)]
        action: cmd::audit::AuditCommands,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Commands::LoginShellAccess(args) = &cli.command {
        std::process::exit(cmd::login_shell::run_access_probe(args));
    }
    // Initialize tracing (stderr-only, controlled by COSH_LOG or RUST_LOG)
    let filter = std::env::var("COSH_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&filter)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();

    let distro = Distro::detect();
    let start = Instant::now();

    let exit_code = match cli.command {
        // The probe returns above, before any tracing initialization.
        Commands::LoginShellAccess(_) => unreachable!("access probe already exited"),
        Commands::LoginShell(args) => cmd::login_shell::run(args, &distro, start),
        Commands::Pkg { action } => cmd::pkg::run(action, &distro, start),
        Commands::Svc { action } => cmd::svc::run(action, &distro, start),
        Commands::Checkpoint { action } => cmd::checkpoint::run(action, &distro, start),
        Commands::Audit { action } => cmd::audit::run(action, &distro, start),
    };

    std::process::exit(exit_code);
}

/// Print a successful CoshResponse as JSON and return exit code 0.
pub fn print_success<T: serde::Serialize>(data: T, meta: ResponseMeta) -> i32 {
    let resp = CoshResponse::success(data, meta);
    match serde_json::to_string_pretty(&resp) {
        Ok(json) => println!("{}", json),
        Err(e) => {
            eprintln!("{{\"ok\":false,\"error\":\"serialization failed: {}\"}}", e);
            return 1;
        }
    }
    0
}

/// Print a failure CoshResponse as JSON and return exit code 1.
pub fn print_failure(error: cosh_types::error::CoshError, meta: ResponseMeta) -> i32 {
    let resp: CoshResponse<()> = CoshResponse::failure(error, meta);
    match serde_json::to_string_pretty(&resp) {
        Ok(json) => println!("{}", json),
        Err(e) => {
            eprintln!("{{\"ok\":false,\"error\":\"serialization failed: {}\"}}", e);
        }
    }
    1
}

/// Build ResponseMeta from common parameters.
pub fn build_meta(subsystem: &str, distro: &Distro, start: Instant, dry_run: bool) -> ResponseMeta {
    ResponseMeta {
        subsystem: subsystem.to_string(),
        duration_ms: start.elapsed().as_millis() as u64,
        distro: Some(distro.id_str().to_string()),
        dry_run,
        warning: None,
    }
}

/// Build ResponseMeta with a warning message.
pub fn build_meta_with_warning(
    subsystem: &str,
    distro: &Distro,
    start: Instant,
    dry_run: bool,
    warning: &str,
) -> ResponseMeta {
    ResponseMeta {
        subsystem: subsystem.to_string(),
        duration_ms: start.elapsed().as_millis() as u64,
        distro: Some(distro.id_str().to_string()),
        dry_run,
        warning: Some(warning.to_string()),
    }
}
