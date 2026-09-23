//! SQLite persistence for optimization analysis results.
//!
//! One row per analyzed session; each analysis dimension is stored as a JSON
//! string column so the schema stays stable while dimension payloads evolve.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use agentsight_sqlite_lifecycle::{
    checkpoint_truncate, enforce_size_policy, measure_database, open_connection,
    retention_cutoff_ns, CheckpointOutcome, ConnectionMode, ConnectionOptions, MaintenanceReport,
    MaintenanceStatus, SizeBasis, SizePolicy,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Errors produced by [`OptimizationStore`].
#[derive(Debug, thiserror::Error)]
pub enum OptStoreError {
    /// Shared lifecycle operation failed.
    #[error(transparent)]
    Lifecycle(#[from] agentsight_sqlite_lifecycle::LifecycleError),
    /// SQLite schema or query failed.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Another thread poisoned the database connection lock.
    #[error("store mutex poisoned")]
    Poisoned,
}

/// Analysis dimension identifying which result column to update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Perf,
    PerfIssues,
    Cost,
    CostWaste,
    Accuracy,
    Summary,
}

impl Dimension {
    /// Column name backing this dimension. Static strings only — never
    /// interpolate user input into SQL.
    fn column(self) -> &'static str {
        match self {
            Dimension::Perf => "perf",
            Dimension::PerfIssues => "perf_issues",
            Dimension::Cost => "cost",
            Dimension::CostWaste => "cost_waste",
            Dimension::Accuracy => "accuracy",
            Dimension::Summary => "summary",
        }
    }
}

/// Persisted per-session optimization results (dimension payloads are JSON strings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationRecord {
    pub session_id: String,
    pub perf: Option<String>,
    pub perf_issues: Option<String>,
    pub cost: Option<String>,
    pub cost_waste: Option<String>,
    pub accuracy: Option<String>,
    pub summary: Option<String>,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
}

/// Retention and capacity limits for optimization results.
#[derive(Debug, Clone, Copy, Default)]
pub struct OptimizationMaintenancePolicy {
    /// Maximum result age in days; zero disables age retention.
    pub retention_days: u64,
    /// Maximum logical database size in MiB; zero disables size maintenance.
    pub max_db_size_mb: u64,
}

/// Result of one optimization maintenance pass.
#[derive(Debug, Clone, Copy)]
pub struct OptimizationMaintenanceReport {
    /// Rows removed by age retention.
    pub expired_results: usize,
    /// Result of capacity enforcement.
    pub size: MaintenanceReport,
}

/// Thread-safe store over a dedicated `optimization.db`.
pub struct OptimizationStore {
    conn: Mutex<Connection>,
    db_path: PathBuf,
}

impl OptimizationStore {
    /// Opens (creating if needed) the database at `path` and ensures the schema.
    ///
    /// # Errors
    /// Returns [`OptStoreError::Sqlite`] if the database cannot be opened or
    /// the schema cannot be created.
    pub fn new_with_path(path: &Path) -> Result<Self, OptStoreError> {
        let conn = open_connection(path, ConnectionOptions::default())?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS optimization_results (
                session_id TEXT PRIMARY KEY,
                perf TEXT,
                perf_issues TEXT,
                cost TEXT,
                cost_waste TEXT,
                accuracy TEXT,
                summary TEXT,
                created_at_ns INTEGER NOT NULL,
                updated_at_ns INTEGER NOT NULL
            )",
            [],
        )?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: path.to_path_buf(),
        })
    }

    /// Opens an existing optimization database without creating or modifying it.
    ///
    /// # Errors
    /// Returns an error when the database is absent or cannot be opened read-only.
    pub fn open_read_only_existing(path: &Path) -> Result<Self, OptStoreError> {
        let conn = open_connection(
            path,
            ConnectionOptions {
                mode: ConnectionMode::ReadOnlyExisting,
                enable_wal: false,
                ..ConnectionOptions::default()
            },
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: path.to_path_buf(),
        })
    }

    /// Add columns missing from databases created by an earlier version.
    ///
    /// `CREATE TABLE IF NOT EXISTS` is a no-op once the table exists, so new
    /// dimension columns must be added explicitly. Idempotent: each column is
    /// only added when `PRAGMA table_info` says it is absent.
    fn migrate(conn: &Connection) -> Result<(), OptStoreError> {
        let existing = Self::column_names(conn)?;
        // (column, DDL) pairs — append here when a new dimension is introduced.
        for (column, ddl) in [(
            "summary",
            "ALTER TABLE optimization_results ADD COLUMN summary TEXT",
        )] {
            if !existing.iter().any(|c| c == column) {
                conn.execute(ddl, [])?;
            }
        }
        Ok(())
    }

    /// Column names currently present on `optimization_results`.
    fn column_names(conn: &Connection) -> Result<Vec<String>, OptStoreError> {
        let mut stmt = conn.prepare("PRAGMA table_info(optimization_results)")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row?);
        }
        Ok(names)
    }

    /// Upserts one dimension result (JSON string) for a session.
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn save_dimension(
        &self,
        session_id: &str,
        dimension: Dimension,
        result_json: &str,
    ) -> Result<(), OptStoreError> {
        let now_ns = now_ns();
        let conn = self.conn.lock().map_err(|_| OptStoreError::Poisoned)?;
        let sql = format!(
            "INSERT INTO optimization_results (session_id, {col}, created_at_ns, updated_at_ns)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(session_id) DO UPDATE SET {col} = ?2, updated_at_ns = ?3",
            col = dimension.column()
        );
        conn.execute(&sql, params![session_id, result_json, now_ns])?;
        Ok(())
    }

    /// Fetches the stored results for a session, if any.
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn get(&self, session_id: &str) -> Result<Option<OptimizationRecord>, OptStoreError> {
        let conn = self.conn.lock().map_err(|_| OptStoreError::Poisoned)?;
        let record = conn
            .query_row(
                "SELECT session_id, perf, perf_issues, cost, cost_waste, accuracy, summary,
                        created_at_ns, updated_at_ns
                 FROM optimization_results WHERE session_id = ?1",
                params![session_id],
                Self::map_row,
            )
            .optional()?;
        Ok(record)
    }

    /// Lists records updated within `[start_ns, end_ns]`, newest first.
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn list(
        &self,
        start_ns: i64,
        end_ns: i64,
        limit: usize,
    ) -> Result<Vec<OptimizationRecord>, OptStoreError> {
        let conn = self.conn.lock().map_err(|_| OptStoreError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT session_id, perf, perf_issues, cost, cost_waste, accuracy, summary,
                    created_at_ns, updated_at_ns
             FROM optimization_results
             WHERE updated_at_ns >= ?1 AND updated_at_ns <= ?2
             ORDER BY updated_at_ns DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![start_ns, end_ns, limit as i64], Self::map_row)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// Deletes records older than `cutoff_ns`, returning the number removed.
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn prune_before(&self, cutoff_ns: i64) -> Result<usize, OptStoreError> {
        let conn = self.conn.lock().map_err(|_| OptStoreError::Poisoned)?;
        let removed = conn.execute(
            "DELETE FROM optimization_results WHERE updated_at_ns < ?1",
            params![cutoff_ns],
        )?;
        Ok(removed)
    }

    /// Applies age retention and trims oldest results to 90% of the size limit.
    ///
    /// # Errors
    /// Returns an error on lifecycle, SQLite, or mutex failure.
    pub fn maintain(
        &self,
        policy: OptimizationMaintenancePolicy,
    ) -> Result<OptimizationMaintenanceReport, OptStoreError> {
        let expired_results = if policy.retention_days == 0 {
            0
        } else {
            let cutoff =
                retention_cutoff_ns(u64::try_from(now_ns()).unwrap_or(0), policy.retention_days)?;
            self.prune_before(i64::try_from(cutoff).unwrap_or(i64::MAX))?
        };
        if expired_results > 0 && self.checkpoint()? == CheckpointOutcome::Busy {
            let snapshot = self.size_snapshot()?;
            return Ok(OptimizationMaintenanceReport {
                expired_results,
                size: MaintenanceReport {
                    status: MaintenanceStatus::CheckpointBusy,
                    rounds: 0,
                    deleted_rows: 0,
                    before: snapshot,
                    after: snapshot,
                },
            });
        }

        let limit_bytes = policy.max_db_size_mb.saturating_mul(1024 * 1024);
        let size = enforce_size_policy::<OptStoreError>(
            SizePolicy {
                limit_bytes,
                trigger_bytes: limit_bytes,
                target_bytes: limit_bytes.saturating_mul(9) / 10,
                trigger_basis: SizeBasis::Physical,
                target_basis: SizeBasis::Logical,
                max_rounds: 20,
                max_stalled_rounds: 3,
            },
            || self.size_snapshot(),
            |fraction| self.delete_oldest_fraction(fraction),
            || self.checkpoint(),
        )?;
        Ok(OptimizationMaintenanceReport {
            expired_results,
            size,
        })
    }

    fn delete_oldest_fraction(&self, fraction: f64) -> Result<usize, OptStoreError> {
        let conn = self.conn.lock().map_err(|_| OptStoreError::Poisoned)?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM optimization_results", [], |row| {
                row.get(0)
            })?;
        if count == 0 || fraction <= 0.0 {
            return Ok(0);
        }
        let limit = ((count as f64 * fraction.clamp(0.0, 1.0)) as i64).max(1);
        Ok(conn.execute(
            "DELETE FROM optimization_results WHERE session_id IN (
                SELECT session_id FROM optimization_results
                ORDER BY updated_at_ns ASC, session_id ASC LIMIT ?1
            )",
            params![limit],
        )?)
    }

    fn size_snapshot(&self) -> Result<agentsight_sqlite_lifecycle::SizeSnapshot, OptStoreError> {
        let conn = self.conn.lock().map_err(|_| OptStoreError::Poisoned)?;
        measure_database(&self.db_path, &conn).map_err(Into::into)
    }

    fn checkpoint(&self) -> Result<CheckpointOutcome, OptStoreError> {
        let conn = self.conn.lock().map_err(|_| OptStoreError::Poisoned)?;
        checkpoint_truncate(&conn).map_err(Into::into)
    }

    fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OptimizationRecord> {
        Ok(OptimizationRecord {
            session_id: row.get(0)?,
            perf: row.get(1)?,
            perf_issues: row.get(2)?,
            cost: row.get(3)?,
            cost_waste: row.get(4)?,
            accuracy: row.get(5)?,
            summary: row.get(6)?,
            created_at_ns: row.get(7)?,
            updated_at_ns: row.get(8)?,
        })
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (OptimizationStore, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("opt-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("test-{:?}.db", std::time::Instant::now()));
        (OptimizationStore::new_with_path(&path).unwrap(), path)
    }

    #[test]
    fn save_and_get_roundtrip() {
        let (store, _path) = temp_store();
        assert!(store.get("s1").unwrap().is_none());

        store
            .save_dimension("s1", Dimension::Perf, r#"{"total":1}"#)
            .unwrap();
        store
            .save_dimension("s1", Dimension::Accuracy, r#"{"issues":[]}"#)
            .unwrap();

        let rec = store.get("s1").unwrap().unwrap();
        assert_eq!(rec.session_id, "s1");
        assert_eq!(rec.perf.as_deref(), Some(r#"{"total":1}"#));
        assert_eq!(rec.accuracy.as_deref(), Some(r#"{"issues":[]}"#));
        assert!(rec.cost.is_none());
        assert!(rec.created_at_ns > 0);
    }

    #[test]
    fn save_all_dimensions_populates_all_columns() {
        let (store, _path) = temp_store();
        store
            .save_dimension("s1", Dimension::Perf, "perf-json")
            .unwrap();
        store
            .save_dimension("s1", Dimension::PerfIssues, "perf-issues-json")
            .unwrap();
        store
            .save_dimension("s1", Dimension::Cost, "cost-json")
            .unwrap();
        store
            .save_dimension("s1", Dimension::CostWaste, "cost-waste-json")
            .unwrap();
        store
            .save_dimension("s1", Dimension::Accuracy, "accuracy-json")
            .unwrap();
        store
            .save_dimension("s1", Dimension::Summary, "summary-json")
            .unwrap();

        let rec = store.get("s1").unwrap().unwrap();
        assert_eq!(rec.perf.as_deref(), Some("perf-json"));
        assert_eq!(rec.perf_issues.as_deref(), Some("perf-issues-json"));
        assert_eq!(rec.cost.as_deref(), Some("cost-json"));
        assert_eq!(rec.cost_waste.as_deref(), Some("cost-waste-json"));
        assert_eq!(rec.accuracy.as_deref(), Some("accuracy-json"));
        assert_eq!(rec.summary.as_deref(), Some("summary-json"));
    }

    /// Schema created before the `summary` column existed. `CREATE TABLE IF NOT
    /// EXISTS` is a no-op on such a database, so only the ALTER TABLE migration
    /// can make it usable — this is the regression guard for that path.
    fn create_legacy_db(path: &std::path::Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "CREATE TABLE optimization_results (
                session_id TEXT PRIMARY KEY,
                perf TEXT,
                perf_issues TEXT,
                cost TEXT,
                cost_waste TEXT,
                accuracy TEXT,
                created_at_ns INTEGER NOT NULL,
                updated_at_ns INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO optimization_results (session_id, perf, created_at_ns, updated_at_ns)
             VALUES ('legacy', 'legacy-perf', 1, 1)",
            [],
        )
        .unwrap();
    }

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("opt-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{tag}-{:?}.db", std::time::Instant::now()))
    }

    #[test]
    fn migrates_legacy_db_adding_summary_column() {
        let path = temp_path("legacy");
        create_legacy_db(&path);

        let store = OptimizationStore::new_with_path(&path).unwrap();

        // Pre-existing row survives the migration untouched.
        let legacy = store.get("legacy").unwrap().unwrap();
        assert_eq!(legacy.perf.as_deref(), Some("legacy-perf"));
        assert!(legacy.summary.is_none());

        // And the new dimension is now writable/readable.
        store
            .save_dimension("legacy", Dimension::Summary, "fresh-summary")
            .unwrap();
        assert_eq!(
            store.get("legacy").unwrap().unwrap().summary.as_deref(),
            Some("fresh-summary")
        );
        // list() selects the new column too.
        assert_eq!(store.list(0, i64::MAX, 10).unwrap().len(), 1);
    }

    #[test]
    fn migration_is_idempotent_across_reopens() {
        let path = temp_path("idem");
        create_legacy_db(&path);
        for _ in 0..3 {
            let store = OptimizationStore::new_with_path(&path).unwrap();
            store
                .save_dimension("legacy", Dimension::Summary, "s")
                .unwrap();
        }
        let store = OptimizationStore::new_with_path(&path).unwrap();
        assert_eq!(
            store.get("legacy").unwrap().unwrap().summary.as_deref(),
            Some("s")
        );
    }

    #[test]
    fn upsert_overwrites_dimension() {
        let (store, _path) = temp_store();
        store.save_dimension("s1", Dimension::Cost, "1").unwrap();
        store.save_dimension("s1", Dimension::Cost, "2").unwrap();
        let rec = store.get("s1").unwrap().unwrap();
        assert_eq!(rec.cost.as_deref(), Some("2"));
    }

    #[test]
    fn list_filters_by_time_range() {
        let (store, _path) = temp_store();
        store.save_dimension("s1", Dimension::Perf, "{}").unwrap();
        let all = store.list(0, i64::MAX, 10).unwrap();
        assert_eq!(all.len(), 1);
        let none = store.list(0, 1, 10).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn prune_removes_old_records() {
        let (store, _path) = temp_store();
        store.save_dimension("s1", Dimension::Perf, "{}").unwrap();
        assert_eq!(store.prune_before(i64::MAX).unwrap(), 1);
        assert!(store.get("s1").unwrap().is_none());
    }

    #[test]
    fn read_only_open_requires_an_existing_database() {
        let path = temp_path("read-only");
        assert!(matches!(
            OptimizationStore::open_read_only_existing(&path),
            Err(OptStoreError::Lifecycle(
                agentsight_sqlite_lifecycle::LifecycleError::DatabaseMissing(_)
            ))
        ));

        let writable = OptimizationStore::new_with_path(&path).unwrap();
        writable
            .save_dimension("session", Dimension::Perf, "{}")
            .unwrap();
        drop(writable);

        let read_only = OptimizationStore::open_read_only_existing(&path).unwrap();
        assert!(read_only.get("session").unwrap().is_some());
    }

    #[test]
    fn maintenance_applies_age_retention() {
        let (store, _path) = temp_store();
        store
            .save_dimension("expired", Dimension::Perf, "{}")
            .unwrap();
        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "UPDATE optimization_results SET updated_at_ns = 1 WHERE session_id = ?1",
                params!["expired"],
            )
            .unwrap();
        }

        let report = store
            .maintain(OptimizationMaintenancePolicy {
                retention_days: 1,
                max_db_size_mb: 0,
            })
            .unwrap();

        assert_eq!(report.expired_results, 1);
        assert!(store.get("expired").unwrap().is_none());
    }

    #[test]
    fn maintenance_trims_oldest_results_to_size_target() {
        let (store, _path) = temp_store();
        let payload = "x".repeat(20_000);
        for index in 0..100 {
            store
                .save_dimension(&format!("session-{index:03}"), Dimension::Perf, &payload)
                .unwrap();
        }

        let report = store
            .maintain(OptimizationMaintenancePolicy {
                retention_days: 0,
                max_db_size_mb: 1,
            })
            .unwrap();

        assert!(report.size.deleted_rows > 0);
        assert!(store.get("session-099").unwrap().is_some());
    }
}
