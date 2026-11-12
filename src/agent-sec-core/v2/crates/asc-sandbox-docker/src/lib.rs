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
//! - Linux mounts require host root, `CAP_SYS_ADMIN` and `open_tree`/`move_mount`.
//!   Reject symlinks at every level and verify FD identities before private root-owned staging.
//!   Workspaces remain live; replacing a source path cannot change a pinned object. Fail closed.
//! - The target program must be an absolute path inside the image; the working
//!   directory must be covered by an allow rule so it exists in the container.
//! - The trusted Docker CLI uses an absolute path, cleared environment and per-run empty config.
//!   Local Unix endpoints require ownership, peer identity and staging visibility checks.
//!   All calls pin endpoint/config; personal contexts, proxies, credentials and auto-pull are unused.
//! - A container created by this backend is removed on every completed or
//!   failed run; a SIGKILL of the supervising process can leak one container.

#![forbid(unsafe_code)]

mod client;
mod config;
mod execute;
#[cfg(target_os = "linux")]
mod mounts;
mod seccomp;
mod translate;

pub use config::DockerConfig;
pub use execute::DockerBackend;
pub use translate::{ContainerPlan, MountPlan};

#[cfg(test)]
#[path = "../tests/docker_backend.rs"]
mod backend_tests;

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
