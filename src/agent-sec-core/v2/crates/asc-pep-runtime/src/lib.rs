//! Direct sandbox execution with explicit, fixed OS permissions.

pub mod linux_sandbox;
pub mod supervisor;

pub use linux_sandbox::{SandboxBackend, execute, prepare_launch, prepare_path_facts};

/// Failures before a backend outcome is available.
#[derive(Debug, thiserror::Error)]
pub enum PepError {
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
    /// Trusted helper broke its setup handshake.
    #[error("sandbox backend protocol failed: {0}")]
    Protocol(String),
}
