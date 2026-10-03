//! The security-event `SQLite` reader facade.
//!
//! Migrated from v1 `security_events/sqlite_reader.py`, which is itself a thin
//! delegation layer. The read-only connection lifecycle — including the inode
//! check that survives a replaced database file — lives in
//! [`asc_sqlite_kernel::ReadOnlySource`].

use std::path::Path;
use std::sync::Arc;

use asc_security_events::{
    CorrelationCandidate, SECURITY_EVENTS_SQLITE_SCHEMA_VERSION, SecurityEvent,
    SecurityEventsSummary, config::get_db_path,
};
use asc_sqlite_kernel::{KernelError, ReadOnlySource, SqliteStore};

use crate::security_events::repository::{
    CorrelationRequest, EventFilters, GroupCounts, SecurityEventRepository, validate_group_field,
};
use crate::security_events::table::SECURITY_EVENTS_TABLES;
use crate::security_events::writer::{LOG_PREFIX, WriterError};

/// v1's default page size for [`SqliteEventReader::query`].
pub const DEFAULT_QUERY_LIMIT: u32 = 1000;

/// v1's default row count for the summary's latest-events list.
pub const DEFAULT_LATEST_LIMIT: u32 = 5;

/// Read-only access to the security-event index.
///
/// Database availability failures degrade to empty results because a missing
/// database is normal before the first write. Invalid caller input still returns
/// an error.
#[derive(Debug)]
pub struct SqliteEventReader {
    source: ReadOnlySource<SecurityEventRepository>,
}

impl SqliteEventReader {
    /// Opens a reader over `path`.
    ///
    /// No migrator is passed: a read-only store must never migrate. v1 relies on
    /// `read_only=True` for the same guarantee.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] when the path cannot be normalized.
    pub fn new(path: &Path) -> Result<Self, KernelError> {
        let store = Arc::new(SqliteStore::new(
            path,
            true,
            SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
            SECURITY_EVENTS_TABLES,
            None,
            LOG_PREFIX,
        )?);
        Ok(Self {
            source: ReadOnlySource::new(store, SecurityEventRepository),
        })
    }

    /// Opens a reader over the resolved default database path.
    ///
    /// # Errors
    ///
    /// Returns [`WriterError`] when the data directory cannot be resolved or the
    /// store cannot be built.
    pub fn at_default_path() -> Result<Self, WriterError> {
        let path = get_db_path()?;
        Ok(Self::new(&path)?)
    }

    /// Returns the database path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.source.store().path()
    }

    /// Returns matching events, newest first.
    #[must_use]
    pub fn query(&self, filters: &EventFilters, limit: u32, offset: u32) -> Vec<SecurityEvent> {
        self.source
            .query_or_default(|repo, conn| repo.query(conn, filters, limit, offset))
    }

    /// Returns matching events using v1's default page size.
    #[must_use]
    pub fn query_default_page(&self, filters: &EventFilters) -> Vec<SecurityEvent> {
        self.query(filters, DEFAULT_QUERY_LIMIT, 0)
    }

    /// Returns one event by id, or `None`.
    #[must_use]
    pub fn get(&self, event_id: &str) -> Option<SecurityEvent> {
        self.source
            .query_or(None, |repo, conn| repo.get(conn, event_id))
    }

    /// Returns correlation candidates for the observability correlator.
    #[must_use]
    pub fn query_correlation_candidates(
        &self,
        request: &CorrelationRequest<'_>,
    ) -> Vec<CorrelationCandidate> {
        self.source
            .query_or_default(|repo, conn| repo.query_correlation_candidates(conn, request))
    }

    /// Counts matching events.
    #[must_use]
    pub fn count(&self, filters: &EventFilters, offset: u32) -> u64 {
        self.source
            .query_or(0, |repo, conn| repo.count(conn, filters, offset))
    }

    /// Counts matching events grouped by `group_field`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Malformed`] when `group_field` is outside the V1
    /// allowlist. Database availability failures still degrade to an empty result.
    pub fn count_by(
        &self,
        group_field: &str,
        filters: &EventFilters,
        offset: u32,
    ) -> Result<GroupCounts, KernelError> {
        validate_group_field(group_field)?;
        Ok(self
            .source
            .query_or_default(|repo, conn| repo.count_by(conn, group_field, filters, offset)))
    }

    /// Returns the dashboard aggregates and the newest rows.
    #[must_use]
    pub fn summary(&self, filters: &EventFilters, latest_limit: u32) -> SecurityEventsSummary {
        self.source
            .query_or_default(|repo, conn| repo.summary(conn, filters, latest_limit))
    }

    /// Drops the cached read-only connection.
    pub fn close(&self) {
        self.source.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security_events::writer::SqliteEventWriter;
    use serde_json::Map;
    use tempfile::TempDir;

    fn seed(path: &Path) {
        let writer = SqliteEventWriter::new(path).expect("writer");
        let mut event = SecurityEvent::new("sandbox_prehook", "exec", Map::new());
        "e1".clone_into(&mut event.event_id);
        writer.write(&event);
        writer.close_at(1000.0);
    }

    #[test]
    fn the_defaults_match_v1() {
        assert_eq!(DEFAULT_QUERY_LIMIT, 1000);
        assert_eq!(DEFAULT_LATEST_LIMIT, 5);
    }

    #[test]
    fn a_reader_over_a_missing_database_returns_empty_results() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("absent.db");
        let reader = SqliteEventReader::new(&path).expect("reader");

        assert!(
            reader
                .query_default_page(&EventFilters::default())
                .is_empty()
        );
        assert_eq!(reader.count(&EventFilters::default(), 0), 0);
        assert!(reader.get("e1").is_none());
        assert!(
            reader
                .count_by("category", &EventFilters::default(), 0)
                .expect("valid group field")
                .is_empty()
        );
        assert_eq!(reader.summary(&EventFilters::default(), 5).total, 0);
        assert!(
            !path.exists(),
            "a read-only store must never create the database"
        );
    }

    #[test]
    fn a_reader_sees_what_the_writer_wrote() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        seed(&path);

        let reader = SqliteEventReader::new(&path).expect("reader");
        assert_eq!(reader.count(&EventFilters::default(), 0), 1);
        assert_eq!(
            reader.get("e1").map(|event| event.event_id),
            Some("e1".to_owned())
        );
        reader.close();
        assert_eq!(
            reader.count(&EventFilters::default(), 0),
            1,
            "the reader must reopen after close"
        );
    }

    #[test]
    fn an_invalid_group_field_is_rejected() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        seed(&path);

        let reader = SqliteEventReader::new(&path).expect("reader");
        let error = reader
            .count_by("details", &EventFilters::default(), 0)
            .expect_err("invalid group field must be rejected");
        assert!(matches!(error, KernelError::Malformed(_)));
    }
}
