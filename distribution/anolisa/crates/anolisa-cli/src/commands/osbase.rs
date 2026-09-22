mod application;

use clap::{Parser, Subcommand};

use anolisa_core::execution::ExecutionIntent;
use anolisa_core::osbase_install::{self, PhaseStatus};

use crate::context::CliContext;
use crate::response::{CliError, render_json};

use self::application::{SandboxOutcome, SandboxOutputEvent, SandboxRequest};

#[derive(Parser)]
pub struct OsbaseArgs {
    #[command(subcommand)]
    pub command: OsbaseCommands,
}

#[derive(Subcommand)]
pub enum OsbaseCommands {
    /// Kernel modules and eBPF base management
    Kernel(KernelArgs),
    /// Sandbox substrate management
    Sandbox(SandboxArgs),
    /// Security overlay management (loongshield, seccomp-profiles)
    Security(SecurityArgs),
}

// --- Kernel ---

#[derive(Parser)]
pub struct KernelArgs {
    #[command(subcommand)]
    pub command: KernelCommands,
}

#[derive(Subcommand)]
pub enum KernelCommands {
    /// Install kernel modules and eBPF programs
    Install {
        /// Request an install preview; currently fails closed without executing
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove kernel modules
    Remove,
    /// Show kernel substrate status
    Status,
}

// --- Sandbox ---

#[derive(Parser)]
pub struct SandboxArgs {
    #[command(subcommand)]
    pub command: SandboxCommands,
}

#[derive(Subcommand)]
pub enum SandboxCommands {
    /// Install a sandbox scenario (reads from sandbox.toml manifest)
    ///
    /// Runs: Preflight → Packages → Services → Verify → State
    Install {
        /// Scenario name (e.g. runc, rund, gvisor, firecracker, landlock).
        /// Run `anolisa osbase sandbox list` to see available scenarios.
        target: String,

        /// Print install plan without executing
        #[arg(long)]
        dry_run: bool,

        /// Skip confirmation prompts and non-fatal gates
        #[arg(long)]
        force: bool,

        /// Skip post-install verification
        #[arg(long)]
        no_verify: bool,
    },

    /// Uninstall scenario packages (dnf remove)
    Uninstall {
        /// Scenario name (e.g. gvisor, firecracker).
        scenario: String,

        /// Print uninstall plan without executing
        #[arg(long)]
        dry_run: bool,
    },

    /// Remove a sandbox scenario
    Remove {
        /// Scenario to remove
        target: String,

        /// Also remove config files and data directories
        #[arg(long)]
        purge: bool,

        /// Request a removal preview; currently fails closed without executing
        #[arg(long)]
        dry_run: bool,
    },

    /// List all available sandbox scenarios (from sandbox.toml manifest)
    List {
        /// Output as structured JSON
        #[arg(long)]
        json: bool,
    },

    /// Show sandbox scenario status
    Status {
        /// Specific scenario to query (omit for all)
        target: Option<String>,

        /// Output as structured JSON
        #[arg(long)]
        json: bool,
    },
}

// --- Security ---

#[derive(Parser)]
pub struct SecurityArgs {
    #[command(subcommand)]
    pub command: SecurityCommands,
}

#[derive(Subcommand)]
pub enum SecurityCommands {
    /// Install a security overlay
    Install {
        /// Target: loongshield, seccomp-profiles
        target: String,
        /// Request an install preview; currently fails closed without executing
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove a security overlay
    Remove { target: String },
    /// Show security overlay status
    Status { target: Option<String> },
}

pub fn handle(args: OsbaseArgs, ctx: &CliContext) -> Result<(), CliError> {
    match args.command {
        OsbaseCommands::Sandbox(s) => handle_sandbox(s.command, ctx),
        OsbaseCommands::Kernel(k) => {
            let command = match k.command {
                KernelCommands::Install { .. } => "osbase kernel install",
                KernelCommands::Remove => "osbase kernel remove",
                KernelCommands::Status => "osbase kernel status",
            };
            Err(CliError::not_implemented(command))
        }
        OsbaseCommands::Security(s) => {
            let command = match s.command {
                SecurityCommands::Install { target, .. } => {
                    format!("osbase security install {target}")
                }
                SecurityCommands::Remove { target } => format!("osbase security remove {target}"),
                SecurityCommands::Status { target } => match target {
                    Some(t) => format!("osbase security status {t}"),
                    None => "osbase security status".to_string(),
                },
            };
            Err(CliError::not_implemented(command))
        }
    }
}

fn handle_sandbox(command: SandboxCommands, ctx: &CliContext) -> Result<(), CliError> {
    // List only reads the manifest — no privilege or helper needed.
    if let SandboxCommands::List { json } = &command {
        return handle_sandbox_list(*json);
    }

    let request = sandbox_request(command, ctx.dry_run);
    let mut output = render_sandbox_event;
    let outcome = application::run(request, &mut output)?;
    render_sandbox_outcome(outcome)
}

fn sandbox_request(command: SandboxCommands, global_dry_run: bool) -> SandboxRequest {
    match command {
        SandboxCommands::Install {
            target,
            dry_run,
            force,
            no_verify,
        } => SandboxRequest::Install {
            target,
            intent: execution_intent(dry_run || global_dry_run),
            force,
            skip_verify: no_verify,
        },
        SandboxCommands::Uninstall { scenario, dry_run } => SandboxRequest::Uninstall {
            scenario,
            intent: execution_intent(dry_run || global_dry_run),
        },
        SandboxCommands::Remove {
            target,
            purge,
            dry_run,
        } => SandboxRequest::Remove {
            target,
            purge,
            intent: execution_intent(dry_run || global_dry_run),
        },
        SandboxCommands::List { .. } => unreachable!(),
        SandboxCommands::Status { target, .. } => SandboxRequest::Status { target },
    }
}

fn execution_intent(dry_run: bool) -> ExecutionIntent {
    if dry_run {
        ExecutionIntent::Plan
    } else {
        ExecutionIntent::Apply
    }
}

fn render_sandbox_event(event: SandboxOutputEvent) {
    match event {
        SandboxOutputEvent::Scenario(scenario) => eprintln!("[osbase] scenario: {scenario}"),
    }
}

fn render_sandbox_outcome(outcome: SandboxOutcome) -> Result<(), CliError> {
    match outcome {
        SandboxOutcome::Helper { command, outcome } => {
            for line in outcome.message.lines() {
                eprintln!("[osbase] {line}");
            }
            if outcome.exit_code == 0 || outcome.exit_code == 2 {
                if outcome.exit_code == 2 {
                    eprintln!("[osbase] install completed with degraded status (non-fatal)");
                }
                return Ok(());
            }
            Err(CliError::Runtime {
                command,
                reason: format!("install failed (exit_code={})", outcome.exit_code),
            })
        }
        SandboxOutcome::DirectInstall { command, outcome } => {
            if outcome.exit_code == 1 {
                let failed_phase = outcome
                    .phases
                    .iter()
                    .rev()
                    .find(|phase| phase.status == PhaseStatus::Failed);
                let reason = match failed_phase {
                    Some(phase) => format!(
                        "phase '{}' failed: {}",
                        phase.name,
                        phase.message.as_deref().unwrap_or("unknown error")
                    ),
                    None => "install failed".to_string(),
                };
                for warning in &outcome.warnings {
                    eprintln!("[osbase] warning: {warning}");
                }
                return Err(CliError::Runtime { command, reason });
            }
            if !outcome.warnings.is_empty() {
                eprintln!(
                    "[osbase] install completed with {} warning(s)",
                    outcome.warnings.len()
                );
            }
            for hint in &outcome.hints {
                eprintln!("[osbase] hint: {hint}");
            }
            Ok(())
        }
        SandboxOutcome::DirectUninstall => Ok(()),
    }
}

fn handle_sandbox_list(json: bool) -> Result<(), CliError> {
    match osbase_install::list_scenarios() {
        Ok(names) => {
            if json {
                let data = serde_json::json!({ "scenarios": names });
                render_json("osbase sandbox list", data)?;
            } else {
                println!("Available sandbox scenarios (from sandbox.toml):");
                println!();
                for name in &names {
                    println!("  - {name}");
                }
                println!();
                println!("Install: anolisa osbase sandbox install <scenario>");
            }
            Ok(())
        }
        Err(e) => Err(CliError::Runtime {
            command: "osbase sandbox list".to_string(),
            reason: format!("{e}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use anolisa_core::osbase_install::{
        OsbaseDomain, OsbaseInstallOutcome, PhaseResult, PhaseStatus,
    };

    use super::*;
    use crate::helper_client::HelperOperationOutcome;

    #[test]
    fn sandbox_uninstall_combines_global_and_local_dry_run() {
        for (global_dry_run, local_dry_run, expected_intent) in [
            (false, false, ExecutionIntent::Apply),
            (false, true, ExecutionIntent::Plan),
            (true, false, ExecutionIntent::Plan),
            (true, true, ExecutionIntent::Plan),
        ] {
            let request = sandbox_request(
                SandboxCommands::Uninstall {
                    scenario: "gvisor".to_string(),
                    dry_run: local_dry_run,
                },
                global_dry_run,
            );

            match request {
                SandboxRequest::Uninstall { intent, .. } => {
                    assert_eq!(intent, expected_intent);
                }
                _ => panic!("uninstall command must map to an uninstall request"),
            }
        }
    }

    #[test]
    fn sandbox_remove_combines_global_and_local_dry_run() {
        for (global_dry_run, local_dry_run, expected_intent) in [
            (false, false, ExecutionIntent::Apply),
            (false, true, ExecutionIntent::Plan),
            (true, false, ExecutionIntent::Plan),
            (true, true, ExecutionIntent::Plan),
        ] {
            let request = sandbox_request(
                SandboxCommands::Remove {
                    target: "gvisor".to_string(),
                    purge: true,
                    dry_run: local_dry_run,
                },
                global_dry_run,
            );

            match request {
                SandboxRequest::Remove { intent, .. } => {
                    assert_eq!(intent, expected_intent);
                }
                _ => panic!("remove command must map to a remove request"),
            }
        }
    }

    #[test]
    fn helper_operation_preserves_success_degraded_and_failure_codes() {
        for (exit_code, succeeds) in [(0, true), (2, true), (1, false)] {
            let result = render_sandbox_outcome(SandboxOutcome::Helper {
                command: "osbase sandbox status".to_string(),
                outcome: HelperOperationOutcome {
                    message: "phase summary".to_string(),
                    exit_code,
                },
            });

            assert_eq!(result.is_ok(), succeeds, "exit code {exit_code}");
            if let Err(CliError::Runtime { reason, .. }) = result {
                assert_eq!(reason, "install failed (exit_code=1)");
            }
        }
    }

    #[test]
    fn direct_install_preserves_success_degraded_and_phase_failure() {
        for (exit_code, phase_status, succeeds) in [
            (0, PhaseStatus::Success, true),
            (2, PhaseStatus::Degraded, true),
            (1, PhaseStatus::Failed, false),
        ] {
            let result = render_sandbox_outcome(SandboxOutcome::DirectInstall {
                command: "osbase sandbox install runc".to_string(),
                outcome: OsbaseInstallOutcome {
                    domain: OsbaseDomain::Sandbox,
                    target: "runc".to_string(),
                    phases: vec![PhaseResult {
                        name: "verify".to_string(),
                        status: phase_status,
                        message: Some("verification failed".to_string()),
                        duration_ms: None,
                    }],
                    exit_code,
                    warnings: if exit_code == 0 {
                        Vec::new()
                    } else {
                        vec!["warning".to_string()]
                    },
                    hints: vec!["hint".to_string()],
                },
            });

            assert_eq!(result.is_ok(), succeeds, "exit code {exit_code}");
            if let Err(CliError::Runtime { reason, .. }) = result {
                assert_eq!(reason, "phase 'verify' failed: verification failed");
            }
        }
    }
}
