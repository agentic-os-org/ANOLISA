//! Backend contract shared by the mock and ActPlane implementations.

use std::sync::mpsc::Receiver;

use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, ApplyPolicy, Binding, HealthStatus, ReplaceFailureCode, ReplaceOutcome,
    ReplacePolicy, SecurityEvent, ViolationEvent,
};
use thiserror::Error;
use uuid::Uuid;

use crate::SubscriberClass;

#[cfg(feature = "mock-backend")]
pub(crate) const STRUCTURED_FILE_PROTECTION_UNSUPPORTED: &str =
    "structured file protection policies are not implemented by this backend";

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
    /// The bound process tree already holds a writable shared mapping that no
    /// LSM hook can revoke, so content protection cannot be guaranteed.
    #[error(
        "process {pid} holds a pre-existing writable shared mapping of {path}; restart it without the mapping before applying content protection"
    )]
    PreexistingWritableMap {
        /// Process inside the requested binding tree holding the mapping.
        pid: i32,
        /// Mapped file path as reported by `/proc/<pid>/maps`.
        path: String,
    },
    /// The bound process tree already holds a writable descriptor that could
    /// be mapped shared-writable after binding without any further open.
    #[error(
        "process {pid} holds a pre-existing writable descriptor of {path}; restart it without the descriptor before applying content protection"
    )]
    PreexistingWritableFd {
        /// Process inside the requested binding tree holding the descriptor.
        pid: i32,
        /// Protected path the descriptor refers to.
        path: String,
    },
}

pub(crate) fn validate_apply_policy_payload(request: &ApplyPolicy) -> Result<(), BackendError> {
    request
        .validate_payload()
        .map_err(|error| BackendError::CompileFailure(error.to_string()))
}

/// Maps backend failures onto the replacement protocol's failure codes; both
/// backends share it so new variants need one covered mapping site.
pub(crate) fn backend_failure_code(error: &BackendError) -> ReplaceFailureCode {
    match error {
        BackendError::BindingConflict(_) | BackendError::MissingBinding(_) => {
            ReplaceFailureCode::BindingConflict
        }
        BackendError::StaleProcess { .. } => ReplaceFailureCode::StaleProcess,
        BackendError::CompileFailure(_) => ReplaceFailureCode::CompileFailure,
        BackendError::KernelFailure(_) => ReplaceFailureCode::KernelFailure,
        // Pre-attach preflight rejections are policy admission failures.
        BackendError::PreexistingWritableMap { .. } => ReplaceFailureCode::CompileFailure,
        BackendError::PreexistingWritableFd { .. } => ReplaceFailureCode::CompileFailure,
    }
}

#[cfg(feature = "mock-backend")]
pub(crate) fn structured_file_protection_unsupported() -> BackendError {
    BackendError::CompileFailure(STRUCTURED_FILE_PROTECTION_UNSUPPORTED.into())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preexisting_map_failure_maps_to_compile_failure() {
        assert_eq!(
            backend_failure_code(&BackendError::PreexistingWritableMap {
                pid: 7,
                path: "/protected/keep.txt".into(),
            }),
            ReplaceFailureCode::CompileFailure
        );
    }

    #[test]
    fn backend_failure_code_covers_every_variant() {
        let id = Uuid::new_v4();
        assert_eq!(
            backend_failure_code(&BackendError::BindingConflict(id)),
            ReplaceFailureCode::BindingConflict
        );
        assert_eq!(
            backend_failure_code(&BackendError::MissingBinding(id)),
            ReplaceFailureCode::BindingConflict
        );
        assert_eq!(
            backend_failure_code(&BackendError::StaleProcess { pid: 7 }),
            ReplaceFailureCode::StaleProcess
        );
        assert_eq!(
            backend_failure_code(&BackendError::CompileFailure("payload".into())),
            ReplaceFailureCode::CompileFailure
        );
        assert_eq!(
            backend_failure_code(&BackendError::KernelFailure("attach".into())),
            ReplaceFailureCode::KernelFailure
        );
        assert_eq!(
            backend_failure_code(&BackendError::PreexistingWritableFd {
                pid: 7,
                path: "/protected/keep.txt".into(),
            }),
            ReplaceFailureCode::CompileFailure
        );
    }
}
