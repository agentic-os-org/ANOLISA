//! The observability `SQLite` reader facade.
//!
//! Migrated from v1 `observability/sqlite_reader.py`. v1 needs a six-line
//! comment there warning that omitting `models` / `schema_version` makes the
//! store fall back to the import-time-registered security-event models and print
//! a misleading `missing_tables=['security_events']` warning. v2 removed the
//! global registry, so the table spec and schema version are required
//! constructor arguments and that mistake is not expressible.

use std::path::Path;
use std::sync::Arc;

use asc_observability::{
    OBSERVABILITY_LOG_PREFIX, OBSERVABILITY_SQLITE_SCHEMA_VERSION, RunSummary, SessionSummary,
    config::get_observability_db_path,
};
use asc_sqlite_kernel::{KernelError, ReadOnlySource, SqliteStore};

use crate::observability::repository::{
    EpochWindow, ObservabilityEventRepository, ObservabilityEventRow, Page,
};
use crate::observability::table::OBSERVABILITY_TABLES;
use crate::observability::writer::ObservabilityWriterError;

/// Read-only access to the observability index.
#[derive(Debug)]
pub struct ObservabilityReader {
    source: ReadOnlySource<ObservabilityEventRepository>,
}

impl ObservabilityReader {
    /// Opens a reader over `path`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] when the path cannot be normalized.
    pub fn new(path: &Path) -> Result<Self, KernelError> {
        let store = Arc::new(SqliteStore::new(
            path,
            true,
            OBSERVABILITY_SQLITE_SCHEMA_VERSION,
            OBSERVABILITY_TABLES,
            None,
            OBSERVABILITY_LOG_PREFIX,
        )?);
        Ok(Self {
            source: ReadOnlySource::new(store, ObservabilityEventRepository),
        })
    }

    /// Opens a reader over the resolved default database path.
    ///
    /// # Errors
    ///
    /// Returns [`ObservabilityWriterError`] when the data directory cannot be
    /// resolved or the store cannot be built.
    pub fn at_default_path() -> Result<Self, ObservabilityWriterError> {
        let path = get_observability_db_path()?;
        Ok(Self::new(&path)?)
    }

    /// Returns the database path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.source.store().path()
    }

    /// Returns the number of indexed records.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.source.query_or(0, ObservabilityEventRepository::count)
    }

    /// Returns the number of distinct sessions in `window`.
    #[must_use]
    pub fn count_sessions(&self, window: EpochWindow) -> u64 {
        self.source
            .query_or(0, |repo, conn| repo.count_sessions(conn, window))
    }

    /// Returns the number of distinct runs of `session_id` in `window`.
    #[must_use]
    pub fn count_runs(&self, session_id: &str, window: EpochWindow) -> u64 {
        self.source
            .query_or(0, |repo, conn| repo.count_runs(conn, session_id, window))
    }

    /// Returns sessions ordered by most recent activity first.
    #[must_use]
    pub fn list_sessions(&self, window: EpochWindow, page: Page) -> Vec<SessionSummary> {
        self.source
            .query_or_default(|repo, conn| repo.list_sessions(conn, window, page))
    }

    /// Returns the runs of `session_id` in chronological order.
    #[must_use]
    pub fn list_runs(&self, session_id: &str, window: EpochWindow, page: Page) -> Vec<RunSummary> {
        self.source
            .query_or_default(|repo, conn| repo.list_runs(conn, session_id, window, page))
    }

    /// Returns the rows of one run, oldest first.
    #[must_use]
    pub fn list_events(
        &self,
        session_id: &str,
        run_id: &str,
        window: EpochWindow,
        page: Page,
    ) -> Vec<ObservabilityEventRow> {
        self.source
            .query_or_default(|repo, conn| repo.list_events(conn, session_id, run_id, window, page))
    }

    /// Drops the cached read-only connection.
    pub fn close(&self) {
        self.source.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::writer::ObservabilitySqliteWriter;
    use asc_observability::{ObservabilityHook, ObservabilityMetadata, ObservabilityRecord};
    use chrono::{FixedOffset, TimeZone};
    use serde_json::{Map, json};
    use tempfile::TempDir;

    fn seed(path: &Path) {
        let hook = ObservabilityHook::BeforeAgentRun;
        let mut metrics = Map::new();
        metrics.insert("user_input".to_owned(), json!("hello"));
        let observed_at = FixedOffset::east_opt(0)
            .expect("utc offset")
            .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
            .single()
            .expect("timestamp");
        let record = ObservabilityRecord::new(
            hook,
            observed_at,
            ObservabilityMetadata::new("s-1", "r-1"),
            metrics,
        )
        .expect("record");

        let writer = ObservabilitySqliteWriter::new(path).expect("writer");
        writer.write(&record);
        writer.close_at(1000.0);
    }

    #[test]
    fn a_reader_over_a_missing_database_returns_empty_results() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("absent.db");
        let reader = ObservabilityReader::new(&path).expect("reader");

        assert_eq!(reader.count(), 0);
        assert_eq!(reader.count_sessions(EpochWindow::default()), 0);
        assert_eq!(reader.count_runs("s-1", EpochWindow::default()), 0);
        assert!(
            reader
                .list_sessions(EpochWindow::default(), Page::default())
                .is_empty()
        );
        assert!(
            reader
                .list_runs("s-1", EpochWindow::default(), Page::default())
                .is_empty()
        );
        assert!(
            reader
                .list_events("s-1", "r-1", EpochWindow::default(), Page::default())
                .is_empty()
        );
        assert!(
            !path.exists(),
            "a read-only store must never create the database"
        );
    }

    #[test]
    fn a_reader_sees_what_the_writer_wrote() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("observability.db");
        seed(&path);

        let reader = ObservabilityReader::new(&path).expect("reader");
        assert_eq!(reader.count(), 1);
        assert_eq!(reader.count_sessions(EpochWindow::default()), 1);
        assert_eq!(reader.count_runs("s-1", EpochWindow::default()), 1);

        let runs = reader.list_runs("s-1", EpochWindow::default(), Page::default());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].user_input_preview.as_deref(), Some("hello"));

        reader.close();
        assert_eq!(reader.count(), 1, "the reader must reopen after close");
    }

    #[test]
    fn the_table_spec_is_a_required_constructor_argument() {
        // v1 needs a comment to warn about this; here the compiler enforces it,
        // and the store carries the observability spec rather than a global default.
        let dir = TempDir::new().expect("temp dir");
        let reader =
            ObservabilityReader::new(&dir.path().join("observability.db")).expect("reader");
        assert_eq!(
            reader.source.store().tables()[0].name,
            "observability_events"
        );
        assert_eq!(
            reader.source.store().schema_version(),
            OBSERVABILITY_SQLITE_SCHEMA_VERSION
        );
    }
}
