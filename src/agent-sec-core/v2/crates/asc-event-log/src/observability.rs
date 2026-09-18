//! The observability `JSONL` stream wrapper.
//!
//! Migrated from v1 `observability/writer.py::ObservabilityWriter`.
//!
//! Note the asymmetry with [`crate::SecurityEventWriter`]: v1's
//! `ObservabilityWriter.write()` delegates to `write_or_raise()`, so the
//! observability `JSONL` path **raises** rather than swallowing. This is not a
//! transcription slip; it is the v1 contract and it is preserved here.

use std::path::PathBuf;

use asc_observability::ObservabilityRecord;
use asc_observability::config::{OBSERVABILITY_LOG_PREFIX, get_observability_log_path};

use crate::error::EventLogError;
use crate::jsonl::JsonlEventWriter;

/// Default rotation threshold for the observability stream (256 MB).
pub const DEFAULT_OBSERVABILITY_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Default number of rotated observability backups to keep.
pub const DEFAULT_OBSERVABILITY_BACKUP_COUNT: usize = 3;

/// Appends [`ObservabilityRecord`] values to the observability `JSONL` file.
#[derive(Debug)]
pub struct ObservabilityWriter {
    inner: JsonlEventWriter,
}

impl ObservabilityWriter {
    /// Creates a writer for an explicit path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            inner: JsonlEventWriter::new(path)
                .with_max_bytes(DEFAULT_OBSERVABILITY_MAX_BYTES)
                .with_backup_count(DEFAULT_OBSERVABILITY_BACKUP_COUNT)
                .with_error_prefix(OBSERVABILITY_LOG_PREFIX),
        }
    }

    /// Creates a writer at the resolved default observability log path.
    ///
    /// # Errors
    ///
    /// Returns [`EventLogError::Config`] when the data directory cannot be
    /// resolved.
    pub fn with_default_path() -> Result<Self, EventLogError> {
        Ok(Self::new(get_observability_log_path()?))
    }

    /// Overrides the rotation threshold.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.inner = self.inner.with_max_bytes(max_bytes);
        self
    }

    /// Overrides how many rotated backups are retained.
    #[must_use]
    pub fn with_backup_count(mut self, backup_count: usize) -> Self {
        self.inner = self.inner.with_backup_count(backup_count);
        self
    }

    /// Appends one validated observability record.
    ///
    /// Named `write` to match v1, and like v1 it surfaces failures.
    ///
    /// # Errors
    ///
    /// Propagates serialization and filesystem failures.
    pub fn write(&self, record: &ObservabilityRecord) -> Result<(), EventLogError> {
        self.inner.write_or_raise(record)
    }

    /// Returns the underlying generic writer.
    #[must_use]
    pub const fn inner(&self) -> &JsonlEventWriter {
        &self.inner
    }
}
