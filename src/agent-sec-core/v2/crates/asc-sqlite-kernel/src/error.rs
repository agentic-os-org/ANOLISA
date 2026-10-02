//! Kernel error type and the `SQLite` error classifiers.
//!
//! The three classifiers are `pub` on purpose. In v1 they are underscore-private
//! in `security_events/orm_store.py` yet imported across packages by
//! `observability/sqlite_writer.py`; promoting them to real API is what lets the
//! compiler enforce the layering instead of convention.

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Mask that turns an extended result code into its primary code.
const PRIMARY_CODE_MASK: i32 = 0xFF;

/// `SQLITE_BUSY`.
const SQLITE_BUSY: i32 = 5;
/// `SQLITE_LOCKED`.
const SQLITE_LOCKED: i32 = 6;
/// `SQLITE_CORRUPT`.
const SQLITE_CORRUPT: i32 = 11;
/// `SQLITE_SCHEMA`.
const SQLITE_SCHEMA: i32 = 17;
/// `SQLITE_NOTADB`.
const SQLITE_NOTADB: i32 = 26;

/// Message fragments that mark a schema drift repairable by convergence.
///
/// Copied verbatim from v1 `_SQLITE_SCHEMA_ERROR_MARKERS`.
pub const SCHEMA_ERROR_MARKERS: &[&str] = &[
    "database schema has changed",
    "has no column named",
    "no such column",
    "no such table",
];

/// Failures raised by the kernel.
#[derive(Debug, Error)]
pub enum KernelError {
    /// An error reported by `SQLite` itself.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A filesystem operation around the database failed.
    #[error("failed to {operation} {}: {source}", path.display())]
    Io {
        /// Short verb describing the attempted operation.
        operation: &'static str,
        /// Path the operation targeted.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// A column name in a [`crate::schema::TableSpec`] failed validation.
    ///
    /// Wording matches v1, which raises
    /// `ValueError(f"Invalid column name in schema: {col!r}")`. Column names are
    /// interpolated into `ALTER TABLE` text, so this is a security boundary, not
    /// decoration.
    #[error("Invalid column name in schema: '{0}'")]
    InvalidColumnName(String),

    /// No table was supplied for schema initialization.
    ///
    /// Replaces v1 `_require_models`, which rejects an empty model tuple. v2 has
    /// no process-global default model registry, so the rejection moved to the
    /// table slice.
    #[error(
        "No tables supplied for SQLite schema initialization; \
         pass a non-empty TableSpec slice"
    )]
    EmptySchema,

    /// The store is permanently disabled after failed corruption cleanup.
    #[error("sqlite store is disabled")]
    Disabled,

    /// The record was rejected before `SQLite` was touched.
    ///
    /// Stands in for v1's `ValueError` / `TypeError`, which signal a caller bug
    /// rather than an I/O fault and therefore must never tear down the
    /// connection.
    #[error("malformed record: {0}")]
    Malformed(String),
}

impl KernelError {
    /// Builds a [`KernelError::Io`] for `operation` on `path`.
    pub fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }

    /// Returns the primary `SQLite` result code, if this wraps one.
    #[must_use]
    pub fn primary_code(&self) -> Option<i32> {
        match self {
            Self::Sqlite(rusqlite::Error::SqliteFailure(err, _)) => {
                Some(err.extended_code & PRIMARY_CODE_MASK)
            }
            _ => None,
        }
    }

    /// Returns the variant name, mirroring the `error_type` v1 logs.
    #[must_use]
    pub const fn error_type(&self) -> &'static str {
        match self {
            Self::Sqlite(_) => "DatabaseError",
            Self::Io { .. } => "OSError",
            Self::InvalidColumnName(_) | Self::Malformed(_) => "ValueError",
            Self::EmptySchema => "EmptySchemaError",
            Self::Disabled => "StoreDisabled",
        }
    }
}

/// Returns whether `err` indicates true database corruption.
///
/// Mirrors v1 `_is_sqlite_corruption_error`.
#[must_use]
pub fn is_corruption(err: &KernelError) -> bool {
    matches!(err.primary_code(), Some(SQLITE_CORRUPT | SQLITE_NOTADB))
}

/// Returns whether `err` is a busy/locked error surviving the busy timeout.
///
/// Mirrors v1 `_is_sqlite_busy_error`.
#[must_use]
pub fn is_busy(err: &KernelError) -> bool {
    matches!(err.primary_code(), Some(SQLITE_BUSY | SQLITE_LOCKED))
}

/// Returns whether `err` is repairable by schema convergence.
///
/// Mirrors v1 `_is_sqlite_schema_error`: the primary code check first, then a
/// case-insensitive message scan for [`SCHEMA_ERROR_MARKERS`].
#[must_use]
pub fn is_schema(err: &KernelError) -> bool {
    if err.primary_code() == Some(SQLITE_SCHEMA) {
        return true;
    }
    let message = err.to_string().to_lowercase();
    SCHEMA_ERROR_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_failure(extended_code: i32, message: &str) -> KernelError {
        KernelError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(extended_code),
            Some(message.to_owned()),
        ))
    }

    #[test]
    fn corruption_codes_match_v1() {
        assert!(is_corruption(&sqlite_failure(SQLITE_CORRUPT, "malformed")));
        assert!(is_corruption(&sqlite_failure(SQLITE_NOTADB, "not a db")));
        // Extended codes must be masked down to the primary code.
        assert!(is_corruption(&sqlite_failure(
            SQLITE_CORRUPT | (1 << 8),
            "corrupt index"
        )));
        assert!(!is_corruption(&sqlite_failure(SQLITE_BUSY, "busy")));
    }

    #[test]
    fn busy_codes_match_v1() {
        assert!(is_busy(&sqlite_failure(SQLITE_BUSY, "busy")));
        assert!(is_busy(&sqlite_failure(SQLITE_LOCKED, "locked")));
        assert!(is_busy(&sqlite_failure(SQLITE_BUSY | (2 << 8), "snapshot")));
        assert!(!is_busy(&sqlite_failure(SQLITE_CORRUPT, "malformed")));
    }

    #[test]
    fn schema_detection_uses_code_then_markers() {
        assert!(is_schema(&sqlite_failure(SQLITE_SCHEMA, "whatever")));
        for marker in SCHEMA_ERROR_MARKERS {
            let shouted = marker.to_uppercase();
            assert!(
                is_schema(&sqlite_failure(1, &shouted)),
                "marker {marker} must match case-insensitively"
            );
        }
        assert!(!is_schema(&sqlite_failure(1, "syntax error")));
    }

    #[test]
    fn non_sqlite_errors_have_no_primary_code() {
        let err = KernelError::io(
            "open",
            Path::new("/tmp/x.db"),
            io::Error::from(io::ErrorKind::NotFound),
        );
        assert_eq!(err.primary_code(), None);
        assert!(!is_corruption(&err));
        assert!(!is_busy(&err));
        assert!(!is_schema(&err));
        assert_eq!(err.error_type(), "OSError");
    }

    #[test]
    fn invalid_column_message_matches_v1() {
        let err = KernelError::InvalidColumnName("run_id2".to_owned());
        assert_eq!(err.to_string(), "Invalid column name in schema: 'run_id2'");
    }
}
