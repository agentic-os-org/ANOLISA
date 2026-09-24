//! `SkillSec` domain capabilities shared by daemon actions and background runs.
//! Configuration and key ownership are explicit; no user HOME lookup or Python fallback occurs.

#![forbid(unsafe_code)]

pub mod activation;
pub mod config;
pub mod executor;
mod filesystem;
pub mod integrity;
mod ledger;
pub mod models;
pub mod scanner;
pub mod service;

pub use asc_action_types::{DecisionAction, SkillIdentity};
pub use config::SkillSecConfig;
pub use integrity::{FileHashes, HashDiff, KeyStore, SigningIdentity, hash_tree};
pub use models::{Finding, Manifest, ScanEntry, ScanStatus, UserDecision};
pub use service::{InitOptions, ScanOptions, SkillRoot, SkillSecService};

/// Domain failures, kept separate from daemon transport errors and risk findings.
#[derive(Debug, thiserror::Error)]
pub enum SkillSecError {
    /// Configuration or a path cannot represent a supported Skill operation.
    #[error("invalid SkillSec input: {0}")]
    Invalid(String),
    /// A filesystem operation failed at an explicit path.
    #[error("SkillSec filesystem operation failed at {path}: {source}")]
    Io {
        /// Path being accessed; never contains signing key bytes.
        path: std::path::PathBuf,
        /// Original operating-system failure.
        #[source]
        source: std::io::Error,
    },
    /// Stored metadata cannot be decoded or encoded.
    #[error("invalid SkillSec metadata: {0}")]
    Json(#[from] serde_json::Error),
    /// Stored metadata or content did not authenticate.
    #[error("SkillSec integrity check failed: {0}")]
    Integrity(String),
    /// Cryptographic key generation or decoding failed without exposing secrets.
    #[error("SkillSec signing key operation failed")]
    Key,
    /// The operation requires the kernel-authenticated root administrator.
    #[error("SkillSec operation requires root administrator")]
    PermissionDenied,
    /// Bounded shared-daemon admission is exhausted.
    #[error("SkillSec is busy; retry after an in-flight operation completes")]
    Busy,
    /// Rotation must finish before ordinary ledger access resumes.
    #[error("SkillSec key rotation is pending; administrator must resume rotation")]
    RotationPending,
    /// A request exhausted its execution deadline before completing.
    #[error("SkillSec execution deadline exceeded")]
    Timeout,
    /// A scanner could not initialize or finish its requested operation.
    #[error("SkillSec scanner failed: {0}")]
    Scanner(String),
}

pub(crate) fn check_deadline(deadline: std::time::Instant) -> Result<(), SkillSecError> {
    if std::time::Instant::now() >= deadline {
        Err(SkillSecError::Timeout)
    } else {
        Ok(())
    }
}

pub(crate) fn io_error(
    path: impl Into<std::path::PathBuf>,
    source: impl Into<std::io::Error>,
) -> SkillSecError {
    SkillSecError::Io {
        path: path.into(),
        source: source.into(),
    }
}

impl From<asc_action_types::SkillSecInputError> for SkillSecError {
    fn from(error: asc_action_types::SkillSecInputError) -> Self {
        match error {
            asc_action_types::SkillSecInputError::Invalid(message) => Self::Invalid(message),
        }
    }
}
