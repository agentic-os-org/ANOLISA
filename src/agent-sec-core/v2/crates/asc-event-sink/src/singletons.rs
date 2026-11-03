//! Lazily built, resettable process-wide security-event sinks.
//!
//! Migrated from the module-level globals of v1 `security_events/__init__.py`. Each slot is a `RwLock<Option<Arc<T>>>` rather
//! than a `OnceLock`: v1's globals are plain module variables that
//! `tests/unit-test/conftest.py` sets back to `None` to force a rebuild, and a
//! `OnceLock` cannot express that. Construction stays lazy — nothing here touches
//! the filesystem until the first accessor call.

use std::sync::{Arc, PoisonError, RwLock};

use asc_event_log::SecurityEventWriter;
use asc_persistence_sqlite::security_events::{SqliteEventReader, SqliteEventWriter};

use crate::error::SinkError;

/// A process-wide slot that builds its value on first use and can be cleared.
#[derive(Debug)]
struct Slot<T> {
    cell: RwLock<Option<Arc<T>>>,
}

impl<T> Slot<T> {
    const fn new() -> Self {
        Self {
            cell: RwLock::new(None),
        }
    }

    /// Returns the value, building it with `build` if the slot is empty.
    ///
    /// A failed `build` leaves the slot empty so the next call retries, matching
    /// v1 where the global is only assigned after the constructor returns.
    fn get_or_init(
        &self,
        build: impl FnOnce() -> Result<T, SinkError>,
    ) -> Result<Arc<T>, SinkError> {
        if let Some(value) = self.read().as_ref() {
            return Ok(Arc::clone(value));
        }
        let mut guard = self.cell.write().unwrap_or_else(PoisonError::into_inner);
        // Another thread may have won the race between the two locks.
        if let Some(value) = guard.as_ref() {
            return Ok(Arc::clone(value));
        }
        let value = Arc::new(build()?);
        *guard = Some(Arc::clone(&value));
        Ok(value)
    }

    /// Returns the value only if it has already been built.
    fn peek(&self) -> Option<Arc<T>> {
        self.read().as_ref().map(Arc::clone)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Option<Arc<T>>> {
        self.cell.read().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(any(test, feature = "testing"))]
    fn install(&self, value: T) -> Arc<T> {
        let value = Arc::new(value);
        let mut guard = self.cell.write().unwrap_or_else(PoisonError::into_inner);
        *guard = Some(Arc::clone(&value));
        value
    }

    #[cfg(any(test, feature = "testing"))]
    fn clear(&self) {
        let mut guard = self.cell.write().unwrap_or_else(PoisonError::into_inner);
        *guard = None;
    }
}

static SECURITY_JSONL: Slot<SecurityEventWriter> = Slot::new();
static SECURITY_SQLITE: Slot<SqliteEventWriter> = Slot::new();
static SECURITY_READER: Slot<SqliteEventReader> = Slot::new();

/// Returns the process-wide security-event `JSONL` writer.
///
/// v1: `security_events.get_writer()`.
///
/// # Errors
///
/// Returns [`SinkError`] when the log path cannot be resolved.
pub fn writer() -> Result<Arc<SecurityEventWriter>, SinkError> {
    SECURITY_JSONL.get_or_init(|| Ok(SecurityEventWriter::with_default_path()?))
}

/// Returns the process-wide security-event `SQLite` writer.
///
/// v1: `security_events.get_sqlite_writer()`. v1 registers `atexit` here; see
/// [`crate::shutdown`] for why v2 cannot and what replaces it.
///
/// # Errors
///
/// Returns [`SinkError`] when the database path cannot be resolved.
pub fn sqlite_writer() -> Result<Arc<SqliteEventWriter>, SinkError> {
    SECURITY_SQLITE.get_or_init(|| Ok(SqliteEventWriter::at_default_path()?))
}

/// Returns the process-wide security-event `SQLite` reader.
///
/// v1: `security_events.get_reader()`.
///
/// # Errors
///
/// Returns [`SinkError`] when the database path cannot be resolved.
pub fn reader() -> Result<Arc<SqliteEventReader>, SinkError> {
    SECURITY_READER.get_or_init(|| Ok(SqliteEventReader::at_default_path()?))
}

/// Returns the security-event `SQLite` writer only if it was already built.
///
/// Shutdown must not be the thing that creates a database.
#[must_use]
pub fn initialized_sqlite_writer() -> Option<Arc<SqliteEventWriter>> {
    SECURITY_SQLITE.peek()
}

/// Clears every slot so the next accessor rebuilds it.
///
/// This is v1's `conftest.py` resetting the module globals to `None`. It does not
/// close anything: an `Arc` a caller still holds stays usable.
#[cfg(any(test, feature = "testing"))]
pub fn reset_sinks_for_test() {
    SECURITY_JSONL.clear();
    SECURITY_SQLITE.clear();
    SECURITY_READER.clear();
}

/// Installs a security-event `JSONL` writer, replacing any current one.
///
/// The default paths come from `AGENT_SEC_DATA_DIR` and `HOME`, and mutating the
/// environment is `unsafe` in this edition while the workspace forbids `unsafe`.
/// Injection is therefore the only way a test can point the sinks at a temporary
/// directory.
#[cfg(any(test, feature = "testing"))]
pub fn install_writer_for_test(value: SecurityEventWriter) -> Arc<SecurityEventWriter> {
    SECURITY_JSONL.install(value)
}

/// Installs a security-event `SQLite` writer, replacing any current one.
#[cfg(any(test, feature = "testing"))]
pub fn install_sqlite_writer_for_test(value: SqliteEventWriter) -> Arc<SqliteEventWriter> {
    SECURITY_SQLITE.install(value)
}

/// Installs a security-event `SQLite` reader, replacing any current one.
#[cfg(any(test, feature = "testing"))]
pub fn install_reader_for_test(value: SqliteEventReader) -> Arc<SqliteEventReader> {
    SECURITY_READER.install(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{event, serial, temp_dir};

    #[test]
    fn an_untouched_slot_reports_nothing_initialized() {
        let _guard = serial();
        reset_sinks_for_test();

        assert!(initialized_sqlite_writer().is_none());
    }

    #[test]
    fn repeated_access_returns_the_same_instance() {
        let _guard = serial();
        let dir = temp_dir();

        let first = install_sqlite_writer_for_test(
            SqliteEventWriter::new(&dir.path().join("events.db")).expect("writer"),
        );
        let second = initialized_sqlite_writer().expect("initialized");
        assert!(
            Arc::ptr_eq(&first, &second),
            "the slot must hand out one instance, not a rebuild"
        );

        reset_sinks_for_test();
        assert!(initialized_sqlite_writer().is_none());
    }

    #[test]
    fn a_reset_slot_forgets_its_value_without_closing_it() {
        let _guard = serial();
        let dir = temp_dir();
        let path = dir.path().join("events.db");

        let held = install_sqlite_writer_for_test(SqliteEventWriter::new(&path).expect("writer"));
        reset_sinks_for_test();

        // The handle a caller already took stays usable; only the slot forgot it.
        held.write(&event("e-1"));
        held.close_at(1000.0);
        assert!(path.exists());
    }
}
