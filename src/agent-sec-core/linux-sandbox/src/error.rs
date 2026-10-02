//! error types for linux-sandbox.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, SandboxError>;

#[derive(Error, Debug)]
pub enum SandboxError {
    /// An operation is not supported in the current context.
    #[error("unsupported operation: {0}")]
    UnsupportedOperation(String),

    /// Error from linux seccomp filter setup.
    #[cfg(target_os = "linux")]
    #[error("seccomp setup error")]
    SeccompInstall(#[from] seccompiler::Error),

    /// Error from linux seccomp backend.
    #[cfg(target_os = "linux")]
    #[error("seccomp backend error")]
    SeccompBackend(#[from] seccompiler::BackendError),

    /// Wrapping std::io::Error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
