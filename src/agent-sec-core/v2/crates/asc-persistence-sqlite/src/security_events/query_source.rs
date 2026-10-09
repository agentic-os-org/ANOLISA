//! The `SQLite` adapter for the security-event query port.
//!
//! This is the storage side of `asc_security_events::query`: it renders the
//! port's scope and filters in SQL through the shared repository and reports
//! storage failures instead of degrading, so a daemon `sec.*` response can
//! distinguish "the store is unreadable" from "there are no events". A
//! database that does not exist yet still answers empty — that is the normal
//! state before the first write, not a failure.
//!
//! Every query runs under a hard execution budget enforced inside `SQLite`
//! itself (a progress handler), so work stops at the source instead of being
//! abandoned mid-flight by a transport deadline that cannot cancel it.

use std::path::Path;
use std::time::{Duration, Instant};

use asc_security_events::SecurityEvent;
use asc_security_events::SecurityEventsSummary;
use asc_security_events::query::{
    EventFilters, GroupCounts, QueryError, QueryScope, SecurityEventQueries, VALID_GROUP_FIELDS,
};
use asc_sqlite_kernel::KernelError;
use rusqlite::Connection;

use crate::security_events::reader::SqliteEventReader;
use crate::security_events::repository::SecurityEventRepository;

/// Hard cap on one query's execution time.
///
/// The transport deadline that frames a daemon response is five seconds; the
/// budget matches it so a runaway query is interrupted and reported as an
/// error instead of racing the transport into an ambiguous timeout.
pub const QUERY_BUDGET: Duration = Duration::from_secs(5);

/// `SQLite` virtual-machine steps between progress callbacks.
const PROGRESS_STEPS: i32 = 100_000;

/// [`SecurityEventQueries`] over the daemon's security-event database.
///
/// The reader opens its own read-only connection and survives a replaced
/// database file by inode check; the adapter maps kernel failures to
/// [`QueryError::Unavailable`] and keeps the missing-database degradation for
/// the pre-first-write state.
pub struct SqliteEventQuerySource {
    reader: SqliteEventReader,
}

impl SqliteEventQuerySource {
    /// Opens the source over the database at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError`] when the path cannot be normalized.
    pub fn new(path: &Path) -> Result<Self, KernelError> {
        Ok(Self {
            reader: SqliteEventReader::new(path)?,
        })
    }

    /// Runs one repository query under the execution budget.
    ///
    /// `Ok(None)` means the database is absent, which the caller renders as
    /// the pre-first-write empty answer.
    fn guarded<T>(
        &self,
        query: impl FnOnce(&SecurityEventRepository, &Connection) -> Result<T, KernelError>,
    ) -> Result<Option<T>, KernelError> {
        let deadline = Instant::now() + QUERY_BUDGET;
        self.reader.try_query(|repository, connection| {
            connection.progress_handler(PROGRESS_STEPS, Some(move || Instant::now() >= deadline));
            let outcome = query(repository, connection);
            connection.progress_handler(0, None::<fn() -> bool>);
            outcome
        })
    }
}

/// Maps a kernel failure to the port's unavailable error.
fn unavailable(error: &KernelError) -> QueryError {
    QueryError::Unavailable(error.to_string())
}

impl SecurityEventQueries for SqliteEventQuerySource {
    fn summary(
        &self,
        filters: &EventFilters,
        scope: QueryScope,
        latest_limit: u32,
    ) -> Result<SecurityEventsSummary, QueryError> {
        match self.guarded(|repository, connection| {
            repository.summary(connection, filters, &scope, latest_limit)
        }) {
            Ok(Some(summary)) => Ok(summary),
            Ok(None) => Ok(SecurityEventsSummary::default()),
            Err(error) => Err(unavailable(&error)),
        }
    }

    fn list(
        &self,
        filters: &EventFilters,
        scope: QueryScope,
        limit: u32,
        offset: i64,
    ) -> Result<Vec<SecurityEvent>, QueryError> {
        match self.guarded(|repository, connection| {
            repository.query(connection, filters, &scope, limit, offset)
        }) {
            Ok(Some(events)) => Ok(events),
            Ok(None) => Ok(Vec::new()),
            Err(error) => Err(unavailable(&error)),
        }
    }

    fn count(
        &self,
        filters: &EventFilters,
        scope: QueryScope,
        offset: i64,
    ) -> Result<u64, QueryError> {
        match self
            .guarded(|repository, connection| repository.count(connection, filters, &scope, offset))
        {
            Ok(Some(count)) => Ok(count),
            Ok(None) => Ok(0),
            Err(error) => Err(unavailable(&error)),
        }
    }

    fn count_by(
        &self,
        group_field: &str,
        filters: &EventFilters,
        scope: QueryScope,
        offset: i64,
    ) -> Result<GroupCounts, QueryError> {
        let column = VALID_GROUP_FIELDS
            .iter()
            .copied()
            .find(|field| *field == group_field)
            .ok_or_else(|| QueryError::InvalidGroupField(group_field.to_owned()))?;
        match self.guarded(|repository, connection| {
            repository.count_by(connection, column, filters, &scope, offset)
        }) {
            Ok(Some(groups)) => Ok(groups),
            Ok(None) => Ok(Vec::new()),
            Err(KernelError::Malformed(reason)) => Err(QueryError::InvalidGroupField(reason)),
            Err(error) => Err(unavailable(&error)),
        }
    }

    fn get(&self, event_id: &str, scope: QueryScope) -> Result<Option<SecurityEvent>, QueryError> {
        match self.guarded(|repository, connection| repository.get(connection, event_id, &scope)) {
            Ok(outcome) => Ok(outcome.flatten()),
            Err(error) => Err(unavailable(&error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security_events::writer::SqliteEventWriter;
    use asc_security_events::SecurityEvent;
    use serde_json::Map;
    use tempfile::TempDir;

    fn seed_two_owners(path: &Path) {
        let writer = SqliteEventWriter::new(path).expect("writer");
        for (id, uid, category) in [
            ("a1", 1000_u32, "exec"),
            ("a2", 1000, "network"),
            ("b1", 2000, "exec"),
        ] {
            let mut event = SecurityEvent::new("sandbox_prehook", category, Map::new());
            id.clone_into(&mut event.event_id);
            event.uid = uid;
            event.session_id = Some("s-1".to_owned());
            writer.write(&event);
        }
        writer.close_at(1000.0);
    }

    #[test]
    fn a_missing_database_still_answers_empty() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("absent.db");
        let source = SqliteEventQuerySource::new(&path).expect("source");

        assert!(
            source
                .list(&EventFilters::default(), QueryScope::Owner(1000), 100, 0)
                .expect("query")
                .is_empty()
        );
        assert_eq!(
            source
                .count(&EventFilters::default(), QueryScope::Owner(1000), 0)
                .expect("count"),
            0
        );
        assert!(
            source
                .get("a1", QueryScope::Owner(1000))
                .expect("get")
                .is_none()
        );
        assert!(!path.exists(), "a read-only source never creates the store");
    }

    #[test]
    fn a_corrupt_database_is_an_error_not_an_empty_answer() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        std::fs::write(&path, b"this is not a sqlite database at all").expect("seed bytes");

        let source = SqliteEventQuerySource::new(&path).expect("source opens lazily");
        let error = source
            .list(&EventFilters::default(), QueryScope::Owner(1000), 100, 0)
            .expect_err("a corrupt store must not look empty");
        assert!(matches!(error, QueryError::Unavailable(_)), "{error}");
        let error = source
            .count(&EventFilters::default(), QueryScope::Owner(1000), 0)
            .expect_err("a corrupt store must not count as zero");
        assert!(matches!(error, QueryError::Unavailable(_)), "{error}");
    }

    #[test]
    fn root_reads_every_owner_and_owner_scopes_stay_isolated() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        seed_two_owners(&path);
        let source = SqliteEventQuerySource::new(&path).expect("source");

        let all = source
            .list(&EventFilters::default(), QueryScope::All, 100, 0)
            .expect("all scope");
        assert_eq!(all.len(), 3, "the root scope sees every owner");

        let owner_a = source
            .list(&EventFilters::default(), QueryScope::Owner(1000), 100, 0)
            .expect("owner scope");
        assert_eq!(owner_a.len(), 2);
        for event in owner_a {
            assert_eq!(event.uid, 1000);
        }
    }

    #[test]
    fn an_invalid_group_field_is_rejected_before_the_store() {
        let dir = TempDir::new().expect("temp dir");
        let source = SqliteEventQuerySource::new(&dir.path().join("absent.db")).expect("source");
        let error = source
            .count_by(
                "details",
                &EventFilters::default(),
                QueryScope::Owner(1000),
                0,
            )
            .expect_err("rejected");
        assert!(matches!(error, QueryError::InvalidGroupField(_)), "{error}");
    }

    #[test]
    fn count_reports_the_remaining_rows_after_an_offset() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        seed_two_owners(&path);
        let source = SqliteEventQuerySource::new(&path).expect("source");

        let total = source
            .count(&EventFilters::default(), QueryScope::Owner(1000), 0)
            .expect("total");
        assert_eq!(total, 2);
        let remaining = source
            .count(&EventFilters::default(), QueryScope::Owner(1000), 1)
            .expect("remaining");
        assert_eq!(remaining, 1, "offset counts what is left after skipping");
    }

    #[test]
    fn same_timestamp_rows_are_ordered_by_event_id() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.db");
        let writer = SqliteEventWriter::new(&path).expect("writer");
        for id in ["same-time-c", "same-time-a", "same-time-b"] {
            let mut event = SecurityEvent::new("sandbox_prehook", "exec", Map::new());
            id.clone_into(&mut event.event_id);
            event.uid = 1000;
            // One shared timestamp: `SecurityEvent::new` stamps distinct
            // sub-second times, which would hide the event_id tiebreak.
            event.timestamp = "2026-01-01T00:00:00+00:00".to_owned();
            writer.write(&event);
        }
        writer.close_at(1000.0);
        let source = SqliteEventQuerySource::new(&path).expect("source");

        let ids: Vec<String> = source
            .list(&EventFilters::default(), QueryScope::Owner(1000), 100, 0)
            .expect("list")
            .into_iter()
            .map(|event| event.event_id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "same-time-c".to_owned(),
                "same-time-b".to_owned(),
                "same-time-a".to_owned()
            ],
            "equal timestamps fall back to event_id, newest-first overall"
        );
    }
}
