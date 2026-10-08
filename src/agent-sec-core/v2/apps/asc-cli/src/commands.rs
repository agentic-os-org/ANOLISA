//! Top-level command registration and request dispatch.

mod binding;
mod capabilities;
mod common;
mod policy;
mod scan_code;
mod scan_pii;
mod scan_prompt;
mod scope;
mod skill_ledger;

use asc_daemon_protocol::DaemonRequest;
use clap::Subcommand;

use self::binding::BindingCommand;
pub use self::capabilities::CapabilitiesCommand;
use self::policy::PolicyCommand;
use self::scan_code::ScanCodeCommand;
pub use self::scan_pii::PiiOutputFormat;
use self::scan_pii::ScanPiiCommand;
use self::scan_prompt::ScanPromptCommand;
pub use self::scan_prompt::{PromptScanPlan, ScanPromptInputError};
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
    /// Detect PII and credentials through the daemon.
    ScanPii(ScanPiiCommand),
    /// Scan a prompt for injection or jailbreak attempts.
    ScanPrompt(ScanPromptCommand),
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
            Self::ScanPii(command) => command.request(),
            // Scan-prompt resolves its own request batch (it may read stdin
            // or a batch file), so the single-request path refuses it.
            Self::ScanPrompt(_) => Err(InputError::PromptScanBatch),
            Self::Capabilities(_) => Err(InputError::LocalCommand),
            Self::SkillLedger(command) => command.request(),
        }
    }

    pub(crate) const fn is_skill_sec(&self) -> bool {
        matches!(self, Self::SkillLedger(_))
    }

    pub(crate) fn after_success(&self, request: &DaemonRequest, output: &mut serde_json::Value) {
        if let Self::SkillLedger(command) = self {
            command.after_success(request, output);
        }
    }

    pub(crate) fn prompt_scan_run(&self) -> Result<scan_prompt::PromptScanPlan, InputError> {
        match self {
            Self::ScanPrompt(command) => command.plan().map_err(InputError::from),
            // The plan resolves stdin and input files before any transport,
            // so only the scan-prompt command has one.
            _ => Err(InputError::LocalCommand),
        }
    }

    pub(crate) const fn is_scan_code(&self) -> bool {
        matches!(self, Self::ScanCode(_))
    }

    pub(crate) fn is_llm_code_scan(&self) -> bool {
        matches!(self, Self::ScanCode(command) if command.is_llm_mode())
    }

    pub(crate) const fn pii_format(&self) -> Option<PiiOutputFormat> {
        match self {
            Self::ScanPii(command) => Some(command.format),
            _ => None,
        }
    }

    pub(crate) const fn is_scan_prompt(&self) -> bool {
        matches!(self, Self::ScanPrompt(_))
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
