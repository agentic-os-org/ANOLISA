//! `SkillGuard` domain capabilities shared by daemon actions and background runs.
//! Configuration and key ownership are explicit; no user HOME lookup or Python fallback occurs.

#![forbid(unsafe_code)]

pub mod config;
mod filesystem;
pub mod identity;
pub mod integrity;
pub mod models;
pub mod scanner;

pub use config::GuardConfig;
pub use identity::SkillIdentity;
pub use integrity::{FileHashes, HashDiff, KeyStore, SigningIdentity, hash_tree};
pub use models::{DecisionAction, Finding, Manifest, ScanEntry, ScanStatus, UserDecision};

/// Domain failures, kept separate from daemon transport errors and risk findings.
#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    /// Configuration or a path cannot represent a supported Skill operation.
    #[error("invalid SkillGuard input: {0}")]
    Invalid(String),
    /// A filesystem operation failed at an explicit path.
    #[error("SkillGuard filesystem operation failed at {path}: {source}")]
    Io {
        /// Path being accessed; never contains signing key bytes.
        path: std::path::PathBuf,
        /// Original operating-system failure.
        #[source]
        source: std::io::Error,
    },
    /// Stored metadata cannot be decoded or encoded.
    #[error("invalid SkillGuard metadata: {0}")]
    Json(#[from] serde_json::Error),
    /// Stored metadata or content did not authenticate.
    #[error("SkillGuard integrity check failed: {0}")]
    Integrity(String),
    /// Cryptographic key generation or decoding failed without exposing secrets.
    #[error("SkillGuard signing key operation failed")]
    Key,
    /// A request exhausted its execution deadline before completing.
    #[error("SkillGuard execution deadline exceeded")]
    Timeout,
    /// A scanner could not initialize or finish its requested operation.
    #[error("SkillGuard scanner failed: {0}")]
    Scanner(String),
}

pub(crate) fn check_deadline(deadline: std::time::Instant) -> Result<(), GuardError> {
    if std::time::Instant::now() >= deadline {
        Err(GuardError::Timeout)
    } else {
        Ok(())
    }
}

pub(crate) fn io_error(
    path: impl Into<std::path::PathBuf>,
    source: impl Into<std::io::Error>,
) -> GuardError {
    GuardError::Io {
        path: path.into(),
        source: source.into(),
    }
}
