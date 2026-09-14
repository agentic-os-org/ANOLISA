//! Explicit-path security-event sinks for daemon composition roots.

use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use asc_event_log::SecurityEventWriter;
use asc_persistence_sqlite::security_events::SqliteEventWriter;
use asc_security_events::SecurityEvent;

use crate::SinkError;

/// Process-local lazily initialized value with an explicitly supplied path.
#[derive(Debug)]
struct Slot<T> {
    value: RwLock<Option<Arc<T>>>,
}

impl<T> Slot<T> {
    const fn new() -> Self {
        Self {
            value: RwLock::new(None),
        }
    }

    fn get_or_try_init(
        &self,
        build: impl FnOnce() -> Result<T, SinkError>,
    ) -> Result<Arc<T>, SinkError> {
        if let Some(value) = self.peek() {
            return Ok(value);
        }
        let mut guard = self.value.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(value) = guard.as_ref() {
            return Ok(Arc::clone(value));
        }
        let value = Arc::new(build()?);
        *guard = Some(Arc::clone(&value));
        Ok(value)
    }

    fn peek(&self) -> Option<Arc<T>> {
        self.value
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(Arc::clone)
    }
}

/// Explicit-path dual-write security-event sinks.
///
/// The daemon owns these paths and never falls back to process environment
/// resolution. `JSONL` and `SQLite` initialization remain independent as in v1.
#[derive(Debug)]
pub struct ConfiguredSecurityEventSinks {
    jsonl_path: PathBuf,
    sqlite_path: PathBuf,
    jsonl: Slot<SecurityEventWriter>,
    sqlite: Slot<SqliteEventWriter>,
}

impl ConfiguredSecurityEventSinks {
    /// Creates explicit-path sinks without touching the filesystem.
    #[must_use]
    pub fn new(jsonl_path: PathBuf, sqlite_path: PathBuf) -> Self {
        Self {
            jsonl_path,
            sqlite_path,
            jsonl: Slot::new(),
            sqlite: Slot::new(),
        }
    }

    /// Builds the JSONL writer at the configured path.
    ///
    /// # Errors
    ///
    /// Returns a construction error if the configured path cannot be prepared.
    pub fn warm_jsonl(&self) -> Result<(), SinkError> {
        self.jsonl_writer().map(|_| ())
    }

    /// Builds the `SQLite` writer at the configured path.
    ///
    /// # Errors
    ///
    /// Returns a construction error if the configured path cannot be prepared.
    pub fn warm_sqlite(&self) -> Result<(), SinkError> {
        self.sqlite_writer().map(|_| ())
    }

    /// Dual-writes one event while isolating the two persistence paths.
    pub fn log_event(&self, event: &SecurityEvent) {
        if let Ok(writer) = self.jsonl_writer() {
            writer.write(event);
        }
        if let Ok(writer) = self.sqlite_writer() {
            writer.write(event);
        }
    }

    /// Runs maintenance and closes the configured `SQLite` writer if initialized.
    pub fn close(&self) {
        if let Some(writer) = self.sqlite.peek() {
            writer.close();
        }
    }

    fn jsonl_writer(&self) -> Result<Arc<SecurityEventWriter>, SinkError> {
        self.jsonl
            .get_or_try_init(|| Ok(SecurityEventWriter::new(&self.jsonl_path)))
    }

    fn sqlite_writer(&self) -> Result<Arc<SqliteEventWriter>, SinkError> {
        self.sqlite
            .get_or_try_init(|| Ok(SqliteEventWriter::new(&self.sqlite_path)?))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Map;

    use super::*;

    #[test]
    fn uses_explicit_paths_without_creating_an_uninitialized_database() {
        let dir = tempfile::tempdir().expect("temp dir");
        let jsonl = dir.path().join("events.jsonl");
        let sqlite = dir.path().join("events.db");
        let sinks = ConfiguredSecurityEventSinks::new(jsonl.clone(), sqlite.clone());
        sinks.warm_jsonl().expect("warm jsonl");
        sinks.close();

        assert!(!jsonl.exists());
        assert!(!sqlite.exists());
        sinks.log_event(&SecurityEvent::new("code_scan", "code_scan", Map::new()));
        assert!(jsonl.exists());
        assert!(sqlite.exists());
        assert_eq!(fs::read_to_string(jsonl).expect("jsonl").lines().count(), 1);
    }
}
