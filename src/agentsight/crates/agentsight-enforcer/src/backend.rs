//! Backend contract shared by the mock and ActPlane implementations.

use std::sync::mpsc::Receiver;

use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, ApplyPolicy, Binding, HealthStatus, ReplaceOutcome, ReplacePolicy,
    SecurityEvent, ViolationEvent,
};
use thiserror::Error;
use uuid::Uuid;

use crate::SubscriberClass;

/// Failures produced while managing a policy binding.
#[derive(Debug, Error)]
pub enum BackendError {
    /// A binding ID was reused with different desired state.
    #[error("binding {0} conflicts with active desired state")]
    BindingConflict(Uuid),
    /// The requested binding does not exist.
    #[error("binding {0} does not exist")]
    MissingBinding(Uuid),
    /// PID start time no longer matches the apply request.
    #[error("process {pid} is stale or has been reused")]
    StaleProcess {
        /// PID rejected by the backend.
        pid: i32,
    },
    /// ActPlane rejected the supplied policy source.
    #[error("policy compilation failed: {0}")]
    CompileFailure(String),
    /// Kernel attachment or runtime state management failed.
    #[error("kernel enforcement failed: {0}")]
    KernelFailure(String),
}

/// Privileged policy lifecycle and violation source.
pub trait EnforcementBackend: Send + Sync + 'static {
    /// Reports whether the backend can accept policy operations.
    ///
    /// # Errors
    ///
    /// Returns a backend error when readiness cannot be determined.
    fn health(&self) -> Result<HealthStatus, BackendError>;

    /// Applies desired policy state idempotently.
    ///
    /// # Errors
    ///
    /// Returns a typed conflict, process, compile, or kernel error.
    fn apply(&self, request: ApplyPolicy) -> Result<Binding, BackendError>;

    /// Compiles and applies a product-level credential-exfiltration policy.
    ///
    /// # Errors
    ///
    /// Returns a validation, compile, stale-process, or kernel error.
    fn apply_credential_policy(
        &self,
        request: ApplyCredentialPolicy,
    ) -> Result<Binding, BackendError>;

    /// Replaces one exact active binding under the backend lifecycle lock.
    ///
    /// # Errors
    ///
    /// Returns a backend error only when the replacement result cannot be
    /// represented safely as a typed [`ReplaceOutcome`].
    fn replace(&self, request: ReplacePolicy) -> Result<ReplaceOutcome, BackendError>;

    /// Detaches one binding.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::MissingBinding`] when the ID is unknown or a
    /// kernel error when detachment cannot be acknowledged.
    fn detach(&self, binding_id: Uuid) -> Result<(), BackendError>;

    /// Lists the backend's current bindings.
    ///
    /// # Errors
    ///
    /// Returns a backend error when state cannot be read safely.
    fn bindings(&self) -> Result<Vec<Binding>, BackendError>;

    /// Creates an identified bounded violation subscription.
    fn subscribe(&self, id: Uuid, class: SubscriberClass) -> Receiver<ViolationEvent>;

    /// Removes a subscription during normal connection lifecycle cleanup.
    fn unsubscribe(&self, id: Uuid);

    /// Records events that reached the required queue but not its remote peer.
    fn record_required_delivery_loss(&self, count: u64);

    /// Creates an independent bounded normalized security-event subscription.
    fn subscribe_security_events(&self) -> Receiver<SecurityEvent>;

    /// Records normalized events accepted locally but lost at the remote peer.
    fn record_security_delivery_loss(&self, count: u64);

    /// Detaches every active binding before the daemon exits.
    ///
    /// # Errors
    ///
    /// Returns the first lifecycle or kernel cleanup failure.
    fn shutdown(&self) -> Result<(), BackendError> {
        for binding in self.bindings()? {
            self.detach(binding.request.binding_id)?;
        }
        Ok(())
    }
}
