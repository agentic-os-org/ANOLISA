//! Schema initialization, migrations, and size limit management for GenAI SQLite store.

use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use agentsight_sqlite_lifecycle::{
    CheckpointOutcome, MaintenanceStatus, SizeBasis, SizePolicy, checkpoint_truncate,
    enforce_size_policy, measure_database, retention_cutoff_ns,
};
use rusqlite::{Connection, params};

use super::GenAISqliteStore;

/// Percentage of records pruned before retrying a `SQLITE_FULL` write.
const PRUNE_PERCENT: f64 = 0.05;
/// Maximum prune retry attempts to avoid infinite loops.
pub(super) const MAX_PRUNE_RETRIES: u32 = 3;

impl GenAISqliteStore {
    /// Initialize database tables
    pub(super) fn init_tables(&self) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS genai_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                -- call lifecycle status: 'pending' | 'complete' | 'interrupted'
                -- 'pending'     : request captured, waiting for response
                -- 'complete'    : full request+response recorded
                -- 'interrupted' : response never arrived (crash / truncation)
                status TEXT NOT NULL DEFAULT 'complete',
                call_id TEXT,
                trace_id TEXT,
                conversation_id TEXT,
                session_id TEXT,
                instance TEXT,
                start_timestamp_ns INTEGER NOT NULL,
                end_timestamp_ns INTEGER,
                duration_ns INTEGER,
                first_output_timestamp_ns INTEGER,
                pid INTEGER,
                process_name TEXT,
                agent_name TEXT,
                -- GenAI standard fields
                operation_name TEXT,
                provider TEXT,
                model TEXT,
                request_model TEXT,
                response_model TEXT,
                temperature REAL,
                max_tokens INTEGER,
                top_p REAL,
                frequency_penalty REAL,
                presence_penalty REAL,
                finish_reasons TEXT,
                server_address TEXT,
                -- Token usage
                input_tokens INTEGER,
                output_tokens INTEGER,
                total_tokens INTEGER,
                cache_creation_tokens INTEGER,
                cache_read_tokens INTEGER,
                -- Messages (JSON)
                system_instructions TEXT,
                input_messages TEXT,
                output_messages TEXT,
                -- AgentSight extensions
                user_query TEXT,
                http_method TEXT,
                http_path TEXT,
                status_code INTEGER,
                is_sse INTEGER,
                sse_event_count INTEGER,
                -- Interruption type detected for this call (nullable)
                interruption_type TEXT,
                -- Call kind classification (main/recap/web_search)
                call_kind TEXT NOT NULL DEFAULT 'main',
                -- Pending row provenance and reconciliation key
                pending_origin TEXT NOT NULL DEFAULT 'request_capture',
                pending_match_key TEXT,
                -- Full event as JSON (fallback)
                event_json TEXT NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE INDEX IF NOT EXISTS idx_genai_session_id ON genai_events(session_id);
            CREATE INDEX IF NOT EXISTS idx_genai_trace_id ON genai_events(trace_id);
            CREATE INDEX IF NOT EXISTS idx_genai_conversation_id ON genai_events(conversation_id);
            CREATE INDEX IF NOT EXISTS idx_genai_instance ON genai_events(instance);
            CREATE INDEX IF NOT EXISTS idx_genai_start_timestamp ON genai_events(start_timestamp_ns);
            CREATE INDEX IF NOT EXISTS idx_genai_pid ON genai_events(pid);
            CREATE INDEX IF NOT EXISTS idx_genai_model ON genai_events(model);
            CREATE INDEX IF NOT EXISTS idx_genai_call_id ON genai_events(call_id);
            CREATE INDEX IF NOT EXISTS idx_genai_provider ON genai_events(provider);
            -- Composite indexes for common query patterns
            CREATE INDEX IF NOT EXISTS idx_genai_session_timestamp ON genai_events(session_id, start_timestamp_ns);
            CREATE INDEX IF NOT EXISTS idx_genai_trace_timestamp ON genai_events(trace_id, start_timestamp_ns);
            CREATE INDEX IF NOT EXISTS idx_genai_conversation_timestamp ON genai_events(conversation_id, start_timestamp_ns);
            CREATE INDEX IF NOT EXISTS idx_genai_pid_timestamp ON genai_events(pid, start_timestamp_ns);
            CREATE INDEX IF NOT EXISTS idx_genai_instance_timestamp ON genai_events(instance, start_timestamp_ns)",
            // NOTE: idx_genai_status and idx_genai_interruption_type are NOT created here
            // because they depend on columns added via migration. They are created in the
            // migration blocks below, which guarantees the columns exist first.
        )?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_resource_samples (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp_ns INTEGER NOT NULL,
                pid INTEGER NOT NULL,
                agent_name TEXT,
                cpu_percent REAL NOT NULL,
                memory_bytes INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_resource_pid_timestamp
                ON agent_resource_samples(pid, timestamp_ns);
            CREATE INDEX IF NOT EXISTS idx_resource_timestamp
                ON agent_resource_samples(timestamp_ns);",
        )?;

        // ── Forward-compatible migrations ──────────────────────────────────────
        // Each block checks for a column's existence before ALTER TABLE, making
        // all migrations idempotent and safe to run on both old and new databases.
        // Columns are listed in the order they were added historically.

        // Query existing columns once to avoid repeated PRAGMA calls
        let existing_cols: std::collections::HashSet<String> = {
            let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('genai_events')")?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect()
        };

        // Helper macro: ALTER TABLE only if column absent, then always ensure index
        macro_rules! ensure_col {
            // Column with no index
            ($col:literal, $def:literal) => {
                if !existing_cols.contains($col) {
                    conn.execute_batch(&format!(
                        "ALTER TABLE genai_events ADD COLUMN {} {};",
                        $col, $def
                    ))?;
                    log::info!("Migrated genai_events: added '{}' column", $col);
                }
            };
            // Column + index
            ($col:literal, $def:literal, $idx:literal) => {
                if !existing_cols.contains($col) {
                    conn.execute_batch(&format!(
                        "ALTER TABLE genai_events ADD COLUMN {} {};",
                        $col, $def
                    ))?;
                    log::info!("Migrated genai_events: added '{}' column", $col);
                }
                // Always run CREATE INDEX IF NOT EXISTS — safe even if index already exists
                conn.execute_batch(&format!(
                    "CREATE INDEX IF NOT EXISTS {} ON genai_events({});",
                    $idx, $col
                ))?;
            };
        }

        // v2: Anthropic prompt-cache token counters
        ensure_col!("cache_creation_tokens", "INTEGER");
        ensure_col!("cache_read_tokens", "INTEGER");

        // v3: two-phase write lifecycle status
        ensure_col!(
            "status",
            "TEXT NOT NULL DEFAULT 'complete'",
            "idx_genai_status"
        );

        // v4: per-call interruption type
        ensure_col!("interruption_type", "TEXT", "idx_genai_interruption_type");

        // Migration: add conversation_id column for existing databases
        let _ = conn.execute(
            "ALTER TABLE genai_events ADD COLUMN conversation_id TEXT",
            [],
        );

        // v5: tool_call_ids JSON array for output tool calls
        ensure_col!("tool_call_ids", "TEXT");

        // v6: call_kind classification (main/recap/web_search)
        ensure_col!(
            "call_kind",
            "TEXT NOT NULL DEFAULT 'main'",
            "idx_genai_call_kind"
        );

        // v7: pending provenance for idle/drain lifecycle handling
        ensure_col!(
            "pending_origin",
            "TEXT NOT NULL DEFAULT 'request_capture'",
            "idx_genai_pending_origin"
        );

        // v8: stable key used to reconcile idle stream snapshots on completion
        ensure_col!("pending_match_key", "TEXT", "idx_genai_pending_match_key");

        // v9: first provider event that carries model output
        ensure_col!("first_output_timestamp_ns", "INTEGER");

        Ok(())
    }

    // ─── Size limit methods ───────────────────────────────────────────────────

    pub(super) fn size_snapshot(
        &self,
    ) -> Result<agentsight_sqlite_lifecycle::SizeSnapshot, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        measure_database(&self.db_path, &conn).map_err(Into::into)
    }

    #[cfg(test)]
    pub(super) fn get_total_db_size(&self) -> u64 {
        self.size_snapshot()
            .map(|snapshot| snapshot.physical_bytes)
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(super) fn effective_db_size(&self) -> u64 {
        self.size_snapshot()
            .map(|snapshot| snapshot.logical_bytes)
            .unwrap_or(0)
    }

    /// Runs configured maintenance after the configured number of writes.
    pub(super) fn check_and_prune_if_needed(&self) -> Result<(), Box<dyn std::error::Error>> {
        let interval = self.storage_policy.check_interval_inserts;
        if interval == 0 {
            return Ok(());
        }
        let count = self
            .maintenance_insert_count
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if !count.is_multiple_of(interval) {
            return Ok(());
        }
        self.run_maintenance()
    }

    /// Applies age retention followed by capacity enforcement.
    pub(super) fn run_maintenance(&self) -> Result<(), Box<dyn std::error::Error>> {
        let deleted_by_age = self.purge_expired()?;
        if deleted_by_age > 0 && self.checkpoint_outcome()? == CheckpointOutcome::Busy {
            log::warn!("GenAI WAL checkpoint remained busy after age retention");
            return Ok(());
        }

        let limit_bytes = self
            .storage_policy
            .max_db_size_mb
            .saturating_mul(1024 * 1024);
        let policy = SizePolicy {
            limit_bytes,
            trigger_bytes: limit_bytes,
            target_bytes: limit_bytes.saturating_mul(9) / 10,
            trigger_basis: SizeBasis::Physical,
            target_basis: SizeBasis::Logical,
            max_rounds: 20,
            max_stalled_rounds: 3,
        };
        let report = enforce_size_policy::<Box<dyn std::error::Error>>(
            policy,
            || self.size_snapshot(),
            |fraction| self.prune_old_records_with_percent(fraction),
            || self.checkpoint_outcome(),
        )?;
        match report.status {
            MaintenanceStatus::CheckpointBusy => log::warn!(
                "GenAI size maintenance stopped at a busy checkpoint after deleting {} rows",
                report.deleted_rows
            ),
            MaintenanceStatus::Stalled | MaintenanceStatus::MaxRounds => log::warn!(
                "GenAI size maintenance stopped with {:?} at {} logical bytes",
                report.status,
                report.after.logical_bytes
            ),
            _ => {}
        }
        Ok(())
    }

    fn purge_expired(&self) -> Result<usize, Box<dyn std::error::Error>> {
        if self.storage_policy.retention_days == 0 {
            return Ok(0);
        }
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let cutoff_ns = retention_cutoff_ns(now_ns, self.storage_policy.retention_days)?;
        let cutoff_i64 = i64::try_from(cutoff_ns).unwrap_or(i64::MAX);
        let cutoff_seconds = cutoff_i64 / 1_000_000_000;
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let mut deleted = conn.execute(
            "DELETE FROM genai_events WHERE start_timestamp_ns < ?1",
            params![cutoff_i64],
        )?;
        deleted += conn.execute(
            "DELETE FROM agent_resource_samples WHERE timestamp_ns < ?1",
            params![cutoff_i64],
        )?;
        if table_exists(&conn, "evaluation_runs")? {
            deleted += conn.execute(
                "DELETE FROM evaluation_runs
                 WHERE CAST(strftime('%s', created_at) AS INTEGER) < ?1",
                params![cutoff_seconds],
            )?;
        }
        Ok(deleted)
    }

    /// Prune old records using the default 5% ratio.
    ///
    /// Thin wrapper around [`prune_old_records_with_percent`] for callers that
    /// only need the conservative default (e.g. SQLITE_FULL retry).
    pub(super) fn prune_old_records(&self) -> Result<usize, Box<dyn std::error::Error>> {
        self.prune_old_records_with_percent(PRUNE_PERCENT)
    }

    /// Delete the oldest `percent` fraction from every lifecycle-owned table.
    fn prune_old_records_with_percent(
        &self,
        percent: f64,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        let pct = percent.clamp(0.0, 1.0);
        if pct == 0.0 {
            return Ok(0);
        }
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());

        let event_count = row_count(&conn, "genai_events")?;
        let resource_count = row_count(&conn, "agent_resource_samples")?;
        let evaluation_count = if table_exists(&conn, "evaluation_runs")? {
            row_count(&conn, "evaluation_runs")?
        } else {
            0
        };
        let event_delete_count = prune_count(event_count, pct);
        let resource_delete_count = prune_count(resource_count, pct);
        let evaluation_delete_count = prune_count(evaluation_count, pct);

        let deleted_events = conn.execute(
            "DELETE FROM genai_events WHERE id IN (
                SELECT id FROM genai_events ORDER BY start_timestamp_ns ASC, id ASC LIMIT ?1
            )",
            params![event_delete_count],
        )?;
        let deleted_resources = conn.execute(
            "DELETE FROM agent_resource_samples WHERE id IN (
                SELECT id FROM agent_resource_samples ORDER BY timestamp_ns ASC, id ASC LIMIT ?1
            )",
            params![resource_delete_count],
        )?;
        let deleted_evaluations = if evaluation_count > 0 {
            conn.execute(
                "DELETE FROM evaluation_runs WHERE id IN (
                    SELECT id FROM evaluation_runs ORDER BY created_at ASC, id ASC LIMIT ?1
                )",
                params![evaluation_delete_count],
            )?
        } else {
            0
        };

        Ok(deleted_events + deleted_resources + deleted_evaluations)
    }

    fn checkpoint_outcome(&self) -> Result<CheckpointOutcome, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        checkpoint_truncate(&conn).map_err(Into::into)
    }

    /// Flush WAL frames to the main database and truncate the WAL file.
    ///
    /// Returns `Ok(true)` when another reader keeps the checkpoint busy.
    pub fn wal_checkpoint(&self) -> Result<bool, Box<dyn std::error::Error>> {
        Ok(self.checkpoint_outcome()? == CheckpointOutcome::Busy)
    }
}

fn table_exists(conn: &Connection, table_name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1
        )",
        params![table_name],
        |row| row.get(0),
    )
}

fn row_count(conn: &Connection, table_name: &str) -> rusqlite::Result<i64> {
    match table_name {
        "genai_events" => conn.query_row("SELECT COUNT(*) FROM genai_events", [], |row| row.get(0)),
        "agent_resource_samples" => {
            conn.query_row("SELECT COUNT(*) FROM agent_resource_samples", [], |row| {
                row.get(0)
            })
        }
        "evaluation_runs" => {
            conn.query_row("SELECT COUNT(*) FROM evaluation_runs", [], |row| row.get(0))
        }
        _ => Err(rusqlite::Error::InvalidParameterName(
            table_name.to_string(),
        )),
    }
}

fn prune_count(count: i64, fraction: f64) -> i64 {
    if count > 0 {
        ((count as f64) * fraction).max(1.0) as i64
    } else {
        0
    }
}
