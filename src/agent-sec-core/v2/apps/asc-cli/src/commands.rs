//! Top-level command registration and request dispatch.

mod binding;
mod capabilities;
mod common;
mod policy;
mod scan_code;
mod scan_pii;
mod scope;

use asc_daemon_protocol::DaemonRequest;
use clap::Subcommand;

use self::binding::BindingCommand;
pub use self::capabilities::CapabilitiesCommand;
use self::policy::PolicyCommand;
use self::scan_code::ScanCodeCommand;
pub use self::scan_pii::PiiOutputFormat;
use self::scan_pii::ScanPiiCommand;
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
            Self::Capabilities(_) => Err(InputError::LocalCommand),
        }
    }

    pub(crate) const fn is_scan_code(&self) -> bool {
        matches!(self, Self::ScanCode(_))
    }

    pub(crate) const fn pii_format(&self) -> Option<PiiOutputFormat> {
        match self {
            Self::ScanPii(command) => Some(command.format),
            _ => None,
        }
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
