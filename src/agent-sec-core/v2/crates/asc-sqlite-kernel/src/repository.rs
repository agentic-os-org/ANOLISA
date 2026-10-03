//! The record repository contract.

use rusqlite::Connection;

use crate::error::KernelError;
use crate::schema::TableSpec;

/// Per-stream persistence operations the kernel drives.
///
/// `checkpoint` gets a default implementation because v1's two copies of it are
/// byte-for-byte identical.
pub trait RecordRepository: Send + Sync {
    /// The record type this repository persists.
    type Record;

    /// Returns the table contract this repository writes to.
    fn tables(&self) -> &'static [TableSpec];

    /// Inserts one record, returning whether a row was actually written.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Malformed`] when the record itself is invalid, and
    /// [`KernelError::Sqlite`] for database faults. The distinction matters: the
    /// kernel never disposes a connection over a malformed record.
    fn insert_or_raise(
        &self,
        conn: &Connection,
        record: &Self::Record,
    ) -> Result<bool, KernelError>;

    /// Deletes rows older than `max_age_days` relative to `now`.
    ///
    /// `now` is a parameter rather than a call to the clock so retention can be
    /// tested deterministically. v1 only has it on the observability side.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Sqlite`] when the delete fails.
    fn prune(&self, conn: &Connection, max_age_days: u32, now: f64) -> Result<usize, KernelError>;

    /// Truncates the WAL.
    ///
    /// Failures are intentionally ignored: v1 treats the checkpoint as a
    /// best-effort housekeeping step.
    fn checkpoint(&self, conn: &Connection) {
        let _ = conn.pragma_update(None, "wal_checkpoint", "TRUNCATE");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ColumnSpec, TableSpec};

    const WIDGETS: &[TableSpec] = &[TableSpec {
        name: "widgets",
        columns: &[ColumnSpec {
            name: "id",
            definition: "TEXT PRIMARY KEY",
        }],
        indexes: &[],
        extra_columns: &[],
    }];

    struct WidgetRepository;

    impl RecordRepository for WidgetRepository {
        type Record = String;

        fn tables(&self) -> &'static [TableSpec] {
            WIDGETS
        }

        fn insert_or_raise(
            &self,
            conn: &Connection,
            record: &Self::Record,
        ) -> Result<bool, KernelError> {
            if record.is_empty() {
                return Err(KernelError::Malformed("id must not be empty".to_owned()));
            }
            let changed = conn.execute(
                "INSERT INTO widgets (id) VALUES (?1) ON CONFLICT(id) DO NOTHING",
                [record],
            )?;
            Ok(changed > 0)
        }

        fn prune(
            &self,
            _conn: &Connection,
            _max_age_days: u32,
            _now: f64,
        ) -> Result<usize, KernelError> {
            Ok(0)
        }
    }

    #[test]
    fn default_checkpoint_is_silent_on_failure() {
        let conn = Connection::open_in_memory().expect("memory db");
        // An in-memory database has no WAL, so the pragma fails; the default
        // implementation must not panic or surface anything.
        WidgetRepository.checkpoint(&conn);
    }

    #[test]
    fn insert_reports_whether_a_row_landed() {
        let conn = Connection::open_in_memory().expect("memory db");
        conn.execute_batch(&WIDGETS[0].create_table_sql())
            .expect("create");
        assert!(
            WidgetRepository
                .insert_or_raise(&conn, &"a".to_owned())
                .expect("insert")
        );
        assert!(
            !WidgetRepository
                .insert_or_raise(&conn, &"a".to_owned())
                .expect("conflict"),
            "a conflicting insert must report false, not fail"
        );
        assert!(matches!(
            WidgetRepository.insert_or_raise(&conn, &String::new()),
            Err(KernelError::Malformed(_))
        ));
    }
}
