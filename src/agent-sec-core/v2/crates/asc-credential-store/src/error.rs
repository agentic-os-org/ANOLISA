use std::path::PathBuf;

use crate::CredentialId;

/// Failures of credential store operations.
///
/// Messages never contain real or fake credential values.
#[derive(Debug, thiserror::Error)]
pub enum CredentialStoreError {
    /// The store directory does not exist; the gateway feature was never enabled.
    #[error("credential store is not enabled: {path} does not exist")]
    NotEnabled { path: PathBuf },
    /// A path failed the ownership, mode or file-type requirements.
    #[error("unsafe credential store path {path}: {reason}")]
    UnsafePath { path: PathBuf, reason: &'static str },
    /// An operating-system call failed.
    #[error("I/O failure at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The persisted store failed to parse or violates a store invariant.
    #[error("corrupt credential store: {0}")]
    Corrupt(String),
    /// A caller-supplied value is malformed.
    #[error("invalid {field}: {reason}")]
    Invalid {
        field: &'static str,
        reason: &'static str,
    },
    /// No credential with this identifier exists.
    #[error("unknown credential {0}")]
    UnknownCredential(CredentialId),
    /// The credential was removed; its fakes are tombstones and no new fake is issued.
    #[error("credential {0} is retired")]
    Retired(CredentialId),
    /// The real value equals another credential's real value or an issued fake.
    #[error("real value of credential {0} collides with another managed value")]
    DuplicateValue(CredentialId),
    /// A fake mapping limit would be exceeded.
    #[error("capacity exceeded: {0}")]
    Capacity(&'static str),
    /// No fake distinct from every managed value was found after retries.
    #[error("cannot generate a unique fake value")]
    FakeGeneration,
    /// The system random generator failed.
    #[error("system random generator failed")]
    Randomness,
    /// Another process holds the store open.
    #[error("credential store {path} is in use by another process")]
    Locked { path: PathBuf },
}

pub(crate) fn io_error(
    path: impl Into<PathBuf>,
    source: impl Into<std::io::Error>,
) -> CredentialStoreError {
    CredentialStoreError::Io {
        path: path.into(),
        source: source.into(),
    }
}
