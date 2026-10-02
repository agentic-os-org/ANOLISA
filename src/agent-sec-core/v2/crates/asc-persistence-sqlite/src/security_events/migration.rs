//! The rev-3 `verdict` migration.
//!
//! Migrated from v1 `security_events/models.py::migrate_security_events_schema`.
//! Two properties are load-bearing and easy to lose in a rewrite:
//!
//! * The backfill cursor advances on **`rowid`**, not on the shrinking
//!   `verdict IS NULL` set. A row whose `details` carries no verdict is never
//!   updated, so a set-based loop would spin forever on it.
//! * Each batch commits separately, so a large migration never holds one long
//!   write lock. That is why [`SchemaMigrator::migrate`] runs outside any
//!   enclosing transaction.

use asc_security_events::{SECURITY_EVENTS_VERDICT_SCHEMA_VERSION, extract_verdict};
use asc_sqlite_kernel::{KernelError, SchemaMigrator};
use rusqlite::Connection;
use serde_json::Value;

/// Rows scanned per backfill batch (v1 `_VERDICT_MIGRATION_BATCH_SIZE`).
pub const VERDICT_MIGRATION_BATCH_SIZE: u32 = 5000;

/// Applies security-event schema migrations before generic convergence.
#[derive(Debug, Default, Clone, Copy)]
pub struct SecurityEventsMigrator;

impl SchemaMigrator for SecurityEventsMigrator {
    /// Runs the `verdict` migration when the range crosses revision 3.
    ///
    /// The guard is v1's `from < 3 <= to`, so a database already at rev 3 or a
    /// target below it is left untouched.
    fn migrate(
        &self,
        conn: &Connection,
        from: u32,
        to: u32,
        _log_prefix: &str,
    ) -> Result<(), KernelError> {
        if from < SECURITY_EVENTS_VERDICT_SCHEMA_VERSION
            && SECURITY_EVENTS_VERDICT_SCHEMA_VERSION <= to
        {
            migrate_verdict_column(conn)?;
        }
        Ok(())
    }
}

/// Adds the `verdict` column when missing, then backfills it from `details`.
///
/// A missing table is not an error: v1 returns early because generic
/// convergence will create the table with the column already present.
fn migrate_verdict_column(conn: &Connection) -> Result<(), KernelError> {
    if !table_exists(conn, "security_events")? {
        return Ok(());
    }

    if !column_exists(conn, "security_events", "verdict")? {
        conn.execute_batch("ALTER TABLE security_events ADD COLUMN verdict TEXT")?;
    }

    let mut last_rowid: i64 = 0;
    loop {
        let batch = read_batch(conn, last_rowid)?;
        if batch.is_empty() {
            return Ok(());
        }

        // Advance on rowid, not on the NULL set: rows without a recoverable
        // verdict stay NULL forever and would otherwise be rescanned.
        last_rowid = batch
            .iter()
            .map(|row| row.rowid)
            .max()
            .unwrap_or(last_rowid);

        apply_batch(conn, &batch)?;
    }
}

/// One candidate row of the backfill scan.
struct Candidate {
    rowid: i64,
    event_id: String,
    details: String,
}

fn read_batch(conn: &Connection, last_rowid: i64) -> Result<Vec<Candidate>, KernelError> {
    let mut statement = conn.prepare(
        "SELECT rowid AS row_id, event_id, details \
         FROM security_events \
         WHERE verdict IS NULL AND rowid > ?1 \
         ORDER BY rowid \
         LIMIT ?2",
    )?;
    let rows = statement.query_map((last_rowid, VERDICT_MIGRATION_BATCH_SIZE), |row| {
        Ok(Candidate {
            rowid: row.get(0)?,
            event_id: row.get(1)?,
            details: row.get(2)?,
        })
    })?;
    let mut batch = Vec::new();
    for row in rows {
        batch.push(row?);
    }
    Ok(batch)
}

/// Writes the recoverable verdicts of one batch inside a single transaction.
fn apply_batch(conn: &Connection, batch: &[Candidate]) -> Result<(), KernelError> {
    let updates: Vec<(&str, String)> = batch
        .iter()
        .filter_map(|row| verdict_of(&row.details).map(|verdict| (row.event_id.as_str(), verdict)))
        .collect();
    if updates.is_empty() {
        return Ok(());
    }

    conn.execute_batch("BEGIN")?;
    let result = (|| -> Result<(), KernelError> {
        let mut statement =
            conn.prepare("UPDATE security_events SET verdict = ?1 WHERE event_id = ?2")?;
        for (event_id, verdict) in &updates {
            statement.execute((verdict, event_id))?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(err)
        }
    }
}

/// Returns the verdict embedded in a stored `details` blob.
///
/// Unparseable JSON and non-object payloads are skipped rather than reported:
/// v1 `continue`s past them so one bad row cannot stall the migration.
fn verdict_of(details: &str) -> Option<String> {
    match serde_json::from_str::<Value>(details) {
        Ok(Value::Object(map)) => extract_verdict(&map),
        _ => None,
    }
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, KernelError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, KernelError> {
    Ok(asc_sqlite_kernel::schema::existing_columns(conn, table)?
        .iter()
        .any(|name| name == column))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security_events::table::SECURITY_EVENTS_TABLES;

    /// Builds a rev-2 shaped table: everything except `verdict`.
    fn rev2_database() -> Connection {
        let conn = Connection::open_in_memory().expect("memory db");
        conn.execute_batch(
            "CREATE TABLE security_events (\
                event_id TEXT PRIMARY KEY, event_type TEXT NOT NULL, category TEXT NOT NULL, \
                result TEXT NOT NULL DEFAULT 'succeeded', timestamp TEXT NOT NULL, \
                timestamp_epoch REAL NOT NULL, trace_id TEXT NOT NULL DEFAULT '', \
                pid INTEGER NOT NULL, uid INTEGER NOT NULL, session_id TEXT, run_id TEXT, \
                call_id TEXT, tool_call_id TEXT, details TEXT NOT NULL)",
        )
        .expect("rev2 schema");
        conn
    }

    fn insert(conn: &Connection, event_id: &str, details: &str) {
        conn.execute(
            "INSERT INTO security_events (event_id, event_type, category, timestamp, \
             timestamp_epoch, pid, uid, details) VALUES (?1, 't', 'c', '2026-01-01T00:00:00+00:00', \
             1.0, 1, 1, ?2)",
            (event_id, details),
        )
        .expect("insert");
    }

    fn verdict(conn: &Connection, event_id: &str) -> Option<String> {
        conn.query_row(
            "SELECT verdict FROM security_events WHERE event_id = ?1",
            [event_id],
            |row| row.get(0),
        )
        .expect("select verdict")
    }

    #[test]
    fn the_column_is_added_and_backfilled_from_both_shapes() {
        let conn = rev2_database();
        insert(&conn, "direct", r#"{"verdict": "deny"}"#);
        insert(&conn, "nested", r#"{"result": {"verdict": "allow"}}"#);
        insert(&conn, "absent", r#"{"note": "nothing here"}"#);
        insert(&conn, "broken", "not json at all");
        insert(&conn, "array", "[1, 2, 3]");

        SecurityEventsMigrator
            .migrate(&conn, 2, 3, "[test]")
            .expect("migrate");

        assert_eq!(verdict(&conn, "direct").as_deref(), Some("deny"));
        assert_eq!(verdict(&conn, "nested").as_deref(), Some("allow"));
        assert_eq!(verdict(&conn, "absent"), None);
        assert_eq!(verdict(&conn, "broken"), None);
        assert_eq!(verdict(&conn, "array"), None);
    }

    #[test]
    fn a_row_without_a_verdict_does_not_stall_the_scan() {
        let conn = rev2_database();
        // The first row by rowid is unrecoverable; if the cursor advanced on the
        // NULL set instead of rowid this would never terminate.
        insert(&conn, "absent", "{}");
        insert(&conn, "direct", r#"{"verdict": "deny"}"#);

        SecurityEventsMigrator
            .migrate(&conn, 2, 3, "[test]")
            .expect("migrate");

        assert_eq!(verdict(&conn, "direct").as_deref(), Some("deny"));
    }

    #[test]
    fn the_migration_is_idempotent() {
        let conn = rev2_database();
        insert(&conn, "direct", r#"{"verdict": "deny"}"#);
        SecurityEventsMigrator
            .migrate(&conn, 2, 3, "[test]")
            .expect("first");
        SecurityEventsMigrator
            .migrate(&conn, 2, 3, "[test]")
            .expect("second");
        assert_eq!(verdict(&conn, "direct").as_deref(), Some("deny"));
    }

    #[test]
    fn a_range_outside_revision_three_is_a_no_op() {
        let conn = rev2_database();
        insert(&conn, "direct", r#"{"verdict": "deny"}"#);

        SecurityEventsMigrator
            .migrate(&conn, 1, 2, "[test]")
            .expect("below the range");
        assert!(
            !column_exists(&conn, "security_events", "verdict").expect("columns"),
            "a 1 -> 2 migration must not add the rev-3 column"
        );

        SecurityEventsMigrator
            .migrate(&conn, 3, 3, "[test]")
            .expect("already migrated");
        assert!(!column_exists(&conn, "security_events", "verdict").expect("columns"));
    }

    #[test]
    fn a_missing_table_is_not_an_error() {
        let conn = Connection::open_in_memory().expect("memory db");
        SecurityEventsMigrator
            .migrate(&conn, 2, 3, "[test]")
            .expect("a fresh database has nothing to migrate");
    }

    #[test]
    fn a_batch_larger_than_the_limit_is_fully_covered() {
        let conn = rev2_database();
        let total = VERDICT_MIGRATION_BATCH_SIZE + 7;
        conn.execute_batch("BEGIN").expect("begin");
        for index in 0..total {
            insert(&conn, &format!("e{index}"), r#"{"verdict": "deny"}"#);
        }
        conn.execute_batch("COMMIT").expect("commit");

        SecurityEventsMigrator
            .migrate(&conn, 2, 3, "[test]")
            .expect("migrate");

        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM security_events WHERE verdict IS NULL",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(remaining, 0, "the loop must span more than one batch");
    }

    #[test]
    fn the_table_spec_also_carries_verdict_for_rev_one_databases() {
        // Belt-and-braces: the migrator handles 2 -> 3, the spec handles 1 -> 3.
        assert!(
            SECURITY_EVENTS_TABLES[0]
                .extra_columns
                .iter()
                .any(|column| column.name == "verdict")
        );
    }
}
