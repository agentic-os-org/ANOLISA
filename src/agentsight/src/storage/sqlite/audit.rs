//! Audit record storage
//!
//! Handles table creation, record insertion, and querying for audit events.

use crate::analyzer::{AuditEventType, AuditExtra, AuditRecord, AuditSummary};
use agentsight_sqlite_lifecycle::{
    CheckpointOutcome, ConnectionMode, ConnectionOptions, SizeSnapshot, checkpoint_truncate,
    measure_database, open_connection,
};
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

/// SQLite-based audit event store
pub struct AuditStore {
    conn: Connection,
    table_name: String,
}

impl AuditStore {
    /// Create a new AuditStore with default table name, automatically creating the table and indexes
    pub fn new(path: &Path) -> Result<Self> {
        Self::with_table(path, "audit_events")
    }

    /// Create a new AuditStore with custom table name
    pub fn with_table(path: &Path, table_name: &str) -> Result<Self> {
        let conn = open_connection(path, ConnectionOptions::default())?;
        let table_name = table_name.to_string();

        // Create table and indexes with dynamic table name
        let create_table_sql = format!(
            "CREATE TABLE IF NOT EXISTS {table_name} (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type    TEXT NOT NULL,
                timestamp_ns  INTEGER NOT NULL,
                pid           INTEGER NOT NULL,
                ppid          INTEGER,
                comm          TEXT NOT NULL,
                duration_ns   INTEGER DEFAULT 0,
                extra         TEXT
            );"
        );
        let create_index_sql = format!(
            "CREATE INDEX IF NOT EXISTS idx_{table_name}_ts ON {table_name}(timestamp_ns);
             CREATE INDEX IF NOT EXISTS idx_{table_name}_type ON {table_name}(event_type);
             CREATE INDEX IF NOT EXISTS idx_{table_name}_pid ON {table_name}(pid);"
        );
        conn.execute_batch(&format!("{create_table_sql}{create_index_sql}"))?;

        // Idempotent migration (#1025): add session correlation columns
        // to pre-existing databases.
        ensure_correlation_columns(&conn, &table_name)?;

        Ok(AuditStore { conn, table_name })
    }

    /// Opens an existing audit table without creating or modifying the database.
    pub fn open_read_only_existing(path: &Path, table_name: &str) -> Result<Self> {
        let conn = open_connection(
            path,
            ConnectionOptions {
                mode: ConnectionMode::ReadOnlyExisting,
                enable_wal: false,
                ..ConnectionOptions::default()
            },
        )?;
        Ok(Self {
            conn,
            table_name: table_name.to_string(),
        })
    }

    /// Default database path: ~/.agentsight/audit.db
    pub fn default_path() -> PathBuf {
        crate::config::default_base_path().join("audit.db")
    }

    /// Insert an audit record, returns the row ID
    pub fn insert(&self, record: &AuditRecord) -> Result<i64> {
        let event_type_str = record.event_type.to_string();
        let extra_json =
            serde_json::to_string(&record.extra).context("Failed to serialize extra")?;

        let sql = format!(
            "INSERT INTO {} (event_type, timestamp_ns, pid, ppid, comm, duration_ns, extra, session_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            self.table_name
        );
        self.conn.execute(
            &sql,
            params![
                event_type_str,
                record.timestamp_ns as i64,
                record.pid,
                record.ppid.map(|v| v as i64),
                record.comm,
                record.duration_ns as i64,
                extra_json,
                record.session_id,
            ],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Query audit events since a given timestamp
    pub fn query_since(
        &self,
        since_ns: u64,
        event_type: Option<AuditEventType>,
    ) -> Result<Vec<AuditRecord>> {
        let (sql, type_str);
        let query_params: Vec<Box<dyn rusqlite::types::ToSql>>;

        if let Some(et) = event_type {
            type_str = et.to_string();
            sql = format!(
                "SELECT id, event_type, timestamp_ns, pid, ppid, comm, duration_ns, extra, session_id
                 FROM {} WHERE timestamp_ns >= ?1 AND event_type = ?2
                 ORDER BY timestamp_ns ASC",
                self.table_name
            );
            query_params = vec![Box::new(since_ns as i64), Box::new(type_str.clone())];
        } else {
            sql = format!(
                "SELECT id, event_type, timestamp_ns, pid, ppid, comm, duration_ns, extra, session_id
                 FROM {} WHERE timestamp_ns >= ?1
                 ORDER BY timestamp_ns ASC",
                self.table_name
            );
            query_params = vec![Box::new(since_ns as i64)];
        }

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            query_params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_refs.as_slice(), |row| Ok(row_to_record(row)))?;

        let mut records = Vec::new();
        for row in rows {
            match row {
                Ok(Ok(record)) => records.push(record),
                Ok(Err(e)) => log::warn!("Failed to parse audit record: {e}"),
                Err(e) => log::warn!("Failed to read row: {e}"),
            }
        }

        Ok(records)
    }

    /// Query audit events by PID
    pub fn query_by_pid(
        &self,
        pid: u32,
        event_type: Option<AuditEventType>,
    ) -> Result<Vec<AuditRecord>> {
        let (sql, type_str);
        let query_params: Vec<Box<dyn rusqlite::types::ToSql>>;

        if let Some(et) = event_type {
            type_str = et.to_string();
            sql = format!(
                "SELECT id, event_type, timestamp_ns, pid, ppid, comm, duration_ns, extra, session_id
                 FROM {} WHERE pid = ?1 AND event_type = ?2
                 ORDER BY timestamp_ns ASC",
                self.table_name
            );
            query_params = vec![Box::new(pid), Box::new(type_str.clone())];
        } else {
            sql = format!(
                "SELECT id, event_type, timestamp_ns, pid, ppid, comm, duration_ns, extra, session_id
                 FROM {} WHERE pid = ?1
                 ORDER BY timestamp_ns ASC",
                self.table_name
            );
            query_params = vec![Box::new(pid)];
        }

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            query_params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_refs.as_slice(), |row| Ok(row_to_record(row)))?;

        let mut records = Vec::new();
        for row in rows {
            match row {
                Ok(Ok(record)) => records.push(record),
                Ok(Err(e)) => log::warn!("Failed to parse audit record: {e}"),
                Err(e) => log::warn!("Failed to read row: {e}"),
            }
        }

        Ok(records)
    }

    /// Purge records older than the given timestamp
    ///
    /// Returns the number of deleted rows.
    pub fn purge_before(&self, cutoff_ns: u64) -> Result<u64> {
        let sql = format!("DELETE FROM {} WHERE timestamp_ns < ?1", self.table_name);
        let deleted = self.conn.execute(&sql, params![cutoff_ns as i64])?;
        Ok(deleted as u64)
    }

    /// Number of records in the table.
    ///
    /// Used by size-based purging to size deletion batches proportionally.
    /// The table name is interpolated, not parameterized (SQL placeholders do
    /// not support identifiers); it is built internally from config, never
    /// from user input.
    pub fn count(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM {}", self.table_name),
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Delete the oldest N records by timestamp.
    ///
    /// Used for size-based pruning when the database file exceeds its
    /// configured maximum, regardless of record age.
    pub fn delete_oldest_batch(&self, limit: usize) -> Result<usize> {
        let sql = format!(
            "DELETE FROM {} WHERE id IN (
                SELECT id FROM {} ORDER BY timestamp_ns ASC LIMIT ?1
            )",
            self.table_name, self.table_name
        );
        let deleted = self.conn.execute(&sql, params![limit as i64])?;
        Ok(deleted)
    }

    /// Run VACUUM to reclaim free pages after bulk deletes.
    ///
    /// Since all stores in `Storage` share one database file, calling
    /// this on any store vacuums the entire file. Re-enables WAL mode
    /// afterwards as a defensive measure (VACUUM may reset it).
    pub fn vacuum(&self) -> Result<()> {
        self.conn
            .execute_batch("VACUUM; PRAGMA journal_mode=WAL;")?;
        Ok(())
    }

    /// Execute WAL checkpoint to flush WAL data back to the main database file.
    pub fn checkpoint(&self) -> Result<()> {
        match checkpoint_truncate(&self.conn)? {
            CheckpointOutcome::Completed => Ok(()),
            CheckpointOutcome::Busy => anyhow::bail!("WAL checkpoint remained busy"),
        }
    }

    /// Truncating WAL checkpoint used by size-based maintenance.
    pub fn checkpoint_outcome(&self) -> Result<CheckpointOutcome> {
        checkpoint_truncate(&self.conn).map_err(Into::into)
    }

    /// Measures physical allocation and reusable freelist capacity.
    pub fn size_snapshot(&self) -> Result<SizeSnapshot> {
        let path = self
            .conn
            .path()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("audit store has no database path"))?;
        measure_database(&path, &self.conn).map_err(Into::into)
    }

    /// Get summary statistics since a given timestamp
    pub fn summary(&self, since_ns: u64) -> Result<AuditSummary> {
        // Count by event type
        let total_llm_calls: u64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM {} WHERE timestamp_ns >= ?1 AND event_type = 'llm_call'",
                self.table_name
            ),
            params![since_ns as i64],
            |row| row.get(0),
        )?;

        let total_process_actions: u64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM {} WHERE timestamp_ns >= ?1 AND event_type = 'process_action'",
                self.table_name
            ),
            params![since_ns as i64],
            |row| row.get(0),
        )?;

        // Sum tokens from extra JSON (for llm_call events)
        let mut total_input_tokens: u64 = 0;
        let mut total_output_tokens: u64 = 0;
        let mut provider_counts: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();

        {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT extra FROM {} WHERE timestamp_ns >= ?1 AND event_type = 'llm_call'",
                self.table_name
            ))?;
            let rows = stmt.query_map(params![since_ns as i64], |row| {
                let extra_str: String = row.get(0)?;
                Ok(extra_str)
            })?;

            for row in rows.flatten() {
                if let Ok(extra) = serde_json::from_str::<serde_json::Value>(&row) {
                    total_input_tokens += extra
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    total_output_tokens += extra
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if let Some(provider) = extra.get("provider").and_then(|v| v.as_str()) {
                        *provider_counts.entry(provider.to_string()).or_insert(0) += 1;
                    }
                }
            }
        }

        // Top commands (for process_action events)
        // Extract full command line from extra.args, fallback to comm
        let mut cmd_counts: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        {
            let mut stmt = self.conn.prepare(
                &format!(
                    "SELECT comm, extra FROM {} WHERE timestamp_ns >= ?1 AND event_type = 'process_action'",
                    self.table_name
                ),
            )?;
            let rows = stmt.query_map(params![since_ns as i64], |row| {
                let comm: String = row.get(0)?;
                let extra_str: String = row.get(1)?;
                Ok((comm, extra_str))
            })?;

            for row in rows.flatten() {
                let (comm, extra_str) = row;
                // Try to extract args from extra JSON, fallback to comm
                let cmdline = extract_cmdline_from_extra(&extra_str, &comm);
                *cmd_counts.entry(cmdline).or_insert(0) += 1;
            }
        }

        let mut providers: Vec<(String, u64)> = provider_counts.into_iter().collect();
        providers.sort_by_key(|entry| std::cmp::Reverse(entry.1));

        let mut top_commands: Vec<(String, u64)> = cmd_counts.into_iter().collect();
        top_commands.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        top_commands.truncate(10);

        Ok(AuditSummary {
            total_llm_calls,
            total_process_actions,
            total_input_tokens,
            total_output_tokens,
            providers,
            top_commands,
        })
    }
}

/// Parse a database row into an AuditRecord
fn row_to_record(row: &rusqlite::Row) -> Result<AuditRecord> {
    let id: i64 = row.get(0).map_err(|e| anyhow::anyhow!("{e}"))?;
    let event_type_str: String = row.get(1).map_err(|e| anyhow::anyhow!("{e}"))?;
    let timestamp_ns: i64 = row.get(2).map_err(|e| anyhow::anyhow!("{e}"))?;
    let pid: u32 = row.get(3).map_err(|e| anyhow::anyhow!("{e}"))?;
    let ppid: Option<u32> = row.get(4).map_err(|e| anyhow::anyhow!("{e}"))?;
    let comm: String = row.get(5).map_err(|e| anyhow::anyhow!("{e}"))?;
    let duration_ns: i64 = row.get(6).map_err(|e| anyhow::anyhow!("{e}"))?;
    let extra_str: String = row.get(7).map_err(|e| anyhow::anyhow!("{e}"))?;
    let session_id: Option<String> = row.get(8).map_err(|e| anyhow::anyhow!("{e}"))?;

    let event_type: AuditEventType = event_type_str
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{e}"))?;

    let extra: AuditExtra =
        serde_json::from_str(&extra_str).context("Failed to deserialize extra JSON")?;

    Ok(AuditRecord {
        id: Some(id),
        event_type,
        timestamp_ns: timestamp_ns as u64,
        pid,
        ppid,
        comm,
        duration_ns: duration_ns as u64,
        extra,
        session_id,
    })
}

/// Idempotent migration: ensure session correlation columns exist.
///
/// Databases created before #1025 lack `session_id` and `conversation_id`
/// columns. Add them via `ALTER TABLE` only when missing (checked through
/// `pragma_table_info`). Also create an index on `session_id`.
fn ensure_correlation_columns(conn: &Connection, table_name: &str) -> Result<()> {
    let existing: std::collections::HashSet<String> = {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table_name})"))?;
        stmt.query_map([], |row| row.get::<_, String>(1))? // column 1 = name
            .filter_map(|r| r.ok())
            .collect()
    };

    if !existing.contains("session_id") {
        conn.execute_batch(&format!(
            "ALTER TABLE {table_name} ADD COLUMN session_id TEXT;"
        ))?;
    }
    if !existing.contains("conversation_id") {
        conn.execute_batch(&format!(
            "ALTER TABLE {table_name} ADD COLUMN conversation_id TEXT;"
        ))?;
    }
    conn.execute_batch(&format!(
        "CREATE INDEX IF NOT EXISTS idx_{table_name}_session_id ON {table_name}(session_id);"
    ))?;
    Ok(())
}

/// Extract full command line from extra JSON (ProcessAction.args or filename)
/// Falls back to comm if parsing fails or fields are empty
fn extract_cmdline_from_extra(extra_str: &str, comm: &str) -> String {
    // Try to parse as ProcessAction extra
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(extra_str) {
        // Try args first (full command line)
        if let Some(args) = json.get("args").and_then(|v| v.as_str()) {
            if !args.is_empty() {
                return args.to_string();
            }
        }
        // Try filename as fallback
        if let Some(filename) = json.get("filename").and_then(|v| v.as_str()) {
            if !filename.is_empty() {
                return filename.to_string();
            }
        }
    }
    // Fallback to comm (process name, max 16 chars)
    comm.to_string()
}

// Backward compatibility alias
pub type SqliteStore = AuditStore;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_id_round_trip() {
        // Use an in-memory Connection to avoid tempfile dependency, then manually
        // replicate what AuditStore::new does (create table + migration).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE audit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                timestamp_ns INTEGER NOT NULL,
                pid INTEGER NOT NULL,
                ppid INTEGER,
                comm TEXT NOT NULL,
                duration_ns INTEGER DEFAULT 0,
                extra TEXT
            );",
        )
        .unwrap();
        ensure_correlation_columns(&conn, "audit_events").unwrap();

        let store = AuditStore {
            conn,
            table_name: "audit_events".to_string(),
        };

        let record = AuditRecord {
            id: None,
            event_type: AuditEventType::ProcessAction,
            timestamp_ns: 1_000_000_000,
            pid: 42,
            ppid: Some(1),
            comm: "bash".to_string(),
            duration_ns: 500_000,
            extra: AuditExtra::ProcessAction {
                filename: Some("/bin/bash".to_string()),
                args: Some("echo hi".to_string()),
                exit_code: None,
            },
            session_id: Some("sess-abc-123".to_string()),
        };
        store.insert(&record).unwrap();

        let results = store.query_since(0, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].session_id.as_deref(),
            Some("sess-abc-123"),
            "session_id must survive insert→query round-trip"
        );
    }

    #[test]
    fn test_session_id_none_round_trip() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE audit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                timestamp_ns INTEGER NOT NULL,
                pid INTEGER NOT NULL,
                ppid INTEGER,
                comm TEXT NOT NULL,
                duration_ns INTEGER DEFAULT 0,
                extra TEXT
            );",
        )
        .unwrap();
        ensure_correlation_columns(&conn, "audit_events").unwrap();

        let store = AuditStore {
            conn,
            table_name: "audit_events".to_string(),
        };

        let record = AuditRecord {
            id: None,
            event_type: AuditEventType::ProcessAction,
            timestamp_ns: 2_000_000_000,
            pid: 43,
            ppid: None,
            comm: "ls".to_string(),
            duration_ns: 100_000,
            extra: AuditExtra::ProcessAction {
                filename: Some("/bin/ls".to_string()),
                args: Some("ls -la".to_string()),
                exit_code: Some(0),
            },
            session_id: None,
        };
        store.insert(&record).unwrap();

        let results = store.query_by_pid(43, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, None);
    }

    #[test]
    fn test_ensure_correlation_columns_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        // Pre-existing audit table WITHOUT the correlation columns (old DB).
        conn.execute_batch(
            "CREATE TABLE audit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                timestamp_ns INTEGER NOT NULL,
                pid INTEGER NOT NULL,
                comm TEXT NOT NULL,
                extra TEXT
            );",
        )
        .unwrap();

        // Run twice: must be idempotent. A naive ALTER without the pragma guard
        // would error "duplicate column name" on the second call.
        ensure_correlation_columns(&conn, "audit_events").unwrap();
        ensure_correlation_columns(&conn, "audit_events").unwrap();

        let cols: std::collections::HashSet<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(audit_events)").unwrap();
            stmt.query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert!(cols.contains("session_id"));
        assert!(cols.contains("conversation_id"));

        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_audit_events_session_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 1);
    }
}
