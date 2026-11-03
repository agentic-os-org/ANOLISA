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
use asc_sqlite_kernel::{KernelError, MaintenanceOutcome, SqliteSink, SqliteStore, current_epoch};

use crate::observability::policy::ObservabilityFaultPolicy;
use crate::observability::repository::{ObservabilityEventRepository, check_schema};
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
    /// Revision 2 converges older stores by adding a nullable owner column and
    /// owner-scoped indexes; historical rows keep a `NULL` owner.
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

    /// Initializes or validates the schema before admitting queries.
    ///
    /// # Errors
    /// Returns storage failures or unsupported schema revisions.
    pub fn probe(&self) -> Result<(), KernelError> {
        self.sink
            .store()
            .with_connection(true, check_schema)?
            .ok_or(KernelError::Disabled)
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
    pub fn write(&self, record: &ObservabilityRecord, uid: u32) {
        self.sink.write(&(record.clone(), uid));
    }

    /// Inserts `record`, surfacing ingestion failures.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] carrying one of v1's three `OSError` messages, or
    /// the original error for a malformed record and for schema drift.
    pub fn write_or_raise(
        &self,
        record: &ObservabilityRecord,
        uid: u32,
    ) -> Result<(), KernelError> {
        self.sink.write_or_raise(&(record.clone(), uid))
    }

    /// Runs the gated maintenance pass and drops the connection.
    pub fn close(&self) {
        self.close_at(current_epoch());
    }

    /// Closes the writer as of `now`.
    pub fn close_at(&self, now: f64) {
        self.sink.close(now);
    }

    /// [`close_at`](ObservabilitySqliteWriter::close_at) with a deadline for
    /// the gated final maintenance pass; see
    /// [`SqliteSink::close_with_deadline`].
    pub fn close_with_deadline(&self, deadline: Option<std::time::Instant>) {
        self.sink.close_with_deadline(current_epoch(), deadline);
    }

    /// Runs the gated maintenance pass as of `now` without closing the writer.
    ///
    /// The daemon's periodic retention task calls this; [`ObservabilitySqliteWriter::close_at`]
    /// remains the shutdown path. A failed pass does not advance the gate's
    /// marker, so the next call retries.
    #[must_use]
    pub fn run_maintenance_at(&self, now: f64) -> MaintenanceOutcome {
        self.sink.run_maintenance_detailed(now)
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

        writer.write(&record(), 1000);
        writer
            .write_or_raise(&record(), 1000)
            .expect("second write");
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
        writer.write(&record(), 1000);
        assert!(writer.write_or_raise(&record(), 1000).is_err());
    }

    /// A failing retention pass stays silent and leaves the gate unmarked.
    ///
    /// v1's `prune` catches `SQLAlchemyError` and disposes the engine so the
    /// next write reconnects. v2 keeps its cached connection, because a
    /// `rusqlite` connection stays usable after a failed statement (see D-25).
    /// What both versions share, and what this pins, is that the failure never
    /// reaches the caller and never blocks the rest of the close path. What
    /// deliberately changed (#6602): the gate no longer records the attempt -
    /// a marked failure made the next retry wait a full window while the
    /// expired rows stayed in place.
    #[test]
    fn a_failing_retention_pass_stays_silent_and_does_not_mark_the_gate() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("observability.db");
        let writer = ObservabilitySqliteWriter::new(&path).expect("writer");
        writer.write(&record(), 1000);

        // Removing the table from a second connection is the cheapest way to
        // make the `DELETE` inside the retention pass fail for real.
        let conn = rusqlite::Connection::open(&path).expect("open");
        conn.execute("DROP TABLE observability_events", [])
            .expect("drop table");
        drop(conn);

        writer.close_at(1000.0);

        assert!(!writer.sink().store().is_open());
        assert!(
            !dir.path().join("observability.db.maintenance").exists(),
            "the gate must not record a failed pass - the marker belongs to a successful prune"
        );
    }

    #[test]
    fn run_maintenance_at_prunes_expired_records_without_closing() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("observability.db");
        let writer = ObservabilitySqliteWriter::new(&path).expect("writer");
        writer.probe().expect("probe");

        // A 2020 record is far outside the 7-day window anchored at the
        // 2026-01-02 `now`; the 2026-01-01 record survives.
        let expired = {
            let hook = ObservabilityHook::BeforeAgentRun;
            let mut metrics = Map::new();
            metrics.insert(hook.metric_names()[0].to_owned(), json!("x"));
            let observed_at = FixedOffset::east_opt(0)
                .expect("utc offset")
                .with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
                .single()
                .expect("timestamp");
            ObservabilityRecord::new(
                hook,
                observed_at,
                ObservabilityMetadata::new("s-1", "r-1"),
                metrics,
            )
            .expect("record")
        };
        writer
            .write_or_raise(&expired, 1000)
            .expect("write expired");
        writer.write_or_raise(&record(), 1000).expect("write fresh");

        let now = 1_767_312_000.0; // 2026-01-02T00:00:00Z
        assert_eq!(writer.run_maintenance_at(now), MaintenanceOutcome::Ran);
        assert!(
            writer.sink().store().is_open(),
            "a periodic pass must not close the store"
        );

        let count = writer
            .sink()
            .store()
            .with_connection(false, |conn| {
                conn.query_row("SELECT COUNT(*) FROM observability_events", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(KernelError::from)
            })
            .expect("connection")
            .expect("store open");
        assert_eq!(count, 1, "only the expired record is pruned");
        writer.close_at(now + 172_800.0);
    }
}
