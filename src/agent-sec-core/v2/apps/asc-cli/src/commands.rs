//! Top-level command registration and request dispatch.

mod binding;
mod capabilities;
mod common;
mod events;
mod events_summary;
mod gateway;
mod intent;
mod key;
mod observability;
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
pub use self::events::{EventsCommand, EventsRunError, EventsTransport};
pub(crate) use self::gateway::GatewayCommand;
pub use self::intent::IntentFailure;
pub use self::key::KeyCommand;
pub use self::policy::DefaultPolicy;
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
    /// Run a command locally with explicitly supplied sandbox permissions.
    #[command(subcommand)]
    Sandbox(crate::sandbox::SandboxCommand),
    /// Generate unapproved Cedar: daemon proposals from tasks, local init from explicit rules.
    #[command(subcommand)]
    Intent(intent::IntentCommand),
    /// Collect observability records through the daemon.
    #[command(subcommand)]
    Observability(observability::ObservabilityCommand),
    /// Manage authored Policy templates.
    #[command(subcommand)]
    Policy(PolicyCommand),
    /// Manage Scope process selectors and Policy assignments.
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
    /// List the caller's own security events from the daemon.
    Events(EventsCommand),
    /// Manage Skill scanning, signatures, history and activation.
    #[command(name = "skillsec", alias = "skill-ledger", subcommand)]
    SkillLedger(skill_ledger::SkillLedgerCommand),
    /// Show agent-sec hook capabilities from the current CLI environment variables.
    Capabilities(CapabilitiesCommand),
    /// Manage the credential gateway and query its public CA bundle.
    #[command(subcommand)]
    Gateway(GatewayCommand),
    /// Generate fake credentials locally without sending tokens to a daemon.
    #[command(subcommand)]
    Key(KeyCommand),
}

/// Approved local execution paths, independent of the daemon endpoint.
#[derive(Debug)]
pub enum LocalCommand<'a> {
    /// Environment-variable capability view.
    Capabilities(&'a CapabilitiesCommand),
    /// Local fake credential generation.
    Key(&'a KeyCommand),
}

impl LocalCommand<'_> {
    /// Executes locally; credential commands never render token values.
    ///
    /// # Errors
    /// Returns value-free file, generation or output failures.
    pub fn render(
        &self,
        env: &crate::capabilities::Environment,
        stdout: &mut impl std::io::Write,
        stderr: &mut impl std::io::Write,
    ) -> std::io::Result<u8> {
        match self {
            Self::Capabilities(command) => command.render(env, stdout, stderr),
            Self::Key(command) => command.run(),
        }
    }
}

impl Command {
    pub(crate) const fn sandbox(&self) -> Option<&crate::sandbox::SandboxCommand> {
        match self {
            Self::Sandbox(command) => Some(command),
            _ => None,
        }
    }

    pub(crate) fn run_intent(
        &self,
        socket: &std::path::Path,
        timeout: std::time::Duration,
    ) -> Option<Result<u8, IntentFailure>> {
        match self {
            Self::Intent(command) => Some(command.run(socket, timeout)),
            _ => None,
        }
    }

    pub(crate) const fn is_intent(&self) -> bool {
        matches!(self, Self::Intent(_))
    }

    pub(crate) fn run_observability_query(
        &self,
        socket: &std::path::Path,
        timeout: std::time::Duration,
    ) -> Option<std::io::Result<u8>> {
        match self {
            Self::Observability(command) => command.run_query(socket, timeout),
            _ => None,
        }
    }

    pub(crate) fn request(&self) -> Result<DaemonRequest, InputError> {
        match self {
            Self::Observability(command) => command.request(),
            Self::Policy(command) => command.request(),
            Self::Scope(command) => command.request(),
            Self::Binding(command) => command.request(),
            Self::ScanCode(command) => command.request(),
            Self::ScanPii(command) => command.request(),
            // Events resolves its own request pagination (large --limit
            // values are satisfied through bounded page requests), so the
            // single-request path refuses it.
            Self::Events(_) => Err(InputError::EventsBatch),
            Self::Intent(_) => Err(InputError::IntentProposal),
            // Scan-prompt resolves its own request batch (it may read stdin
            // or a batch file), so the single-request path refuses it.
            Self::ScanPrompt(_) => Err(InputError::PromptScanBatch),
            Self::Capabilities(_) | Self::Sandbox(_) | Self::Key(_) => {
                Err(InputError::LocalCommand)
            }
            // Gateway commands own their transport and response rendering.
            Self::Gateway(_) => Err(InputError::Gateway(
                "gateway commands run through their own transport".into(),
            )),
            Self::SkillLedger(command) => command.request(),
        }
    }

    /// Returns the gateway command when this invocation manages the secret gateway.
    pub(crate) const fn gateway(&self) -> Option<&GatewayCommand> {
        match self {
            Self::Gateway(command) => Some(command),
            _ => None,
        }
    }

    /// Returns the events command when this invocation queries events.
    pub(crate) const fn events(&self) -> Option<&EventsCommand> {
        match self {
            Self::Events(command) => Some(command),
            _ => None,
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

    pub(crate) const fn is_observability_schema(&self) -> bool {
        matches!(
            self,
            Self::Observability(observability::ObservabilityCommand::Schema)
        )
    }

    pub(crate) const fn is_observability_record(&self) -> bool {
        matches!(
            self,
            Self::Observability(observability::ObservabilityCommand::Record(_))
        )
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

    /// Returns only explicitly approved daemon-independent execution paths.
    pub(crate) const fn local(&self) -> Option<LocalCommand<'_>> {
        match self {
            Self::Capabilities(command) => Some(LocalCommand::Capabilities(command)),
            Self::Key(command) => Some(LocalCommand::Key(command)),
            _ => None,
        }
    }

    /// Returns the baseline command when it prints embedded content locally.
    pub(crate) const fn policy_default(&self) -> Option<&DefaultPolicy> {
        match self {
            Self::Policy(PolicyCommand::Default(command)) => Some(command),
            _ => None,
        }
    }
}
