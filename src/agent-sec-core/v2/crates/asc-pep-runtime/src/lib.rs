//! Direct sandbox execution and optional PDP-driven authorization.

pub mod linux_sandbox;
pub mod permissions;
pub mod runtime;
pub mod supervisor;

pub use linux_sandbox::{LinuxSandboxBackend, execute, prepare_launch, prepare_path_facts};
pub use permissions::PermissionInput;
pub use runtime::{
    ApprovalHandler, AuthorizationClient, DaemonAuthorizationClient, FixedPermissions, PepOutcome,
    PepRuntime, PermissionProvider, PreparedInvocation,
};

/// Enforcement backend selected by the deployment, independent of the PDP flow.
///
/// Implementations must fail closed: a restriction they cannot install exactly
/// is reported as [`PepError::Unsupported`], never approximated.
pub trait SandboxBackend {
    /// Probes the restrictions this backend can install on the current host.
    ///
    /// # Errors
    /// Returns an error when the backend is unavailable or its probe is malformed.
    fn capabilities(&self) -> Result<asc_sandbox_types::BackendCapabilities, PepError>;
    /// Installs exactly the supplied permissions and runs the pinned invocation.
    ///
    /// # Errors
    /// Rejects malformed input, changed path identities and unsupported
    /// restrictions; installed execution failures stay in the returned result.
    fn execute_bound(
        &self,
        spec: &asc_sandbox_types::ExecutionSpec,
    ) -> Result<asc_sandbox_types::ExecutionResult, PepError>;
    /// Validates backend-specific placement before approval is offered.
    ///
    /// # Errors
    /// Rejects a plan this backend cannot install exactly.
    fn preflight(&self, spec: &asc_sandbox_types::ExecutionSpec) -> Result<(), PepError> {
        let _ = spec;
        Ok(())
    }
}

/// Failures before a backend outcome is available.
#[derive(Debug, thiserror::Error)]
pub enum PepError {
    /// A permission source could not be converted into an OS permission request.
    #[error(transparent)]
    PermissionInput(#[from] permissions::PermissionInputError),
    /// Invalid or unsupported permission contract.
    #[error("invalid sandbox input: {0}")]
    Validation(#[from] asc_sandbox_types::ValidationError),
    /// Local preparation or process-control failure.
    #[error("sandbox I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Invalid request or backend wire data.
    #[error("sandbox data error: {0}")]
    Json(#[from] serde_json::Error),
    /// Backend or host lacks a mandatory capability.
    #[error("unsupported sandbox requirement: {0}")]
    Unsupported(String),
    /// Authorization or decision binding failed.
    #[error("sandbox authorization failed: {0}")]
    Authorization(String),
    /// Trusted helper broke its setup handshake.
    #[error("sandbox backend protocol failed: {0}")]
    Protocol(String),
}
