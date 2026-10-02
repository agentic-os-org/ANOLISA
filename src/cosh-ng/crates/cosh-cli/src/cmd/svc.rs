use std::time::Instant;

use clap::Subcommand;

use cosh_platform::detect::Distro;
use cosh_platform::svc;
use cosh_platform::validate::validate_svc_name;

use crate::{build_meta, print_failure, print_success};

#[derive(Subcommand)]
pub enum SvcCommands {
    /// Show structured status of a service
    Status {
        /// Service name
        name: String,
    },
    /// Start a service
    Start {
        /// Service name
        name: String,
        /// Preview without executing
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop a service
    Stop {
        /// Service name
        name: String,
        /// Preview without executing
        #[arg(long)]
        dry_run: bool,
    },
    /// Restart a service
    Restart {
        /// Service name
        name: String,
        /// Preview without executing
        #[arg(long)]
        dry_run: bool,
    },
    /// Enable a service to start on boot
    Enable {
        /// Service name
        name: String,
        /// Preview without executing
        #[arg(long)]
        dry_run: bool,
    },
    /// Disable a service from starting on boot
    Disable {
        /// Service name
        name: String,
        /// Preview without executing
        #[arg(long)]
        dry_run: bool,
    },
    /// List services
    List {
        /// Filter by state (running, failed, etc.)
        #[arg(long)]
        state: Option<String>,
    },
}

pub fn run(action: SvcCommands, distro: &Distro, start: Instant) -> i32 {
    match action {
        SvcCommands::Status { name } => {
            if let Err(e) = validate_svc_name(&name) {
                return print_failure(e, build_meta("svc", distro, start, false));
            }
            match svc::svc_status(&name) {
                Ok(result) => print_success(result, build_meta("svc", distro, start, false)),
                Err(e) => print_failure(e, build_meta("svc", distro, start, false)),
            }
        }
        SvcCommands::Start { name, dry_run } => {
            if let Err(e) = validate_svc_name(&name) {
                return print_failure(e, build_meta("svc", distro, start, dry_run));
            }
            match svc::svc_action(&name, "start", dry_run) {
                Ok(result) => print_success(result, build_meta("svc", distro, start, dry_run)),
                Err(e) => print_failure(e, build_meta("svc", distro, start, dry_run)),
            }
        }
        SvcCommands::Stop { name, dry_run } => {
            if let Err(e) = validate_svc_name(&name) {
                return print_failure(e, build_meta("svc", distro, start, dry_run));
            }
            match svc::svc_action(&name, "stop", dry_run) {
                Ok(result) => print_success(result, build_meta("svc", distro, start, dry_run)),
                Err(e) => print_failure(e, build_meta("svc", distro, start, dry_run)),
            }
        }
        SvcCommands::Restart { name, dry_run } => {
            if let Err(e) = validate_svc_name(&name) {
                return print_failure(e, build_meta("svc", distro, start, dry_run));
            }
            match svc::svc_action(&name, "restart", dry_run) {
                Ok(result) => print_success(result, build_meta("svc", distro, start, dry_run)),
                Err(e) => print_failure(e, build_meta("svc", distro, start, dry_run)),
            }
        }
        SvcCommands::Enable { name, dry_run } => {
            if let Err(e) = validate_svc_name(&name) {
                return print_failure(e, build_meta("svc", distro, start, dry_run));
            }
            match svc::svc_action(&name, "enable", dry_run) {
                Ok(result) => print_success(result, build_meta("svc", distro, start, dry_run)),
                Err(e) => print_failure(e, build_meta("svc", distro, start, dry_run)),
            }
        }
        SvcCommands::Disable { name, dry_run } => {
            if let Err(e) = validate_svc_name(&name) {
                return print_failure(e, build_meta("svc", distro, start, dry_run));
            }
            match svc::svc_action(&name, "disable", dry_run) {
                Ok(result) => print_success(result, build_meta("svc", distro, start, dry_run)),
                Err(e) => print_failure(e, build_meta("svc", distro, start, dry_run)),
            }
        }
        SvcCommands::List { state } => match svc::svc_list(state.as_deref()) {
            Ok(result) => print_success(result, build_meta("svc", distro, start, false)),
            Err(e) => print_failure(e, build_meta("svc", distro, start, false)),
        },
    }
}
