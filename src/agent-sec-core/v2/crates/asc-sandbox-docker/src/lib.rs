//! Docker enforcement backend for the strict OS permission subset.
//!
//! Translates an [`asc_sandbox_types::ExecutionSpec`] into one container whose
//! placement expresses exactly the requested restrictions: a read-only pinned
//! image rootfs, explicit read-only/read-write bind mounts for allow rules,
//! network off maps to `none`, full to `host`, with capabilities dropped and
//! `no-new-privileges` enabled. Both modes block external Unix sockets, retaining
//! only local stream/seqpacket socket pairs and native `x86_64`/aarch64 ABIs.
//! Restrictions that cannot be expressed exactly (deny subtrees, mount-level
//! noexec) return [`asc_pep_runtime::PepError::Unsupported`], never an approximation.
//!
//! Contract boundaries:
//! - The operator-pinned image rootfs is the trusted base world; default-deny
//!   applies to host filesystem access, which is visible only through the
//!   authorized bind mounts.
//! - The target program must be an absolute path inside the image; the working
//!   directory must be covered by an allow rule so it exists in the container.
//! - The trusted docker CLI is invoked by absolute path with a cleared
//!   environment; the daemon socket is never exposed to the workload.
//! - A container created by this backend is removed on every completed or
//!   failed run; a SIGKILL of the supervising process can leak one container.

#![forbid(unsafe_code)]

mod config;
mod execute;
mod seccomp;
mod translate;

pub use config::DockerConfig;
pub use execute::DockerBackend;
pub use translate::{ContainerPlan, MountPlan};

/// Failures of the docker backend before or during container placement.
#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    /// The permission set exceeds the strictly expressible container subset.
    #[error("unsupported docker sandbox requirement: {0}")]
    Unsupported(String),
    /// Invalid execution input.
    #[error("invalid docker sandbox input: {0}")]
    Validation(#[from] asc_sandbox_types::ValidationError),
    /// Local process control or filesystem failure.
    #[error("docker sandbox I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Malformed docker CLI reply.
    #[error("docker sandbox data error: {0}")]
    Json(#[from] serde_json::Error),
    /// The docker CLI failed or broke the placement contract.
    #[error("docker backend protocol failed: {0}")]
    Protocol(String),
}

impl From<DockerError> for asc_pep_runtime::PepError {
    fn from(error: DockerError) -> Self {
        match error {
            DockerError::Unsupported(message) => Self::Unsupported(message),
            DockerError::Validation(error) => Self::Validation(error),
            DockerError::Io(error) => Self::Io(error),
            DockerError::Json(error) => Self::Json(error),
            DockerError::Protocol(message) => Self::Protocol(message),
        }
    }
}
