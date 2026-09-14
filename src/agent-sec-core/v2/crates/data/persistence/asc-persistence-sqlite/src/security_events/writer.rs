//! The security-event `SQLite` writer facade.
//!
//! Migrated from v1 `security_events/sqlite_writer.py`. Everything that used to
//! live in that file — the eight-step ladder, corruption recovery, the gated
//! maintenance pass — now lives in [`asc_sqlite_kernel`]. What is left is type
//! assembly and v1's defaults.

use std::path::Path;
use std::sync::Arc;

use asc_security_events::{
    ConfigError, SECURITY_EVENTS_SQLITE_SCHEMA_VERSION, SecurityEvent, config::get_db_path,
};
use asc_sqlite_kernel::{KernelError, SqliteSink, SqliteStore, current_epoch};

use crate::security_events::migration::SecurityEventsMigrator;
use crate::security_events::policy::{DropSink, SecurityEventsFaultPolicy, StderrDropSink};
use crate::security_events::repository::SecurityEventRepository;
use crate::security_events::table::SECURITY_EVENTS_TABLES;

/// v1's default retention window for this stream.
pub const DEFAULT_MAX_AGE_DAYS: u32 = 30;

/// The `log_prefix` v1 uses for this stream's store diagnostics.
pub const LOG_PREFIX: &str = "[security_events]";

/// Fire-and-forget `SQLite` writer for security events.
#[derive(Debug)]
pub struct SqliteEventWriter<S: DropSink = StderrDropSink> {
    sink: SqliteSink<SecurityEventRepository, SecurityEventsFaultPolicy<S>>,
}

impl SqliteEventWriter<StderrDropSink> {
    /// Builds a writer at `path` with v1's defaults.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] when the path cannot be normalized.
    pub fn new(path: &Path) -> Result<Self, KernelError> {
        Self::with_options(path, Some(DEFAULT_MAX_AGE_DAYS), StderrDropSink)
    }

    /// Builds a writer at the resolved default database path.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the data directory cannot be resolved, and
    /// [`KernelError`] when the store cannot be built.
    pub fn at_default_path() -> Result<Self, WriterError> {
        let path = get_db_path()?;
        Ok(Self::new(&path)?)
    }
}

impl<S: DropSink> SqliteEventWriter<S> {
    /// Builds a writer with an explicit retention window and drop sink.
    ///
    /// `max_age_days` of `None` disables pruning, matching v1's optional
    /// parameter. The write lock is always on: v1 holds a `threading.Lock` here.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] when the path cannot be normalized.
    pub fn with_options(
        path: &Path,
        max_age_days: Option<u32>,
        drop_sink: S,
    ) -> Result<Self, KernelError> {
        let store = Arc::new(SqliteStore::new(
            path,
            false,
            SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
            SECURITY_EVENTS_TABLES,
            Some(Arc::new(SecurityEventsMigrator)),
            LOG_PREFIX,
        )?);
        Ok(Self {
            sink: SqliteSink::new(
                store,
                SecurityEventRepository,
                SecurityEventsFaultPolicy::new(drop_sink),
                max_age_days,
                true,
            ),
        })
    }

    /// Returns the underlying sink, for reads and store inspection.
    #[must_use]
    pub const fn sink(&self) -> &SqliteSink<SecurityEventRepository, SecurityEventsFaultPolicy<S>> {
        &self.sink
    }

    /// Returns the database path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.sink.store().path()
    }

    /// Returns whether the store has been permanently disabled.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.sink.store().is_disabled()
    }

    /// Opens and initializes the `SQLite` store without inserting a synthetic event.
    ///
    /// # Errors
    ///
    /// Returns the schema or connection failure that prevents durable event writes.
    pub fn probe(&self) -> Result<(), KernelError> {
        self.sink
            .store()
            .with_connection(true, |_| Ok(()))?
            .ok_or(KernelError::Disabled)
    }

    /// Inserts `event`. Never fails, exactly like v1 `write()`.
    pub fn write(&self, event: &SecurityEvent) {
        self.sink.write(event);
    }

    /// Runs the gated maintenance pass and drops the connection.
    ///
    /// Uses the current wall clock; [`SqliteEventWriter::close_at`] takes an
    /// injected time for deterministic tests.
    pub fn close(&self) {
        self.close_at(current_epoch());
    }

    /// Closes the writer as of `now`.
    pub fn close_at(&self, now: f64) {
        self.sink.close(now);
    }
}

/// Failure of a default-path constructor.
#[derive(Debug, thiserror::Error)]
pub enum WriterError {
    /// The data directory or stream path could not be resolved.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The store could not be built.
    #[error(transparent)]
    Kernel(#[from] KernelError),
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use serde_json::Map;
    use tempfile::TempDir;

    fn event(id: &str) -> SecurityEvent {
        let mut event = SecurityEvent::new("sandbox_prehook", "exec", Map::new());
        id.clone_into(&mut event.event_id);
        event
    }

    #[test]
    fn the_defaults_match_v1() {
        assert_eq!(DEFAULT_MAX_AGE_DAYS, 30);

        let dir = TempDir::new().expect("temp dir");
        let writer = SqliteEventWriter::new(&dir.path().join("events.db")).expect("writer");
        assert_eq!(writer.sink().max_age_days(), Some(30));
        assert!(
            writer.sink().serializes_writes(),
            "v1 holds a threading.Lock on this stream"
        );
        assert_eq!(writer.sink().store().log_prefix(), LOG_PREFIX);
        assert!(!writer.sink().store().read_only());
    }

    #[test]
    fn a_write_creates_the_database_lazily() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        let writer = SqliteEventWriter::new(&path).expect("writer");
        assert!(!path.exists(), "construction must not touch the filesystem");

        writer.write(&event("e1"));
        assert!(path.exists());
        writer.close_at(1000.0);
        assert!(!writer.sink().store().is_open());
    }

    #[test]
    fn probe_opens_and_initializes_a_private_database() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        let writer = SqliteEventWriter::new(&path).expect("writer");

        writer.probe().expect("probe");

        assert!(path.exists());
        assert!(writer.sink().store().is_open());
        assert_eq!(
            path.metadata().expect("metadata").permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn future_schema_version_keeps_compatible_writes_available() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        let writer = SqliteEventWriter::new(&path).expect("writer");
        writer.write(&event("before-upgrade"));
        writer.close();

        let connection = rusqlite::Connection::open(&path).expect("open");
        connection
            .pragma_update(
                None,
                "user_version",
                SECURITY_EVENTS_SQLITE_SCHEMA_VERSION + 1,
            )
            .expect("mark future schema");
        drop(connection);

        let writer = SqliteEventWriter::new(&path).expect("writer");
        writer.write(&event("after-upgrade"));

        let connection = rusqlite::Connection::open(&path).expect("open");
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM security_events", [], |row| row.get(0))
            .expect("count rows");
        assert_eq!(count, 2);
    }

    #[test]
    fn close_without_a_write_leaves_no_marker() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        SqliteEventWriter::new(&path).expect("writer").close();
        assert!(!dir.path().join("events.db.maintenance").exists());
    }

    #[test]
    fn retention_can_be_disabled() {
        let dir = TempDir::new().expect("temp dir");
        let writer =
            SqliteEventWriter::with_options(&dir.path().join("events.db"), None, StderrDropSink)
                .expect("writer");
        assert_eq!(writer.sink().max_age_days(), None);
    }

    /// A failing retention pass stays silent and still advances the gate.
    ///
    /// v1's `prune` catches `SQLAlchemyError` and disposes the engine so the
    /// next write reconnects. v2 keeps its cached connection: a `rusqlite`
    /// connection remains usable after a failed statement, so there is nothing
    /// to rebuild (see D-25). What both versions share, and what this pins, is
    /// that the failure never reaches the caller and never blocks the rest of
    /// the close path.
    #[test]
    fn a_failing_retention_pass_is_swallowed_and_still_marks_the_gate() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        let writer = SqliteEventWriter::new(&path).expect("writer");
        writer.write(&event("e1"));

        // Removing the table from a second connection is the cheapest way to
        // make the `DELETE` inside the retention pass fail for real.
        let conn = rusqlite::Connection::open(&path).expect("open");
        conn.execute("DROP TABLE security_events", [])
            .expect("drop table");
        drop(conn);

        writer.close_at(1000.0);

        assert!(!writer.sink().store().is_open());
        assert!(
            dir.path().join("events.db.maintenance").exists(),
            "the gate must record the attempt even when pruning failed"
        );
    }
}
