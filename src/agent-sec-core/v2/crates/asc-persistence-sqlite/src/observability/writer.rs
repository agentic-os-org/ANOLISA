//! The observability `SQLite` writer facade.
//!
//! Migrated from v1 `observability/sqlite_writer.py`. This stream keeps two
//! entry points: [`ObservabilitySqliteWriter::write`] swallows everything, while
//! [`ObservabilitySqliteWriter::write_or_raise`] surfaces the fault so a
//! foreground ingestion failure is visible to its caller.

use std::path::Path;
use std::sync::Arc;

use asc_observability::{
    DEFAULT_OBSERVABILITY_RETENTION_DAYS, OBSERVABILITY_LOG_PREFIX,
    OBSERVABILITY_SQLITE_SCHEMA_VERSION, ObservabilityRecord, config::get_observability_db_path,
};
use asc_security_events::ConfigError;
use asc_sqlite_kernel::{KernelError, SqliteSink, SqliteStore, current_epoch};

use crate::observability::policy::ObservabilityFaultPolicy;
use crate::observability::repository::ObservabilityEventRepository;
use crate::observability::table::OBSERVABILITY_TABLES;

/// `SQLite` index writer for observability records.
#[derive(Debug)]
pub struct ObservabilitySqliteWriter {
    sink: SqliteSink<ObservabilityEventRepository, ObservabilityFaultPolicy>,
}

impl ObservabilitySqliteWriter {
    /// Builds a writer at `path` with v1's defaults.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] when the path cannot be normalized.
    pub fn new(path: &Path) -> Result<Self, KernelError> {
        Self::with_max_age_days(path, Some(DEFAULT_OBSERVABILITY_RETENTION_DAYS))
    }

    /// Builds a writer at the resolved default database path.
    ///
    /// # Errors
    ///
    /// Returns [`ObservabilityWriterError`] when the data directory cannot be
    /// resolved or the store cannot be built.
    pub fn at_default_path() -> Result<Self, ObservabilityWriterError> {
        let path = get_observability_db_path()?;
        Ok(Self::new(&path)?)
    }

    /// Builds a writer with an explicit retention window.
    ///
    /// No migrator is passed and no write lock is taken — both match v1: this
    /// stream is still at schema revision 1 and holds no `threading.Lock`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] when the path cannot be normalized.
    pub fn with_max_age_days(path: &Path, max_age_days: Option<u32>) -> Result<Self, KernelError> {
        let store = Arc::new(SqliteStore::new(
            path,
            false,
            OBSERVABILITY_SQLITE_SCHEMA_VERSION,
            OBSERVABILITY_TABLES,
            None,
            OBSERVABILITY_LOG_PREFIX,
        )?);
        Ok(Self {
            sink: SqliteSink::new(
                store,
                ObservabilityEventRepository,
                ObservabilityFaultPolicy,
                max_age_days,
                false,
            ),
        })
    }

    /// Returns the underlying sink.
    #[must_use]
    pub const fn sink(
        &self,
    ) -> &SqliteSink<ObservabilityEventRepository, ObservabilityFaultPolicy> {
        &self.sink
    }

    /// Returns the database path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.sink.store().path()
    }

    /// Inserts `record`, swallowing every fault.
    pub fn write(&self, record: &ObservabilityRecord) {
        self.sink.write(record);
    }

    /// Inserts `record`, surfacing ingestion failures.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] carrying one of v1's three `OSError` messages, or
    /// the original error for a malformed record and for schema drift.
    pub fn write_or_raise(&self, record: &ObservabilityRecord) -> Result<(), KernelError> {
        self.sink.write_or_raise(record)
    }

    /// Runs the gated maintenance pass and drops the connection.
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
pub enum ObservabilityWriterError {
    /// The data directory or stream path could not be resolved.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The store could not be built.
    #[error(transparent)]
    Kernel(#[from] KernelError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_observability::{ObservabilityHook, ObservabilityMetadata};
    use chrono::{FixedOffset, TimeZone};
    use serde_json::{Map, json};
    use tempfile::TempDir;

    fn record() -> ObservabilityRecord {
        let hook = ObservabilityHook::BeforeAgentRun;
        let mut metrics = Map::new();
        metrics.insert(hook.metric_names()[0].to_owned(), json!("x"));
        let observed_at = FixedOffset::east_opt(0)
            .expect("utc offset")
            .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
            .single()
            .expect("timestamp");
        ObservabilityRecord::new(
            hook,
            observed_at,
            ObservabilityMetadata::new("s-1", "r-1"),
            metrics,
        )
        .expect("record")
    }

    #[test]
    fn the_defaults_match_v1() {
        let dir = TempDir::new().expect("temp dir");
        let writer =
            ObservabilitySqliteWriter::new(&dir.path().join("observability.db")).expect("writer");
        assert_eq!(
            writer.sink().max_age_days(),
            Some(DEFAULT_OBSERVABILITY_RETENTION_DAYS)
        );
        assert!(
            !writer.sink().serializes_writes(),
            "v1 holds no write lock on this stream"
        );
        assert_eq!(writer.sink().store().log_prefix(), OBSERVABILITY_LOG_PREFIX);
    }

    #[test]
    fn both_entry_points_persist_a_record() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("observability.db");
        let writer = ObservabilitySqliteWriter::new(&path).expect("writer");
        assert!(!path.exists(), "construction must not touch the filesystem");

        writer.write(&record());
        writer.write_or_raise(&record()).expect("second write");
        writer.close_at(1000.0);

        let store = Arc::new(
            SqliteStore::new(
                &path,
                true,
                OBSERVABILITY_SQLITE_SCHEMA_VERSION,
                OBSERVABILITY_TABLES,
                None,
                OBSERVABILITY_LOG_PREFIX,
            )
            .expect("store"),
        );
        let count = store
            .with_connection(true, |conn| ObservabilityEventRepository.count(conn))
            .expect("read")
            .expect("value");
        assert_eq!(count, 2);
    }

    #[test]
    fn write_swallows_what_write_or_raise_surfaces() {
        let dir = TempDir::new().expect("temp dir");
        // A directory in place of the database file makes every open fail.
        let path = dir.path().join("observability.db");
        std::fs::create_dir(&path).expect("occupy the path");

        let writer = ObservabilitySqliteWriter::new(&path).expect("writer");
        writer.write(&record());
        assert!(writer.write_or_raise(&record()).is_err());
    }

    /// A failing retention pass stays silent and still advances the gate.
    ///
    /// v1's `prune` catches `SQLAlchemyError` and disposes the engine so the
    /// next write reconnects. v2 keeps its cached connection, because a
    /// `rusqlite` connection stays usable after a failed statement (see D-25).
    /// What both versions share, and what this pins, is that the failure never
    /// reaches the caller and never blocks the rest of the close path.
    #[test]
    fn a_failing_retention_pass_is_swallowed_and_still_marks_the_gate() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("observability.db");
        let writer = ObservabilitySqliteWriter::new(&path).expect("writer");
        writer.write(&record());

        // Removing the table from a second connection is the cheapest way to
        // make the `DELETE` inside the retention pass fail for real.
        let conn = rusqlite::Connection::open(&path).expect("open");
        conn.execute("DROP TABLE observability_events", [])
            .expect("drop table");
        drop(conn);

        writer.close_at(1000.0);

        assert!(!writer.sink().store().is_open());
        assert!(
            dir.path().join("observability.db.maintenance").exists(),
            "the gate must record the attempt even when pruning failed"
        );
    }
}
