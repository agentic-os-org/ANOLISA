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
    sqlite_path: PathBuf,
    jsonl: SecurityEventWriter,
    sqlite: Slot<SqliteEventWriter>,
}

impl ConfiguredSecurityEventSinks {
    /// Creates explicit-path sinks without touching the filesystem.
    #[must_use]
    pub fn new(jsonl_path: PathBuf, sqlite_path: PathBuf) -> Self {
        Self {
            sqlite_path,
            jsonl: SecurityEventWriter::new(jsonl_path),
            sqlite: Slot::new(),
        }
    }

    /// Prepares the JSONL file at the configured path.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the configured path cannot be prepared.
    pub fn warm_jsonl(&self) -> Result<(), SinkError> {
        self.jsonl.probe()?;
        Ok(())
    }

    /// Builds the `SQLite` writer at the configured path.
    ///
    /// # Errors
    ///
    /// Returns a construction error if the configured path cannot be prepared.
    pub fn warm_sqlite(&self) -> Result<(), SinkError> {
        self.sqlite_writer()?.probe()?;
        Ok(())
    }

    /// Dual-writes one event while isolating the two persistence paths.
    pub fn log_event(&self, event: &SecurityEvent) {
        let jsonl = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.jsonl.write(event);
        }));
        if jsonl.is_err() {
            tracing::warn!(target: "asc_process_diagnostic", "security_event_jsonl_callback_failed");
        }
        let sqlite = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match self.sqlite_writer() {
                Ok(writer) => writer.write(event),
                Err(_) => {
                    tracing::warn!(target: "asc_process_diagnostic", "security_event_sqlite_initialization_failed");
                }
            }
        }));
        if sqlite.is_err() {
            tracing::warn!(target: "asc_process_diagnostic", "security_event_sqlite_callback_failed");
        }
    }

    /// Runs maintenance and closes the configured `SQLite` writer if initialized.
    pub fn close(&self) {
        if let Some(writer) = self.sqlite.peek() {
            writer.close();
        }
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
    fn warm_initializes_both_explicit_destinations_without_events() {
        let dir = tempfile::tempdir().expect("temp dir");
        let jsonl = dir.path().join("events.jsonl");
        let sqlite = dir.path().join("events.db");
        let sinks = ConfiguredSecurityEventSinks::new(jsonl.clone(), sqlite.clone());

        assert!(!jsonl.exists());
        assert!(!sqlite.exists());
        sinks.warm_jsonl().expect("warm jsonl");
        sinks.warm_sqlite().expect("warm sqlite");

        assert!(jsonl.exists());
        assert!(sqlite.exists());
        assert_eq!(
            fs::read_to_string(&jsonl).expect("jsonl").lines().count(),
            0
        );
        sinks.log_event(&SecurityEvent::new("code_scan", "code_scan", Map::new()));
        assert_eq!(fs::read_to_string(jsonl).expect("jsonl").lines().count(), 1);
    }

    #[test]
    fn warm_failures_are_isolated_by_destination() {
        let dir = tempfile::tempdir().expect("temp dir");
        let blocked = dir.path().join("blocked");
        fs::write(&blocked, b"blocked").expect("block path");
        let sqlite = dir.path().join("events.db");
        let sinks = ConfiguredSecurityEventSinks::new(blocked.join("events.jsonl"), sqlite.clone());

        assert!(sinks.warm_jsonl().is_err());
        sinks.warm_sqlite().expect("warm independent sqlite");
        assert!(sqlite.exists());

        fs::remove_file(&blocked).expect("unblock path");
        sinks.log_event(&SecurityEvent::new("code_scan", "code_scan", Map::new()));
        assert_eq!(
            fs::read_to_string(blocked.join("events.jsonl"))
                .expect("recovered jsonl")
                .lines()
                .count(),
            1
        );
        fs::remove_dir_all(&blocked).expect("remove recovered directory");
        fs::write(&blocked, b"blocked").expect("block path again");

        let jsonl = dir.path().join("events.jsonl");
        let sinks = ConfiguredSecurityEventSinks::new(jsonl.clone(), blocked.join("events.db"));
        sinks.warm_jsonl().expect("warm independent jsonl");
        assert!(sinks.warm_sqlite().is_err());
        assert!(jsonl.exists());
    }
}
