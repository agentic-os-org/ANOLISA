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
use rusqlite::Connection;

use crate::scope::QueryScope;
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

    /// Runs one repository query and reports the full outcome.
    ///
    /// This is the query-port counterpart of the degrading facades below:
    /// `Ok(None)` means the database is absent, an error means it could not
    /// be read. See [`ReadOnlySource::try_query`].
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] when the query cannot run.
    pub fn try_query<T>(
        &self,
        query: impl FnOnce(&SecurityEventRepository, &Connection) -> Result<T, KernelError>,
    ) -> Result<Option<T>, KernelError> {
        self.source.try_query(query)
    }

    /// Returns matching events, newest first, within one owner scope.
    #[must_use]
    pub fn query(
        &self,
        filters: &EventFilters,
        scope: &QueryScope,
        limit: u32,
        offset: i64,
    ) -> Vec<SecurityEvent> {
        self.source
            .query_or_default(|repo, conn| repo.query(conn, filters, scope, limit, offset))
    }

    /// Returns matching events using v1's default page size.
    #[must_use]
    pub fn query_default_page(
        &self,
        filters: &EventFilters,
        scope: &QueryScope,
    ) -> Vec<SecurityEvent> {
        self.query(filters, scope, DEFAULT_QUERY_LIMIT, 0)
    }

    /// Returns one event by id within one owner scope, or `None`.
    ///
    /// A record outside the scope and a missing record are indistinguishable.
    #[must_use]
    pub fn get(&self, event_id: &str, scope: &QueryScope) -> Option<SecurityEvent> {
        self.source
            .query_or(None, |repo, conn| repo.get(conn, event_id, scope))
    }

    /// Returns correlation candidates for the observability correlator,
    /// within one owner scope.
    #[must_use]
    pub fn query_correlation_candidates(
        &self,
        request: &CorrelationRequest<'_>,
        scope: &QueryScope,
    ) -> Vec<CorrelationCandidate> {
        self.source
            .query_or_default(|repo, conn| repo.query_correlation_candidates(conn, request, scope))
    }

    /// Counts matching events within one owner scope.
    #[must_use]
    pub fn count(&self, filters: &EventFilters, scope: &QueryScope, offset: i64) -> u64 {
        self.source
            .query_or(0, |repo, conn| repo.count(conn, filters, scope, offset))
    }

    /// Counts matching events within one owner scope, grouped by `group_field`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Malformed`] when `group_field` is outside the V1
    /// allowlist. Database availability failures still degrade to an empty result.
    pub fn count_by(
        &self,
        group_field: &str,
        filters: &EventFilters,
        scope: &QueryScope,
        offset: i64,
    ) -> Result<GroupCounts, KernelError> {
        validate_group_field(group_field)?;
        Ok(self.source.query_or_default(|repo, conn| {
            repo.count_by(conn, group_field, filters, scope, offset)
        }))
    }

    /// Returns the dashboard aggregates and the newest rows of one owner scope.
    #[must_use]
    pub fn summary(
        &self,
        filters: &EventFilters,
        scope: &QueryScope,
        latest_limit: u32,
    ) -> SecurityEventsSummary {
        self.source
            .query_or_default(|repo, conn| repo.summary(conn, filters, scope, latest_limit))
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
        // The fixture rows and the scopes below must agree on the owner no
        // matter which UID runs the suite (`SecurityEvent::new` stamps the
        // current process UID by default).
        event.uid = 0;
        writer.write(&event);
        writer.close_at(1000.0);
    }

    /// Seeds two owners' rows so isolation is observable per method.
    fn seed_two_owners(path: &Path) {
        let writer = SqliteEventWriter::new(path).expect("writer");
        for (id, uid, category) in [
            ("a1", 1000_u32, "exec"),
            ("a2", 1000, "file"),
            ("b1", 2000, "exec"),
        ] {
            let mut event = SecurityEvent::new("sandbox_prehook", category, Map::new());
            id.clone_into(&mut event.event_id);
            event.uid = uid;
            event.session_id = Some("s1".to_owned());
            writer.write(&event);
        }
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
        let scope = QueryScope::Owner(0);

        assert!(
            reader
                .query_default_page(&EventFilters::default(), &scope)
                .is_empty()
        );
        assert_eq!(reader.count(&EventFilters::default(), &scope, 0), 0);
        assert!(reader.get("e1", &scope).is_none());
        assert!(
            reader
                .count_by("category", &EventFilters::default(), &scope, 0)
                .expect("valid group field")
                .is_empty()
        );
        assert_eq!(reader.summary(&EventFilters::default(), &scope, 5).total, 0);
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
        let scope = QueryScope::Owner(0);
        assert_eq!(reader.count(&EventFilters::default(), &scope, 0), 1);
        assert_eq!(
            reader.get("e1", &scope).map(|event| event.event_id),
            Some("e1".to_owned())
        );
        reader.close();
        assert_eq!(
            reader.count(&EventFilters::default(), &scope, 0),
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
            .count_by(
                "details",
                &EventFilters::default(),
                &QueryScope::Owner(0),
                0,
            )
            .expect_err("invalid group field must be rejected");
        assert!(matches!(error, KernelError::Malformed(_)));
    }

    #[test]
    fn every_read_method_isolates_one_owner() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        seed_two_owners(&path);

        let reader = SqliteEventReader::new(&path).expect("reader");
        let owner_a = QueryScope::Owner(1000);
        let owner_b = QueryScope::Owner(2000);

        let a_ids: Vec<String> = reader
            .query_default_page(&EventFilters::default(), &owner_a)
            .into_iter()
            .map(|event| event.event_id)
            .collect();
        let b_ids: Vec<String> = reader
            .query_default_page(&EventFilters::default(), &owner_b)
            .into_iter()
            .map(|event| event.event_id)
            .collect();
        assert_eq!(a_ids, vec!["a2".to_owned(), "a1".to_owned()]);
        assert_eq!(b_ids, vec!["b1".to_owned()]);

        assert_eq!(reader.count(&EventFilters::default(), &owner_a, 0), 2);
        assert_eq!(reader.count(&EventFilters::default(), &owner_b, 0), 1);

        let a_categories = reader
            .count_by("category", &EventFilters::default(), &owner_a, 0)
            .expect("valid group field");
        assert_eq!(a_categories.len(), 2, "owner A sees both its categories");
        let b_categories = reader
            .count_by("category", &EventFilters::default(), &owner_b, 0)
            .expect("valid group field");
        assert_eq!(b_categories.len(), 1, "owner B sees only its own category");

        assert_eq!(
            reader.summary(&EventFilters::default(), &owner_a, 5).total,
            2
        );
        assert_eq!(
            reader.summary(&EventFilters::default(), &owner_b, 5).total,
            1
        );
    }

    #[test]
    fn get_is_indistinguishable_between_missing_and_foreign_records() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        seed_two_owners(&path);

        let reader = SqliteEventReader::new(&path).expect("reader");
        let owner_a = QueryScope::Owner(1000);
        assert!(reader.get("a1", &owner_a).is_some());
        assert!(reader.get("b1", &owner_a).is_none(), "foreign owner");
        assert!(reader.get("missing", &owner_a).is_none(), "absent record");
    }

    #[test]
    fn correlation_candidates_stay_within_one_owner() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        seed_two_owners(&path);

        let reader = SqliteEventReader::new(&path).expect("reader");
        let request = CorrelationRequest {
            session_id: "s1",
            categories: &["exec".to_owned()],
            ..CorrelationRequest::default()
        };
        let owner_a = reader.query_correlation_candidates(&request, &QueryScope::Owner(1000));
        let owner_b = reader.query_correlation_candidates(&request, &QueryScope::Owner(2000));
        assert!(!owner_a.is_empty(), "owner A's exec rows are candidates");
        assert!(!owner_b.is_empty(), "owner B's exec rows are candidates");
        for candidate in owner_a {
            assert_eq!(candidate.event.uid, 1000);
        }
        for candidate in owner_b {
            assert_eq!(candidate.event.uid, 2000);
        }
    }
}
