//! Process bootstrap and composition root for the `AgentSecCore` V2 daemon.
//!
//! The binary installs PAP handlers with a root-managed authorization policy
//! and a replaceable process-local PAP Repository. Scan lifecycle output uses
//! explicitly configured durable audit sinks and independent telemetry.

#![forbid(unsafe_code)]

mod actions;
mod bootstrap;
pub use actions::{scan_application, skill_application, skill_task_scope};
mod cli;
mod reconciliation;
mod runtime;
mod signals;

pub use bootstrap::{BootstrapConfig, BootstrapError, default_service_config, serve};
pub use cli::{Cli, CliError, ParseOutcome};
pub use reconciliation::{
    UnavailableReconciliation, start_policy_reconciliation, start_policy_reconciliation_with_client,
};
pub use runtime::{RuntimeError, run_with_shutdown_timeout};
pub use signals::{ProcessSignals, SignalError};

/// Authenticated `SkillFS` integration owned by the daemon process.
pub mod skillfs;
