//! The security-events `JSONL` stream wrapper.
//!
//! Migrated from v1 `security_events/writer.py::SecurityEventWriter`.

use std::path::PathBuf;

use asc_security_events::SecurityEvent;
use asc_security_events::config::get_log_path;

use crate::error::EventLogError;
use crate::jsonl::{
    DEFAULT_BACKUP_COUNT, DEFAULT_ERROR_PREFIX, DEFAULT_MAX_BYTES, JsonlEventWriter,
};

/// Appends [`SecurityEvent`] records to the security-events `JSONL` file.
#[derive(Debug)]
pub struct SecurityEventWriter {
    inner: JsonlEventWriter,
}

impl SecurityEventWriter {
    /// Creates a writer for an explicit path.
    ///
    /// Tests and hosts that manage their own data directory use this form; the
    /// default-path constructor is [`SecurityEventWriter::with_default_path`].
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            inner: JsonlEventWriter::new(path)
                .with_max_bytes(DEFAULT_MAX_BYTES)
                .with_backup_count(DEFAULT_BACKUP_COUNT)
                .with_error_prefix(DEFAULT_ERROR_PREFIX)
                .with_error_handler(Box::new(report_write_failure)),
        }
    }

    /// Creates a writer at the resolved default security-events log path.
    ///
    /// # Errors
    ///
    /// Returns [`EventLogError::Config`] when the data directory cannot be
    /// resolved.
    pub fn with_default_path() -> Result<Self, EventLogError> {
        Ok(Self::new(get_log_path()?))
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

    /// Appends one event, swallowing every failure.
    pub fn write(&self, event: &SecurityEvent) {
        self.inner.write(event);
    }

    /// Appends one event, surfacing failures.
    ///
    /// # Errors
    ///
    /// Propagates serialization and filesystem failures.
    pub fn write_or_raise(&self, event: &SecurityEvent) -> Result<(), EventLogError> {
        self.inner.write_or_raise(event)
    }

    /// Creates and opens the target without appending a synthetic event.
    ///
    /// # Errors
    ///
    /// Propagates filesystem failures while preparing the private event log.
    pub fn probe(&self) -> Result<(), EventLogError> {
        self.inner.probe()
    }

    /// Returns the underlying generic writer.
    #[must_use]
    pub const fn inner(&self) -> &JsonlEventWriter {
        &self.inner
    }
}

/// Reports a swallowed security-events write failure on stderr.
///
/// v1 routes this through the `agent_sec_cli` logger tree into `cli.jsonl`.
/// That diagnostic stream is a separate v1 module which is not part of this
/// migration, so v2 emits one prefixed line on stderr instead. Only the error
/// type and message are printed, never the record, so a failing write cannot
/// leak event details.
fn report_write_failure(err: &EventLogError) {
    eprintln!(
        "{DEFAULT_ERROR_PREFIX} security events JSONL write failed: {}: {err}",
        err.error_type()
    );
}
