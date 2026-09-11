//! Error types for the security-event contract layer.

use thiserror::Error;

/// Failures raised while normalizing ISO-8601 timestamps.
#[derive(Debug, Error, PartialEq)]
pub enum TimestampError {
    /// Value is not an ISO-8601 timestamp.
    #[error("Invalid time format for {field}: '{value}'. Expected ISO 8601 format.")]
    InvalidFormat {
        /// Field name reported to the caller.
        field: String,
        /// Offending input, echoed for diagnosis.
        value: String,
    },
    /// Value lacks an offset and the caller refused to guess one.
    #[error("{field} must include timezone information.")]
    MissingTimezone {
        /// Field name reported to the caller.
        field: String,
    },
    /// Value carries a non-zero offset where UTC was required.
    #[error("{field} must be normalized to UTC.")]
    NotUtc {
        /// Field name reported to the caller.
        field: String,
    },
    /// Epoch seconds fall outside the representable range.
    #[error("epoch {epoch} is not a representable timestamp")]
    InvalidEpoch {
        /// Offending epoch value.
        epoch: f64,
    },
}

/// Failures raised while resolving stream names and data directories.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Stream name failed the `^[A-Za-z0-9][A-Za-z0-9_-]*$` check.
    #[error("Invalid stream name: '{0}'")]
    InvalidStreamName(String),
    /// The directory named by `AGENT_SEC_DATA_DIR` could not be prepared.
    ///
    /// Mirrors v1, where only the environment override propagates its `OSError`
    /// while the built-in tiers fall through silently.
    #[error("cannot use AGENT_SEC_DATA_DIR '{path}': {source}")]
    DataDirUnusable {
        /// Directory that could not be created or secured.
        path: String,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

/// Failures raised while building a [`crate::SecurityEvent`].
#[derive(Debug, Error)]
pub enum EventError {
    /// The supplied `timestamp` could not be normalized.
    #[error(transparent)]
    Timestamp(#[from] TimestampError),
}
