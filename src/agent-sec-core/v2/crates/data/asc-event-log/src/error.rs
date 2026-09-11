//! Errors surfaced by the `JSONL` append log.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// A failure while serializing or appending one `JSONL` record.
#[derive(Debug, Error)]
pub enum EventLogError {
    /// The record could not be rendered as JSON.
    #[error("failed to serialize record: {0}")]
    Serialize(#[from] serde_json::Error),

    /// A filesystem operation failed.
    ///
    /// Only the operation name and path are reported; record payloads are never
    /// included, so a diagnostic line cannot leak event details.
    #[error("failed to {operation} {}: {source}", path.display())]
    Io {
        /// Short verb describing what was attempted, e.g. `open`.
        operation: &'static str,
        /// Path the operation targeted.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// The log path could not be resolved from configuration.
    #[error(transparent)]
    Config(#[from] asc_security_events::ConfigError),
}

impl EventLogError {
    /// Builds an [`EventLogError::Io`] for `operation` on `path`.
    pub(crate) fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }

    /// Returns the variant name, mirroring the `error_type` field v1 logs.
    #[must_use]
    pub const fn error_type(&self) -> &'static str {
        match self {
            Self::Serialize(_) => "SerializeError",
            Self::Io { .. } => "OSError",
            Self::Config(_) => "ConfigError",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_report_path_and_operation_without_payload() {
        let err = EventLogError::io(
            "open",
            Path::new("/tmp/x.jsonl"),
            io::Error::from(io::ErrorKind::PermissionDenied),
        );
        let text = err.to_string();
        assert!(text.starts_with("failed to open /tmp/x.jsonl: "), "{text}");
        assert_eq!(err.error_type(), "OSError");
    }
}
