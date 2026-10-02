//! Read-only query plumbing shared by both stream readers.
//!
//! v1 duplicates this lifecycle in `security_events/sqlite_reader.py` and
//! `observability/sqlite_reader.py`: build a read-only store, hand it to a
//! repository, and forward each query while degrading to an empty result when
//! the database is unavailable. That skeleton lives here once.

use std::sync::Arc;

use rusqlite::Connection;

use crate::error::KernelError;
use crate::store::SqliteStore;

/// A read-only view over one store and repository.
#[derive(Debug)]
pub struct ReadOnlySource<R> {
    store: Arc<SqliteStore>,
    repository: R,
}

impl<R> ReadOnlySource<R> {
    /// Wraps `store` and `repository`.
    ///
    /// The store is expected to have been built with `read_only = true`; that is
    /// what keeps queries from creating or migrating anything.
    pub const fn new(store: Arc<SqliteStore>, repository: R) -> Self {
        Self { store, repository }
    }

    /// Returns the underlying store.
    #[must_use]
    pub fn store(&self) -> &Arc<SqliteStore> {
        &self.store
    }

    /// Returns the repository.
    #[must_use]
    pub const fn repository(&self) -> &R {
        &self.repository
    }

    /// Runs `query` and falls back to `default` when it cannot run.
    ///
    /// This is the degradation v1 relies on: a missing or unreadable database
    /// yields an empty result rather than an error, so read commands stay usable
    /// before the first write has happened.
    ///
    /// A query that *fails* additionally drops the cached connection, matching
    /// every v1 read path's `except SQLAlchemyError: self._store.dispose()`. A
    /// database that was simply unavailable is not a reason to dispose, so the
    /// `Ok(None)` arm leaves the store alone.
    pub fn query_or<T>(
        &self,
        default: T,
        query: impl FnOnce(&R, &Connection) -> Result<T, KernelError>,
    ) -> T {
        match self
            .store
            .with_connection(false, |conn| query(&self.repository, conn))
        {
            Ok(Some(value)) => value,
            Ok(None) => default,
            Err(_) => {
                self.store.dispose();
                default
            }
        }
    }

    /// Runs `query` and falls back to `T::default()`.
    pub fn query_or_default<T: Default>(
        &self,
        query: impl FnOnce(&R, &Connection) -> Result<T, KernelError>,
    ) -> T {
        self.query_or(T::default(), query)
    }

    /// Drops the cached connection.
    pub fn close(&self) {
        self.store.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ColumnSpec, TableSpec};
    use tempfile::TempDir;

    const WIDGETS: &[TableSpec] = &[TableSpec {
        name: "widgets",
        columns: &[ColumnSpec {
            name: "id",
            definition: "TEXT PRIMARY KEY",
        }],
        indexes: &[],
        extra_columns: &[],
    }];

    struct WidgetReader {
        table: &'static str,
    }

    impl WidgetReader {
        const fn new() -> Self {
            Self { table: "widgets" }
        }

        fn count(&self, conn: &Connection) -> Result<i64, KernelError> {
            Ok(
                conn.query_row(&format!("SELECT COUNT(*) FROM {}", self.table), [], |row| {
                    row.get(0)
                })?,
            )
        }
    }

    fn store(path: &std::path::Path, read_only: bool) -> Arc<SqliteStore> {
        Arc::new(SqliteStore::new(path, read_only, 1, WIDGETS, None, "[test]").expect("store"))
    }

    #[test]
    fn missing_database_degrades_to_the_default() {
        let dir = TempDir::new().expect("temp dir");
        let source = ReadOnlySource::new(
            store(&dir.path().join("absent.db"), true),
            WidgetReader::new(),
        );
        assert_eq!(source.query_or(-1, WidgetReader::count), -1);
        assert_eq!(source.query_or_default(WidgetReader::count), 0);
    }

    #[test]
    fn a_failing_query_degrades_to_the_default() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        store(&path, false)
            .with_connection(true, |_| Ok(()))
            .expect("create")
            .expect("value");

        let source = ReadOnlySource::new(store(&path, true), WidgetReader::new());
        let value = source.query_or(-1, |_, conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM nonexistent", [], |row| row.get(0))?)
        });
        assert_eq!(value, -1);
    }

    #[test]
    fn reads_go_through_when_the_database_exists() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("widgets.db");
        store(&path, false)
            .with_connection(true, |conn| {
                conn.execute("INSERT INTO widgets (id) VALUES ('a')", [])?;
                Ok(())
            })
            .expect("write")
            .expect("value");

        let source = ReadOnlySource::new(store(&path, true), WidgetReader::new());
        assert_eq!(source.query_or(-1, WidgetReader::count), 1);
        source.close();
        assert_eq!(
            source.query_or(-1, WidgetReader::count),
            1,
            "the source must reopen after close"
        );
    }
}
