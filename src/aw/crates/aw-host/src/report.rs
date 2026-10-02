//! Correlated execution facts, separate from candidate policy effects and adoption.

use aw_provider::Outcome;
use std::{process::ExitStatus, time::Duration};

/// Bounded command call, with structured and native protocols kept distinct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Discover operations and effects.
    Describe,
    /// Acknowledge the exact private configuration.
    ValidateConfig,
    /// Evaluate one admitted event step.
    Invoke,
    /// Execute an uninterpreted native callback without a Provider handshake.
    NativeHook,
}

impl Method {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Describe => "describe",
            Self::ValidateConfig => "validate_config",
            Self::Invoke => "invoke",
            Self::NativeHook => "native_hook",
        }
    }
}

/// Native process facts after verified cleanup; diagnostics are never auto-logged.
pub struct ProcessOutput {
    /// Preserves nonzero and signal termination, independent of protocol status.
    pub status: ExitStatus,
    /// Raw diagnostic bytes; embedding audit writers must redact or omit them.
    pub stderr: Vec<u8>,
    /// Captured response size; raw Provider stdout is not an audit record.
    pub stdout_bytes: usize,
    /// Bytes delivered to the pipe, not proof of Provider consumption.
    pub input_bytes_written: usize,
}

/// Metadata for a single attempted call, including failures before spawning.
pub struct CallRecord {
    /// SHA-256 of the exact configuration document used to prepare this Host.
    pub config_revision: String,
    /// Configured Agent target selected for this Host.
    pub binding_id: String,
    /// Named Provider whose retained command context was used.
    pub provider: String,
    /// Process-local unique request identifier, echoed by successful responses.
    pub request_id: String,
    /// Requested protocol method.
    pub method: Method,
    /// Host event identifier; absent during preparation.
    pub event_id: Option<String>,
    /// Configured step identifier, scoped to the event.
    pub step_id: Option<String>,
    /// Time spent encoding, executing, cleaning up and validating this attempt.
    pub elapsed: Duration,
    /// Absent when transport failed or no command was started.
    pub process: Option<ProcessOutput>,
}

/// Execution failure, never a successful Provider policy block.
#[derive(Debug, thiserror::Error)]
pub enum Failure {
    /// Includes deadline, cancellation, byte limits and unverifiable cleanup.
    #[error(transparent)]
    Transport(#[from] aw_exec::Error),
    /// A native nonzero or signal exit, checked before parsing stdout.
    #[error("Provider process did not exit successfully")]
    Exit,
    /// The process closed stdin before the entire message was written.
    #[error("Provider request was not completely written")]
    IncompleteInput,
    /// A correlated, structured Provider error response.
    #[error("Provider returned an error response")]
    Provider {
        /// Validated machine code, retained separately from diagnostic text.
        code: String,
    },
    /// Invalid request or response, including disallowed effects or wrong bindings.
    #[error(transparent)]
    Protocol(aw_provider::Error),
}

impl From<aw_provider::Error> for Failure {
    fn from(error: aw_provider::Error) -> Self {
        match error {
            aw_provider::Error::ProviderFailure { code } => Self::Provider { code },
            error => Self::Protocol(error),
        }
    }
}

/// Configured Host response to an unsuccessful invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureAction {
    /// Report the failure without introducing a block or granting permission.
    Report,
    /// Request a native block because execution failed, not because policy matched.
    Block,
}

/// Native callback bytes and status; the Adapter applies its framework's semantics.
pub struct NativeOutput {
    /// Unmodified stdout, including non-UTF-8 bytes.
    pub stdout: Vec<u8>,
    /// Unmodified stderr, independent of stdout and never automatically audited.
    pub stderr: Vec<u8>,
    /// Real exit or signal status, including native policy and diagnostic outcomes.
    pub status: ExitStatus,
}

/// Successful transport result with explicit protocol ownership.
pub enum StepOutput {
    /// Protocol-validated candidate effects; the Adapter owns adoption.
    Provider(Outcome),
    /// Native output is not interpreted as AW policy or a Provider response.
    Native(NativeOutput),
}

/// Result of one claimed event step; the embedding Adapter owns effect adoption.
pub struct Invocation {
    /// Correlated execution metadata, including the native process status when available.
    pub record: CallRecord,
    /// Explicitly typed Provider/native output or the original execution failure.
    pub result: Result<StepOutput, Failure>,
    /// Present only on failure; does not replace or hide `result`.
    pub failure_action: Option<FailureAction>,
}

/// Failed preparation call with its metadata; formatting omits Provider diagnostics.
pub struct CallFailure {
    /// Correlation and process facts for the failed call.
    pub record: CallRecord,
    /// Underlying execution or protocol failure.
    pub failure: Failure,
}

impl std::fmt::Debug for CallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallFailure")
            .field("request_id", &self.record.request_id)
            .field("method", &self.record.method)
            .field("failure", &self.failure)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for CallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Provider {} failed: {}",
            self.record.method.as_str(),
            self.failure
        )
    }
}

impl std::error::Error for CallFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.failure)
    }
}

/// Preparation failure retaining earlier successful exchanges for audit correlation.
pub struct PreparationFailure {
    /// Successful prefix in execution order; these calls are not rolled back.
    pub completed: Vec<CallRecord>,
    /// Failing call or final admission/deadline error. A call retains its own record.
    pub cause: crate::Error,
}

impl std::fmt::Debug for PreparationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparationFailure")
            .field("completed_calls", &self.completed.len())
            .field("cause", &self.cause)
            .finish()
    }
}

impl std::fmt::Display for PreparationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Provider preparation failed: {}", self.cause)
    }
}

impl std::error::Error for PreparationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}
