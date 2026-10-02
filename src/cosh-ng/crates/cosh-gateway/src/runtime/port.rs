//! Provider-neutral control boundary for one fenced Agent Runtime binding.

use std::time::Instant;

use cosh_gateway_contracts::{
    ids::RuntimeBindingId,
    runtime::{AgentRuntimeCommand, RuntimeEventEnvelope},
};
use thiserror::Error;

/// Stable failure returned by a provider-specific Runtime adapter.
///
/// Variants intentionally omit provider frames, prompts, stderr, and SDK
/// errors. Diagnostic payloads belong in separately governed evidence.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AgentRuntimePortError {
    /// A command is not valid in the adapter's current lifecycle state.
    #[error("runtime command {operation} is invalid while state is {state}")]
    InvalidState {
        /// Rejected provider-neutral operation.
        operation: &'static str,
        /// Stable adapter state.
        state: &'static str,
    },
    /// Command lifecycle identities do not match the fenced binding.
    #[error("runtime command identity does not match the active binding")]
    IdentityMismatch,
    /// The configured workspace does not match the fenced binding.
    #[error("runtime command workspace does not match the active binding")]
    WorkspaceMismatch,
    /// The requested neutral operation is unavailable in this adapter.
    #[error("runtime adapter does not support {operation}")]
    Unsupported {
        /// Unsupported provider-neutral operation.
        operation: &'static str,
    },
    /// A deadline elapsed before the operation settled.
    #[error("runtime {operation} exceeded its deadline")]
    Deadline {
        /// Deadline-bound operation.
        operation: &'static str,
    },
    /// The provider protocol failed closed.
    #[error("runtime protocol failed validation")]
    Protocol,
    /// The supervised transport failed or closed unexpectedly.
    #[error("runtime transport failed")]
    Transport,
    /// The adapter has delivered its sole terminal event.
    #[error("runtime binding is terminal")]
    Terminal,
}

/// Provider-neutral application port implemented by Core and ACP adapters.
///
/// A port owns one process generation and one fenced binding. Callers retain
/// Task state ownership and must persist returned events before requesting
/// more output.
pub trait AgentRuntimePort: Send {
    /// Returns the COSH binding identity, never a provider session identity.
    fn binding_id(&self) -> &RuntimeBindingId;

    /// Applies one typed command before the absolute deadline.
    ///
    /// # Errors
    ///
    /// Returns a stable lifecycle, identity, deadline, protocol, or transport
    /// error. Provider payloads are never included.
    fn dispatch(
        &mut self,
        command: AgentRuntimeCommand,
        deadline: Instant,
    ) -> Result<(), AgentRuntimePortError>;

    /// Returns the next ordered public Runtime event before the deadline.
    ///
    /// # Errors
    ///
    /// Returns `Deadline` when no event arrives in time and `Terminal` after
    /// the sole terminal event has been delivered.
    fn next_event(
        &mut self,
        deadline: Instant,
    ) -> Result<RuntimeEventEnvelope, AgentRuntimePortError>;
}
