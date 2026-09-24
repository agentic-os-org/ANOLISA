//! Top-level command registration and request dispatch.

mod binding;
mod capabilities;
mod common;
mod policy;
mod scan_code;
mod scope;
mod skill_ledger;

use asc_daemon_protocol::DaemonRequest;
use clap::Subcommand;

use self::binding::BindingCommand;
pub use self::capabilities::CapabilitiesCommand;
use self::policy::PolicyCommand;
use self::scan_code::ScanCodeCommand;
use self::scope::ScopeCommand;
use crate::InputError;

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Manage authored Policy templates.
    #[command(subcommand)]
    Policy(PolicyCommand),
    /// Manage PID or cgroup Scope selectors.
    #[command(subcommand)]
    Scope(ScopeCommand),
    /// Manage Binding desired state; acceptance does not imply enforcement.
    #[command(subcommand)]
    Binding(BindingCommand),
    /// Scan code for security issues.
    ScanCode(ScanCodeCommand),
    /// Manage Skill scanning, signatures, history and activation.
    #[command(subcommand)]
    SkillLedger(skill_ledger::SkillLedgerCommand),
    /// Show agent-sec hook capabilities from the current CLI environment variables.
    Capabilities(CapabilitiesCommand),
}

impl Command {
    pub(crate) fn request(&self) -> Result<DaemonRequest, InputError> {
        match self {
            Self::Policy(command) => command.request(),
            Self::Scope(command) => command.request(),
            Self::Binding(command) => command.request(),
            Self::ScanCode(command) => command.request(),
            Self::Capabilities(_) => Err(InputError::LocalCommand),
            Self::SkillLedger(command) => command.request(),
        }
    }

    pub(crate) const fn is_skill_sec(&self) -> bool {
        matches!(self, Self::SkillLedger(_))
    }

    pub(crate) fn after_success(&self, request: &DaemonRequest) -> Result<(), InputError> {
        if let Self::SkillLedger(command) = self {
            command.after_success(request)?;
        }
        Ok(())
    }

    pub(crate) const fn is_scan_code(&self) -> bool {
        matches!(self, Self::ScanCode(_))
    }

    /// Returns the command when it runs locally instead of through the daemon.
    ///
    /// The capability view resolves everything from the process environment, so
    /// requiring a socket for it would break hosts that never deploy a daemon.
    pub(crate) const fn local(&self) -> Option<&CapabilitiesCommand> {
        match self {
            Self::Capabilities(command) => Some(command),
            _ => None,
        }
    }
}
