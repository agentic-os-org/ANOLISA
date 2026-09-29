//! SQLite storage for interruption_events table.

mod maintenance;

use agentsight_sqlite_lifecycle::{ConnectionMode, ConnectionOptions, open_connection};
use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::sync::Mutex;

use crate::interruption::{InterruptionEvent, InterruptionType};

/// Bucket key reported by the per-session breakdown for events whose
/// `session_id` could never be resolved.
///
/// Grouping them under a sentinel instead of dropping the rows keeps the
/// overview total equal to the sum of the breakdown; the dashboard renders
/// this key as an "unassigned" row. The double-underscore shape cannot
/// collide with a real session id (a UUID or a 32-hex fallback hash).
pub const UNASSIGNED_SESSION_ID: &str = "__unassigned__";

/// Bucket key used by the per-conversation breakdown for NULL
/// `conversation_id`. Mirrors [`UNASSIGNED_SESSION_ID`].
pub const UNASSIGNED_CONVERSATION_ID: &str = "__unassigned__";

// ─── API response types ────────────────────────────────────────────────────────

/// Summary returned by GET /api/interruptions
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InterruptionRecord {
    pub id: i64,
    pub interruption_id: String,
    pub session_id: Option<String>,
    pub trace_id: Option<String>,
    pub conversation_id: Option<String>,
    pub call_id: Option<String>,
    pub pid: Option<i64>,
    pub agent_name: Option<String>,
    pub interruption_type: String,
    pub severity: String,
    pub occurred_at_ns: i64,
    pub detail: Option<String>,
    pub resolved: bool,
}

/// Per-type count for stats endpoint
#[derive(Debug, serde::Serialize)]
pub struct InterruptionTypeStat {
    pub interruption_type: String,
    pub severity: String,
    pub count: i64,
}

// ─── Store ────────────────────────────────────────────────────────────────────

pub struct InterruptionStore {
    conn: Mutex<Connection>,
    db_path: PathBuf,
}

impl InterruptionStore {
    /// Opens an interruption store and initializes its schema.
    pub fn new_with_path(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        let conn = open_connection(path, ConnectionOptions::default())?;
        let store = InterruptionStore {
            conn: Mutex::new(conn),
            db_path: path.to_path_buf(),
        };
        store.init_tables()?;
        Ok(store)
    }

    /// Opens an existing interruption database without creating or modifying it.
    pub fn open_read_only_existing(
        path: &std::path::Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
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

    fn init_tables(&self) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // Migration must run before any index that references the added
        // column: on databases created before conversation_id existed,
        // creating that index first fails and the `?` below would return
        // before this migration ever ran (issue #3314).
        let _ =
            conn.execute_batch("ALTER TABLE interruption_events ADD COLUMN conversation_id TEXT;");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS interruption_events (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                interruption_id     TEXT NOT NULL UNIQUE,
                session_id          TEXT,
                trace_id            TEXT,
                conversation_id     TEXT,
                call_id             TEXT,
                pid                 INTEGER,
                agent_name          TEXT,
                interruption_type   TEXT NOT NULL,
                severity            TEXT NOT NULL,
                occurred_at_ns      INTEGER NOT NULL,
                detail              TEXT,
                resolved            INTEGER NOT NULL DEFAULT 0,
                created_at          DATETIME DEFAULT CURRENT_TIMESTAMP
            );
            CREATE INDEX IF NOT EXISTS idx_interruption_session  ON interruption_events(session_id);
            CREATE INDEX IF NOT EXISTS idx_interruption_occurred ON interruption_events(occurred_at_ns);
            CREATE INDEX IF NOT EXISTS idx_interruption_type     ON interruption_events(interruption_type);
            CREATE INDEX IF NOT EXISTS idx_interruption_agent    ON interruption_events(agent_name);
            CREATE INDEX IF NOT EXISTS idx_interruption_resolved ON interruption_events(resolved);
            CREATE INDEX IF NOT EXISTS idx_interruption_conversation ON interruption_events(conversation_id);
            CREATE TABLE IF NOT EXISTS process_exits (
                pid           INTEGER PRIMARY KEY,
                raw_exit_code INTEGER NOT NULL,
                exited_at_ns  INTEGER NOT NULL
            );",
        )?;
        Ok(())
    }

    // ─── Write ──────────────────────────────────────────────────────────────

    /// Insert a single interruption event (ignores duplicates by interruption_id).
    pub fn insert(&self, event: &InterruptionEvent) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT OR IGNORE INTO interruption_events (
                interruption_id, session_id, trace_id, conversation_id, call_id, pid, agent_name,
                interruption_type, severity, occurred_at_ns, detail, resolved
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                event.interruption_id,
                event.session_id,
                event.trace_id,
                event.conversation_id,
                event.call_id,
                event.pid,
                event.agent_name,
                event.interruption_type.as_str(),
                event.severity.as_str(),
                event.occurred_at_ns,
                event.detail,
                event.resolved as i32,
            ],
        )?;
        Ok(())
    }

    /// Insert multiple events, ignoring duplicates.
    pub fn insert_batch(
        &self,
        events: &[InterruptionEvent],
    ) -> Result<(), Box<dyn std::error::Error>> {
        for e in events {
            self.insert(e)?;
        }
        Ok(())
    }

    /// Deduplication check for OOM events: return true if an agent_crash row
    /// with oom=true already exists for the given (pid, occurred_at_ns).
    pub fn oom_event_exists(&self, pid: i32, occurred_at_ns: i64) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT COUNT(*) FROM interruption_events
             WHERE interruption_type='agent_crash' AND pid=?1 AND occurred_at_ns=?2
               AND detail LIKE '%\"oom\":true%'",
            params![pid, occurred_at_ns],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    }

    /// Return the maximum occurred_at_ns of OOM-sourced agent_crash events.
    /// Returns 0 if no such events exist.
    pub fn latest_oom_event_ns(&self) -> i64 {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT COALESCE(MAX(occurred_at_ns), 0) FROM interruption_events
             WHERE interruption_type='agent_crash' AND detail LIKE '%\"oom\":true%'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    /// Deduplication check: return true if a row with same call_id + type already exists.
    pub fn exists_for_call(&self, call_id: &str, itype: &InterruptionType) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT COUNT(*) FROM interruption_events WHERE call_id=?1 AND interruption_type=?2",
            params![call_id, itype.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    }

    /// Deduplication check: return true if an unresolved row with same
    /// conversation_id + interruption_type + error message already exists.
    ///
    /// When `error_msg` is Some, uses keyword-based matching: the error is
    /// normalized to a core message (stripping nested JSON wrappers) and
    /// compared via substring containment.  This handles cases where the same
    /// error appears as a clean message in one call and as raw JSON in another.
    /// When `error_msg` is None, any unresolved row with same (conversation_id, type) matches.
    pub fn exists_for_conversation(
        &self,
        conversation_id: &str,
        itype: &InterruptionType,
        error_msg: Option<&str>,
    ) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT detail FROM interruption_events
             WHERE conversation_id=?1 AND interruption_type=?2 AND resolved=0",
        ) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let rows = match stmt.query_map(params![conversation_id, itype.as_str()], |row| {
            row.get::<_, Option<String>>(0)
        }) {
            Ok(r) => r,
            Err(_) => return false,
        };

        for detail_opt in rows.flatten() {
            match error_msg {
                None => {
                    // No error_msg filter — any existing row is a duplicate
                    return true;
                }
                Some(target) => {
                    // Compare normalized error keys (handles nested JSON vs clean message)
                    if let Some(ref detail_str) = detail_opt {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(detail_str) {
                            let stored_error =
                                v.get("error").and_then(|e| e.as_str()).unwrap_or("");
                            if errors_match(stored_error, target) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

    /// Mark an interruption as resolved.
    pub fn resolve(&self, interruption_id: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let updated = conn.execute(
            "UPDATE interruption_events SET resolved=1 WHERE interruption_id=?1",
            params![interruption_id],
        )?;
        Ok(updated > 0)
    }

    /// Count unresolved interruptions of a given type for a conversation.
    ///
    /// Used by the DeadLoop auto-kill feature to determine whether the kill
    /// threshold has been reached.
    pub fn count_for_conversation(&self, conversation_id: &str, itype: &InterruptionType) -> usize {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let result: Result<i64, _> = conn.query_row(
            "SELECT COUNT(*) FROM interruption_events
             WHERE conversation_id=?1 AND interruption_type=?2 AND resolved=0",
            params![conversation_id, itype.as_str()],
            |row| row.get(0),
        );
        result.unwrap_or(0) as usize
    }

    // ─── Query ──────────────────────────────────────────────────────────────

    /// List interruptions within a time range.
    pub fn list(
        &self,
        start_ns: i64,
        end_ns: i64,
        agent_name: Option<&str>,
        itype: Option<&str>,
        severity: Option<&str>,
        resolved: Option<bool>,
        limit: i64,
    ) -> Result<Vec<InterruptionRecord>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        // Build dynamic WHERE clause
        let mut conditions = vec!["occurred_at_ns BETWEEN ?1 AND ?2".to_string()];
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(start_ns), Box::new(end_ns)];
        let mut idx = 3usize;

        if let Some(a) = agent_name {
            conditions.push(format!("agent_name = ?{idx}"));
            args.push(Box::new(a.to_string()));
            idx += 1;
        }
        if let Some(t) = itype {
            conditions.push(format!("interruption_type = ?{idx}"));
            args.push(Box::new(t.to_string()));
            idx += 1;
        }
        if let Some(s) = severity {
            conditions.push(format!("severity = ?{idx}"));
            args.push(Box::new(s.to_string()));
            idx += 1;
        }
        if let Some(r) = resolved {
            conditions.push(format!("resolved = ?{idx}"));
            args.push(Box::new(r as i32));
            idx += 1;
        }
        let _ = idx;

        let sql = format!(
            "SELECT id, interruption_id, session_id, trace_id, conversation_id, call_id, pid, agent_name,
                    interruption_type, severity, occurred_at_ns, detail, resolved
             FROM interruption_events
             WHERE {}
             ORDER BY occurred_at_ns DESC
             LIMIT ?{}",
            conditions.join(" AND "),
            args.len() + 1,
        );
        args.push(Box::new(limit));

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_refs.as_slice(), |row| {
            Ok(InterruptionRecord {
                id: row.get(0)?,
                interruption_id: row.get(1)?,
                session_id: row.get(2)?,
                trace_id: row.get(3)?,
                conversation_id: row.get(4)?,
                call_id: row.get(5)?,
                pid: row.get(6)?,
                agent_name: row.get(7)?,
                interruption_type: row.get(8)?,
                severity: row.get(9)?,
                occurred_at_ns: row.get(10)?,
                detail: row.get(11)?,
                resolved: row.get::<_, i32>(12)? != 0,
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Get a single interruption by ID.
    pub fn get_by_id(
        &self,
        interruption_id: &str,
    ) -> Result<Option<InterruptionRecord>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, interruption_id, session_id, trace_id, conversation_id, call_id, pid, agent_name,
                    interruption_type, severity, occurred_at_ns, detail, resolved
             FROM interruption_events WHERE interruption_id=?1",
        )?;
        let mut rows = stmt.query_map(params![interruption_id], |row| {
            Ok(InterruptionRecord {
                id: row.get(0)?,
                interruption_id: row.get(1)?,
                session_id: row.get(2)?,
                trace_id: row.get(3)?,
                conversation_id: row.get(4)?,
                call_id: row.get(5)?,
                pid: row.get(6)?,
                agent_name: row.get(7)?,
                interruption_type: row.get(8)?,
                severity: row.get(9)?,
                occurred_at_ns: row.get(10)?,
                detail: row.get(11)?,
                resolved: row.get::<_, i32>(12)? != 0,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    /// Get all interruptions for a session.
    pub fn list_by_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<InterruptionRecord>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, interruption_id, session_id, trace_id, conversation_id, call_id, pid, agent_name,
                    interruption_type, severity, occurred_at_ns, detail, resolved
             FROM interruption_events
             WHERE session_id=?1
             ORDER BY occurred_at_ns ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(InterruptionRecord {
                id: row.get(0)?,
                interruption_id: row.get(1)?,
                session_id: row.get(2)?,
                trace_id: row.get(3)?,
                conversation_id: row.get(4)?,
                call_id: row.get(5)?,
                pid: row.get(6)?,
                agent_name: row.get(7)?,
                interruption_type: row.get(8)?,
                severity: row.get(9)?,
                occurred_at_ns: row.get(10)?,
                detail: row.get(11)?,
                resolved: row.get::<_, i32>(12)? != 0,
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    pub fn list_by_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<InterruptionRecord>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, interruption_id, session_id, trace_id, conversation_id, call_id, pid, agent_name,
                    interruption_type, severity, occurred_at_ns, detail, resolved
             FROM interruption_events
             WHERE conversation_id=?1
             ORDER BY occurred_at_ns ASC",
        )?;
        let rows = stmt.query_map(params![conversation_id], |row| {
            Ok(InterruptionRecord {
                id: row.get(0)?,
                interruption_id: row.get(1)?,
                session_id: row.get(2)?,
                trace_id: row.get(3)?,
                conversation_id: row.get(4)?,
                call_id: row.get(5)?,
                pid: row.get(6)?,
                agent_name: row.get(7)?,
                interruption_type: row.get(8)?,
                severity: row.get(9)?,
                occurred_at_ns: row.get(10)?,
                detail: row.get(11)?,
                resolved: row.get::<_, i32>(12)? != 0,
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Statistics: count by (type, severity) within a time range.
    ///
    /// `resolved` filters by resolution state: `Some(false)` yields the
    /// unresolved-only view that the dashboard overview shares with the
    /// per-session breakdown, while `None` counts every event (used by the
    /// Prometheus counter and the CLI, which must stay monotonic).
    ///
    /// Grouped by severity as well as type so a type whose severity mapping
    /// changed across versions cannot report one severity for a count that
    /// aggregates several — callers summing `by_severity` rely on this.
    pub fn stats(
        &self,
        start_ns: i64,
        end_ns: i64,
        resolved: Option<bool>,
    ) -> Result<Vec<InterruptionTypeStat>, Box<dyn std::error::Error>> {
        // Two fixed statements instead of an assembled one: the storage layer
        // forbids building SQL by concatenation, so no future filter can turn
        // this into unsafe query assembly.
        const STATS_ALL: &str = "SELECT interruption_type, severity, COUNT(*) AS cnt
             FROM interruption_events
             WHERE occurred_at_ns BETWEEN ?1 AND ?2
             GROUP BY interruption_type, severity
             ORDER BY cnt DESC";
        const STATS_BY_RESOLVED: &str = "SELECT interruption_type, severity, COUNT(*) AS cnt
             FROM interruption_events
             WHERE occurred_at_ns BETWEEN ?1 AND ?2 AND resolved = ?3
             GROUP BY interruption_type, severity
             ORDER BY cnt DESC";

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(InterruptionTypeStat {
                interruption_type: row.get(0)?,
                severity: row.get(1)?,
                count: row.get(2)?,
            })
        };
        let mut result = Vec::new();
        match resolved {
            Some(r) => {
                let mut stmt = conn.prepare(STATS_BY_RESOLVED)?;
                for row in stmt.query_map(params![start_ns, end_ns, r as i32], map_row)? {
                    result.push(row?);
                }
            }
            None => {
                let mut stmt = conn.prepare(STATS_ALL)?;
                for row in stmt.query_map(params![start_ns, end_ns], map_row)? {
                    result.push(row?);
                }
            }
        }
        Ok(result)
    }

    /// Count unresolved interruptions grouped by (session_id, severity, type).
    /// Returns detailed rows for building per-severity badges with type tooltips.
    ///
    /// Events with a NULL `session_id` are bucketed under
    /// [`UNASSIGNED_SESSION_ID`] rather than dropped: the overview card counts
    /// them, so filtering them out here made the breakdown silently disagree
    /// with the total and looked like data loss.
    ///
    /// `agent_name` scopes the result to one agent so the breakdown matches the
    /// dashboard's agent filter; `None` covers all agents. Bound as a parameter
    /// compared with `?4 IS NULL` instead of appending a clause, since the
    /// storage layer forbids assembling SQL by concatenation.
    pub fn count_unresolved_by_session_detailed(
        &self,
        start_ns: i64,
        end_ns: i64,
        agent_name: Option<&str>,
    ) -> Result<Vec<(String, String, String, i64)>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT COALESCE(session_id, ?3) AS sid, severity, interruption_type, COUNT(*) AS cnt
             FROM interruption_events
             WHERE resolved = 0
               AND occurred_at_ns BETWEEN ?1 AND ?2
               AND (?4 IS NULL OR agent_name = ?4)
             GROUP BY sid, severity, interruption_type
             ORDER BY sid, cnt DESC",
        )?;
        let rows = stmt.query_map(
            params![start_ns, end_ns, UNASSIGNED_SESSION_ID, agent_name],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Count unresolved interruptions grouped by
    /// (session_id, conversation_id, severity, type).
    ///
    /// NULL ids are bucketed under [`UNASSIGNED_SESSION_ID`] /
    /// [`UNASSIGNED_CONVERSATION_ID`] for the same total-versus-breakdown
    /// consistency reason as [`Self::count_unresolved_by_session_detailed`], and
    /// `agent_name` scopes the result the same way.
    ///
    /// `session_id` is part of the grouping key because a conversation-only
    /// breakdown cannot say which session an event belongs to: the dashboard
    /// nests conversation rows under a session, so it would render every
    /// session-less event under whichever session happens to own that
    /// conversation, double-counting it against the unassigned-session row.
    pub fn count_unresolved_by_conversation_detailed(
        &self,
        start_ns: i64,
        end_ns: i64,
        agent_name: Option<&str>,
    ) -> Result<Vec<(String, String, String, String, i64)>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT COALESCE(session_id, ?3) AS sid, COALESCE(conversation_id, ?4) AS cid,
                    severity, interruption_type, COUNT(*) AS cnt
             FROM interruption_events
             WHERE resolved = 0
               AND occurred_at_ns BETWEEN ?1 AND ?2
               AND (?5 IS NULL OR agent_name = ?5)
             GROUP BY sid, cid, severity, interruption_type
             ORDER BY sid, cid, cnt DESC",
        )?;
        let rows = stmt.query_map(
            params![
                start_ns,
                end_ns,
                UNASSIGNED_SESSION_ID,
                UNASSIGNED_CONVERSATION_ID,
                agent_name
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Backfill a NULL `session_id` for every interruption of `call_id`.
    ///
    /// Companion to the GenAI store's retroactive session fix-up: a call that
    /// escaped the session-resolution deferral window has its interruptions
    /// already persisted with a NULL `session_id`, so repairing only
    /// `genai_events` would leave them unattributable forever.
    ///
    /// Only NULL rows are touched — an already-attributed interruption keeps
    /// whatever id it was detected with.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection mutex is poisoned (surfaced rather
    /// than recovered, so the caller can retry on a later fix-up attempt) or
    /// the UPDATE fails.
    pub fn backfill_null_session_id(
        &self,
        call_id: &str,
        session_id: &str,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| format!("interruption store connection mutex poisoned: {e}"))?;
        let updated = conn.execute(
            "UPDATE interruption_events
             SET session_id = ?2
             WHERE call_id = ?1 AND session_id IS NULL",
            params![call_id, session_id],
        )?;
        Ok(updated)
    }

    /// Check if a recent agent_crash event exists for the given PID.
    ///
    /// Used for dedup between trace-mode crash detection and serve-mode
    /// HealthChecker: if trace already recorded the crash, serve skips it.
    pub fn agent_crash_exists_recent(&self, pid: i32, window_secs: u64) -> bool {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let cutoff_ns = now_ns - (window_secs as i64 * 1_000_000_000);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT COUNT(*) FROM interruption_events
             WHERE interruption_type='agent_crash' AND pid=?1 AND occurred_at_ns > ?2",
            params![pid, cutoff_ns],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    }

    // ─── Process exit status (trace → serve channel) ───────────────────────

    /// TTL for `process_exits` rows, pruned on every write. Must comfortably
    /// exceed the HealthChecker offline-detection latency (30s cycle + 120s
    /// lookup window) while keeping the table bounded.
    const PROCESS_EXIT_TTL_SECS: i64 = 600;

    /// Record the raw `task_struct->exit_code` (wait(2) encoding) observed by
    /// trace mode when an agent process exits.
    ///
    /// trace and serve run as separate processes and only share the SQLite
    /// files, so this table is the channel that lets the serve-mode
    /// HealthChecker distinguish a trace-confirmed clean exit from a crash
    /// (issue #1989). Keyed by pid via INSERT OR REPLACE so pid reuse keeps
    /// only the latest exit; stale rows are pruned to bound growth.
    pub fn record_process_exit(
        &self,
        pid: i32,
        raw_exit_code: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT OR REPLACE INTO process_exits (pid, raw_exit_code, exited_at_ns)
             VALUES (?1, ?2, ?3)",
            params![pid, raw_exit_code as i64, now_ns],
        )?;
        conn.execute(
            "DELETE FROM process_exits WHERE exited_at_ns < ?1",
            params![now_ns - Self::PROCESS_EXIT_TTL_SECS * 1_000_000_000],
        )?;
        Ok(())
    }

    /// Return the raw exit code trace mode recorded for `pid` within the last
    /// `window_secs`, or `None` when trace did not observe the exit (e.g. the
    /// collector was not running) — callers must then keep their fallback
    /// behavior.
    pub fn get_process_exit_recent(&self, pid: i32, window_secs: u64) -> Option<u32> {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let cutoff_ns = now_ns - (window_secs as i64 * 1_000_000_000);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT raw_exit_code FROM process_exits WHERE pid=?1 AND exited_at_ns > ?2",
            params![pid, cutoff_ns],
            |row| row.get::<_, i64>(0),
        )
        .ok()
        .map(|v| v as u32)
    }

    /// Best-effort removal of the recorded exit status for `pid`.
    ///
    /// Called when `record_process_exit` fails: a stale row from a previous
    /// process could otherwise be read for a reused pid within the lookup
    /// window, misclassifying a real crash as a clean exit. Clearing restores
    /// the safe "no record → fallback crash handling" semantics.
    pub fn clear_process_exit(&self, pid: i32) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute("DELETE FROM process_exits WHERE pid=?1", params![pid])?;
        Ok(())
    }
}

// ─── Error matching helpers ────────────────────────────────────────────────────

/// Try to extract a clean message from a string that may contain nested JSON.
/// Returns the extracted message lowercased, or the original string lowercased.
fn normalize_error_key(raw: &str) -> String {
    let trimmed = raw.trim();
    // Try to parse as JSON and extract "message" field
    if let Some(msg) = extract_message_from_json(trimmed) {
        return msg.to_lowercase();
    }
    // Try to find JSON embedded in the string (e.g. "curl...{\"error\":{...}}")
    if let Some(brace_start) = trimmed.find('{') {
        if let Some(msg) = extract_message_from_json(&trimmed[brace_start..]) {
            return msg.to_lowercase();
        }
    }
    trimmed.to_lowercase()
}

/// Attempt to parse JSON and find a "message" field at common locations:
/// - top-level "message"
/// - "error.message"
fn extract_message_from_json(s: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    // Try "error.message"
    if let Some(msg) = v
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        return Some(msg.to_string());
    }
    // Try top-level "message"
    if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
        return Some(msg.to_string());
    }
    None
}

/// Check whether two error strings refer to the same underlying error.
/// Uses keyword normalization + substring containment.
fn errors_match(a: &str, b: &str) -> bool {
    let na = normalize_error_key(a);
    let nb = normalize_error_key(b);
    if na == nb {
        return true;
    }
    // Substring containment: if one fully contains the other
    na.contains(&nb) || nb.contains(&na)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_store() -> InterruptionStore {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "test_interruption_{}.db",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        InterruptionStore::new_with_path(&path).unwrap()
    }

    fn make_event(conversation_id: &str, itype: InterruptionType) -> InterruptionEvent {
        InterruptionEvent {
            interruption_id: format!(
                "int-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .subsec_nanos()
            ),
            session_id: Some("sess-1".to_string()),
            trace_id: None,
            conversation_id: Some(conversation_id.to_string()),
            call_id: None,
            pid: Some(1234),
            agent_name: Some("TestAgent".to_string()),
            interruption_type: itype,
            severity: crate::interruption::types::Severity::Critical,
            occurred_at_ns: 1000000000,
            detail: None,
            resolved: false,
        }
    }

    #[test]
    fn test_old_schema_db_is_migrated_before_index_creation() {
        // Reproduces issue #3314: a database created before conversation_id
        // existed must have the column added before any index on it is
        // created. With the old ordering the CREATE INDEX aborts the batch
        // and the store never initializes.
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "test_interruption_legacy_{}.db",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let legacy_schema = "CREATE TABLE interruption_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            interruption_id TEXT NOT NULL UNIQUE,
            session_id TEXT, trace_id TEXT, call_id TEXT, pid INTEGER,
            agent_name TEXT, interruption_type TEXT NOT NULL, severity TEXT NOT NULL,
            occurred_at_ns INTEGER NOT NULL, detail TEXT,
            resolved INTEGER NOT NULL DEFAULT 0,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP)";
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(legacy_schema).unwrap();
        }
        InterruptionStore::new_with_path(&path)
            .expect("store must initialize on a pre-conversation_id database");
        let conn = rusqlite::Connection::open(&path).unwrap();
        let column: Option<String> = conn
            .prepare("PRAGMA table_info(interruption_events)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .flatten()
            .find(|name| name == "conversation_id");
        assert_eq!(column.as_deref(), Some("conversation_id"));
        let index: Option<String> = conn
            .prepare("PRAGMA index_list(interruption_events)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .flatten()
            .find(|name| name == "idx_interruption_conversation");
        assert_eq!(index.as_deref(), Some("idx_interruption_conversation"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_reopening_migrated_db_is_idempotent() {
        // Opening the same database twice must not regress: the duplicate-column
        // ALTER error on the second open is ignored by design, and the store
        // keeps working. Guards the fresh-database path of the #3314 fix.
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "test_interruption_reopen_{}.db",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        InterruptionStore::new_with_path(&path).expect("first open");
        InterruptionStore::new_with_path(&path).expect("reopen with duplicate-column ALTER");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_count_for_conversation_empty() {
        let store = temp_store();
        let count = store.count_for_conversation("conv-999", &InterruptionType::DeadLoop);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_count_for_conversation_single() {
        let store = temp_store();
        let event = make_event("conv-1", InterruptionType::DeadLoop);
        store.insert(&event).unwrap();
        let count = store.count_for_conversation("conv-1", &InterruptionType::DeadLoop);
        assert_eq!(count, 1);
    }

    #[test]
    fn test_count_for_conversation_multiple() {
        let store = temp_store();
        for i in 0..3 {
            let mut event = make_event("conv-2", InterruptionType::DeadLoop);
            event.interruption_id = format!("int-multi-{i}");
            store.insert(&event).unwrap();
        }
        let count = store.count_for_conversation("conv-2", &InterruptionType::DeadLoop);
        assert_eq!(count, 3);
    }

    #[test]
    fn test_count_for_conversation_different_type_not_counted() {
        let store = temp_store();
        let event = make_event("conv-3", InterruptionType::RetryStorm);
        store.insert(&event).unwrap();
        let count = store.count_for_conversation("conv-3", &InterruptionType::DeadLoop);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_count_for_conversation_different_conv_not_counted() {
        let store = temp_store();
        let mut event = make_event("conv-4", InterruptionType::DeadLoop);
        event.interruption_id = "int-other-conv".to_string();
        store.insert(&event).unwrap();
        let count = store.count_for_conversation("conv-5", &InterruptionType::DeadLoop);
        assert_eq!(count, 0);
    }

    /// After intentionally poisoning the conn mutex, methods that use
    /// `unwrap_or_else(|e| e.into_inner())` should still operate correctly.
    #[test]
    fn poison_recovery_conn_still_operational() {
        let store = temp_store();

        // Intentionally poison the conn mutex
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = store.conn.lock().unwrap();
            panic!("intentional poison");
        }));
        assert!(result.is_err(), "Mutex should be poisoned");

        // Exercise the poison-recovery path via count_for_conversation
        // which locks conn via unwrap_or_else(|e| e.into_inner())
        let count = store.count_for_conversation("conv-none", &InterruptionType::DeadLoop);
        assert_eq!(count, 0, "Should still query after poison recovery");

        // Also exercise a write path (insert)
        let event = make_event("conv-poison", InterruptionType::DeadLoop);
        store.insert(&event).unwrap();
    }

    // ── insert / get_by_id ──────────────────────────────────────────────────

    #[test]
    fn insert_and_get_by_id() {
        let store = temp_store();
        let mut event = make_event("conv-ins", InterruptionType::LlmError);
        event.interruption_id = "int-get-1".to_string();
        event.detail = Some(r#"{"error":"bad request"}"#.to_string());
        store.insert(&event).unwrap();

        let row = store
            .get_by_id("int-get-1")
            .unwrap()
            .expect("row should exist");
        assert_eq!(row.interruption_id, "int-get-1");
        assert_eq!(row.interruption_type, "llm_error");
        assert_eq!(row.severity, "critical");
        assert!(!row.resolved);
        assert_eq!(row.detail.as_deref(), Some(r#"{"error":"bad request"}"#));
    }

    #[test]
    fn insert_duplicate_ignored() {
        let store = temp_store();
        let event = make_event("conv-dup", InterruptionType::RateLimit);
        // First insert
        store.insert(&event).unwrap();
        // Second insert with same interruption_id is silently ignored
        store.insert(&event).unwrap();

        let records = store
            .list(0, i64::MAX, None, None, None, None, 100)
            .unwrap();
        assert_eq!(records.len(), 1, "duplicate should be ignored");
    }

    #[test]
    fn insert_batch_inserts_multiple() {
        let store = temp_store();
        let events: Vec<_> = (0..3)
            .map(|i| {
                let mut e = make_event("conv-batch", InterruptionType::SseTruncated);
                e.interruption_id = format!("int-batch-{i}");
                e
            })
            .collect();
        store.insert_batch(&events).unwrap();

        let records = store
            .list(0, i64::MAX, None, None, None, None, 100)
            .unwrap();
        assert_eq!(records.len(), 3);
    }

    // ── resolve ──────────────────────────────────────────────────────────────

    #[test]
    fn resolve_existing_event() {
        let store = temp_store();
        let mut event = make_event("conv-resolve", InterruptionType::AuthError);
        event.interruption_id = "int-resolve-1".to_string();
        store.insert(&event).unwrap();

        assert!(store.resolve("int-resolve-1").unwrap());

        let row = store.get_by_id("int-resolve-1").unwrap().unwrap();
        assert!(row.resolved);
    }

    #[test]
    fn resolve_nonexistent_returns_false() {
        let store = temp_store();
        assert!(!store.resolve("no-such-id").unwrap());
    }

    // ── list with filters ────────────────────────────────────────────────────

    #[test]
    fn list_filters_by_agent_name_type_severity_resolved() {
        let store = temp_store();
        let mut e1 = make_event("conv-f1", InterruptionType::RateLimit);
        e1.interruption_id = "int-f1".to_string();
        e1.agent_name = Some("Agent-A".to_string());
        e1.severity = crate::interruption::types::Severity::Medium;
        store.insert(&e1).unwrap();

        let mut e2 = make_event("conv-f2", InterruptionType::AuthError);
        e2.interruption_id = "int-f2".to_string();
        e2.agent_name = Some("Agent-B".to_string());
        e2.severity = crate::interruption::types::Severity::High;
        store.insert(&e2).unwrap();

        // Filter by agent_name
        let rows = store
            .list(0, i64::MAX, Some("Agent-A"), None, None, None, 100)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].interruption_id, "int-f1");

        // Filter by type
        let rows = store
            .list(0, i64::MAX, None, Some("auth_error"), None, None, 100)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].interruption_id, "int-f2");

        // Filter by severity
        let rows = store
            .list(0, i64::MAX, None, None, Some("medium"), None, 100)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].interruption_id, "int-f1");

        // Filter by resolved=false (both unresolved)
        let rows = store
            .list(0, i64::MAX, None, None, None, Some(false), 100)
            .unwrap();
        assert_eq!(rows.len(), 2);

        // Resolve one, then filter resolved=true
        store.resolve("int-f1").unwrap();
        let rows = store
            .list(0, i64::MAX, None, None, None, Some(true), 100)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].interruption_id, "int-f1");
    }

    #[test]
    fn list_respects_time_range_and_limit() {
        let store = temp_store();
        for i in 0..5 {
            let mut e = make_event("conv-tr", InterruptionType::LlmError);
            e.interruption_id = format!("int-tr-{i}");
            e.occurred_at_ns = 1_000_000_000 + i * 1_000_000_000;
            store.insert(&e).unwrap();
        }

        // Time range: only events at 2B and 3B ns
        let rows = store
            .list(2_000_000_000, 3_000_000_000, None, None, None, None, 100)
            .unwrap();
        assert_eq!(rows.len(), 2);

        // Limit 2 (most recent first)
        let rows = store.list(0, i64::MAX, None, None, None, None, 2).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows[0].occurred_at_ns >= rows[1].occurred_at_ns,
            "should be DESC order"
        );
    }

    // ── list_by_session / list_by_conversation ───────────────────────────────

    #[test]
    fn list_by_session_returns_matching_rows() {
        let store = temp_store();
        let mut e1 = make_event("conv-ls", InterruptionType::DeadLoop);
        e1.interruption_id = "int-ls-1".to_string();
        e1.session_id = Some("sess-A".to_string());
        store.insert(&e1).unwrap();

        let mut e2 = make_event("conv-ls2", InterruptionType::RetryStorm);
        e2.interruption_id = "int-ls-2".to_string();
        e2.session_id = Some("sess-B".to_string());
        store.insert(&e2).unwrap();

        let rows = store.list_by_session("sess-A").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].interruption_id, "int-ls-1");
    }

    #[test]
    fn list_by_conversation_returns_matching_rows() {
        let store = temp_store();
        let mut e1 = make_event("conv-lc", InterruptionType::TokenLimit);
        e1.interruption_id = "int-lc-1".to_string();
        store.insert(&e1).unwrap();

        let rows = store.list_by_conversation("conv-lc").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].conversation_id.as_deref(), Some("conv-lc"));
    }

    // ── stats ────────────────────────────────────────────────────────────────

    #[test]
    fn stats_groups_by_type() {
        let store = temp_store();
        for i in 0..2 {
            let mut e = make_event("conv-st", InterruptionType::RateLimit);
            e.interruption_id = format!("int-st-rl-{i}");
            e.occurred_at_ns = 5_000_000_000;
            store.insert(&e).unwrap();
        }
        let mut e = make_event("conv-st2", InterruptionType::AgentCrash);
        e.interruption_id = "int-st-crash".to_string();
        e.occurred_at_ns = 5_000_000_000;
        store.insert(&e).unwrap();

        let stats = store.stats(0, i64::MAX, None).unwrap();
        // rate_limit should have count=2, agent_crash count=1
        let rl = stats
            .iter()
            .find(|s| s.interruption_type == "rate_limit")
            .unwrap();
        assert_eq!(rl.count, 2);
        let crash = stats
            .iter()
            .find(|s| s.interruption_type == "agent_crash")
            .unwrap();
        assert_eq!(crash.count, 1);
    }

    // ── count_unresolved_by_session/conversation_detailed ─────────────────────

    #[test]
    fn count_unresolved_by_session_detailed_groups_correctly() {
        let store = temp_store();
        let mut e1 = make_event("conv-csd1", InterruptionType::DeadLoop);
        e1.interruption_id = "int-csd-1".to_string();
        e1.session_id = Some("sess-X".to_string());
        store.insert(&e1).unwrap();

        // Resolved event should be excluded
        let mut e2 = make_event("conv-csd2", InterruptionType::DeadLoop);
        e2.interruption_id = "int-csd-2".to_string();
        e2.session_id = Some("sess-X".to_string());
        store.insert(&e2).unwrap();
        store.resolve("int-csd-2").unwrap();

        let rows = store
            .count_unresolved_by_session_detailed(0, i64::MAX, None)
            .unwrap();
        assert_eq!(rows.len(), 1, "only unresolved rows should appear");
        assert_eq!(rows[0].0, "sess-X");
        assert_eq!(rows[0].3, 1, "count should be 1 (resolved excluded)");
    }

    #[test]
    fn count_unresolved_by_conversation_detailed_groups_correctly() {
        let store = temp_store();
        let mut e = make_event("conv-ccd", InterruptionType::LlmError);
        e.interruption_id = "int-ccd-1".to_string();
        store.insert(&e).unwrap();

        let rows = store
            .count_unresolved_by_conversation_detailed(0, i64::MAX, None)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "sess-1");
        assert_eq!(rows[0].1, "conv-ccd");
    }

    // ── total-versus-breakdown consistency (issue: 7 high interruptions with a
    //    NULL session_id counted in the overview but missing from the per-session
    //    breakdown) ─────────────────────────────────────────────────────────────

    #[test]
    fn session_breakdown_buckets_null_session_id_instead_of_dropping_it() {
        let store = temp_store();

        let mut attributed = make_event("conv-a", InterruptionType::ToolFailure);
        attributed.interruption_id = "int-attributed".to_string();
        attributed.session_id = Some("sess-real".to_string());
        store.insert(&attributed).unwrap();

        // The empty_response / sse_truncated shape: detected before the session
        // was resolved, so persisted with a NULL session_id.
        for i in 0..3 {
            let mut orphan = make_event("conv-b", InterruptionType::EmptyResponse);
            orphan.interruption_id = format!("int-orphan-{i}");
            orphan.session_id = None;
            store.insert(&orphan).unwrap();
        }

        let rows = store
            .count_unresolved_by_session_detailed(0, i64::MAX, None)
            .unwrap();
        let breakdown_total: i64 = rows.iter().map(|(_, _, _, cnt)| cnt).sum();
        let overview_total: i64 = store
            .stats(0, i64::MAX, Some(false))
            .unwrap()
            .iter()
            .map(|s| s.count)
            .sum();
        assert_eq!(
            breakdown_total, overview_total,
            "per-session breakdown must account for every counted interruption"
        );

        let unassigned = rows
            .iter()
            .find(|(sid, _, _, _)| sid == UNASSIGNED_SESSION_ID)
            .expect("NULL session_id must land in the unassigned bucket");
        assert_eq!(unassigned.3, 3);
        assert!(rows.iter().any(|(sid, _, _, _)| sid == "sess-real"));
    }

    #[test]
    fn conversation_breakdown_buckets_null_conversation_id() {
        let store = temp_store();
        let mut e = make_event("conv-kept", InterruptionType::SseTruncated);
        e.interruption_id = "int-conv-kept".to_string();
        e.conversation_id = None;
        store.insert(&e).unwrap();

        let rows = store
            .count_unresolved_by_conversation_detailed(0, i64::MAX, None)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "sess-1");
        assert_eq!(rows[0].1, UNASSIGNED_CONVERSATION_ID);
    }

    #[test]
    fn conversation_breakdown_keeps_session_less_events_out_of_a_real_session() {
        let store = temp_store();

        // One conversation, two events: one attributed to the session, one
        // detected before the session was resolved. Grouping by conversation
        // alone would file both under the session that owns the conversation,
        // while the unassigned-session row counts the second one as well.
        let mut attributed = make_event("conv-shared", InterruptionType::ToolFailure);
        attributed.interruption_id = "int-shared-attributed".to_string();
        attributed.session_id = Some("sess-owner".to_string());
        store.insert(&attributed).unwrap();

        let mut session_less = make_event("conv-shared", InterruptionType::EmptyResponse);
        session_less.interruption_id = "int-shared-orphan".to_string();
        session_less.session_id = None;
        store.insert(&session_less).unwrap();

        let rows = store
            .count_unresolved_by_conversation_detailed(0, i64::MAX, None)
            .unwrap();

        let owned: i64 = rows
            .iter()
            .filter(|(sid, cid, _, _, _)| sid == "sess-owner" && cid == "conv-shared")
            .map(|(_, _, _, _, cnt)| cnt)
            .sum();
        assert_eq!(
            owned, 1,
            "the session's own row must not absorb the session-less event"
        );

        let orphaned: i64 = rows
            .iter()
            .filter(|(sid, _, _, _, _)| sid == UNASSIGNED_SESSION_ID)
            .map(|(_, _, _, _, cnt)| cnt)
            .sum();
        assert_eq!(
            orphaned, 1,
            "the session-less event stays in the unassigned-session bucket"
        );
    }

    #[test]
    fn stats_resolved_filter_selects_the_intended_view() {
        let store = temp_store();
        let mut open = make_event("conv-open", InterruptionType::EmptyResponse);
        open.interruption_id = "int-open".to_string();
        store.insert(&open).unwrap();
        let mut closed = make_event("conv-closed", InterruptionType::EmptyResponse);
        closed.interruption_id = "int-closed".to_string();
        store.insert(&closed).unwrap();
        store.resolve("int-closed").unwrap();

        let sum = |rows: Vec<InterruptionTypeStat>| -> i64 { rows.iter().map(|s| s.count).sum() };
        assert_eq!(
            sum(store.stats(0, i64::MAX, Some(false)).unwrap()),
            1,
            "dashboard view counts unresolved only"
        );
        assert_eq!(
            sum(store.stats(0, i64::MAX, None).unwrap()),
            2,
            "Prometheus counter view keeps resolved events"
        );
    }

    #[test]
    fn breakdowns_scope_to_the_requested_agent() {
        let store = temp_store();
        // Both agents have an unattributed interruption; a per-agent view must
        // not fold the other agent's rows into the unassigned bucket.
        for (i, agent) in ["AgentA", "AgentB"].iter().enumerate() {
            let mut e = make_event("conv-agent", InterruptionType::EmptyResponse);
            e.interruption_id = format!("int-agent-{i}");
            e.session_id = None;
            e.agent_name = Some((*agent).to_string());
            store.insert(&e).unwrap();
        }

        let only_a = store
            .count_unresolved_by_session_detailed(0, i64::MAX, Some("AgentA"))
            .unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].0, UNASSIGNED_SESSION_ID);
        assert_eq!(only_a[0].3, 1, "AgentB's row must not be counted");

        let both = store
            .count_unresolved_by_session_detailed(0, i64::MAX, None)
            .unwrap();
        assert_eq!(both[0].3, 2, "None must cover every agent");

        let conv_only_b = store
            .count_unresolved_by_conversation_detailed(0, i64::MAX, Some("AgentB"))
            .unwrap();
        assert_eq!(conv_only_b.len(), 1);
        assert_eq!(conv_only_b[0].0, UNASSIGNED_SESSION_ID);
        assert_eq!(conv_only_b[0].1, "conv-agent");
        assert_eq!(conv_only_b[0].4, 1);
    }

    #[test]
    fn stats_groups_by_severity_so_by_severity_sums_stay_exact() {
        let store = temp_store();
        // Same type persisted with two severities (a severity mapping that
        // changed across versions leaves exactly this shape in the DB).
        let mut high = make_event("conv-sev", InterruptionType::EmptyResponse);
        high.interruption_id = "int-sev-high".to_string();
        high.severity = crate::interruption::types::Severity::High;
        store.insert(&high).unwrap();
        let mut medium = make_event("conv-sev", InterruptionType::EmptyResponse);
        medium.interruption_id = "int-sev-medium".to_string();
        medium.severity = crate::interruption::types::Severity::Medium;
        store.insert(&medium).unwrap();

        let rows = store.stats(0, i64::MAX, Some(false)).unwrap();
        let empty_rows: Vec<_> = rows
            .iter()
            .filter(|s| s.interruption_type == "empty_response")
            .collect();
        assert_eq!(
            empty_rows.len(),
            2,
            "one row per (type, severity), not a merged count with one severity"
        );
        assert!(empty_rows.iter().all(|s| s.count == 1));
        assert_eq!(empty_rows.iter().map(|s| s.count).sum::<i64>(), 2);
    }

    // ── backfill_null_session_id (retroactive fix-up companion) ───────────────

    #[test]
    fn backfill_null_session_id_only_touches_null_rows_of_the_call() {
        let store = temp_store();

        let mut orphan = make_event("conv-bf", InterruptionType::SseTruncated);
        orphan.interruption_id = "int-bf-orphan".to_string();
        orphan.session_id = None;
        orphan.call_id = Some("call-1".to_string());
        store.insert(&orphan).unwrap();

        let mut already = make_event("conv-bf", InterruptionType::EmptyResponse);
        already.interruption_id = "int-bf-attributed".to_string();
        already.session_id = Some("sess-keep".to_string());
        already.call_id = Some("call-1".to_string());
        store.insert(&already).unwrap();

        let mut other_call = make_event("conv-bf", InterruptionType::SseTruncated);
        other_call.interruption_id = "int-bf-other".to_string();
        other_call.session_id = None;
        other_call.call_id = Some("call-2".to_string());
        store.insert(&other_call).unwrap();

        let updated = store
            .backfill_null_session_id("call-1", "sess-resolved")
            .unwrap();
        assert_eq!(updated, 1, "only the NULL row of call-1 may be updated");

        let session_of = |id: &str| store.get_by_id(id).unwrap().unwrap().session_id;
        assert_eq!(
            session_of("int-bf-orphan").as_deref(),
            Some("sess-resolved")
        );
        assert_eq!(
            session_of("int-bf-attributed").as_deref(),
            Some("sess-keep"),
            "an already-attributed interruption must not be overwritten"
        );
        assert_eq!(
            session_of("int-bf-other"),
            None,
            "another call's interruption must be left alone"
        );
    }

    #[test]
    fn backfill_null_session_id_unknown_call_updates_nothing() {
        let store = temp_store();
        let mut e = make_event("conv-bf-none", InterruptionType::EmptyResponse);
        e.interruption_id = "int-bf-none".to_string();
        e.session_id = None;
        e.call_id = Some("call-x".to_string());
        store.insert(&e).unwrap();

        let updated = store
            .backfill_null_session_id("no-such-call", "sess-resolved")
            .unwrap();
        assert_eq!(updated, 0);
    }

    // ── OOM dedup ────────────────────────────────────────────────────────────

    #[test]
    fn oom_event_exists_and_latest_oom_event_ns() {
        let store = temp_store();
        let mut event = make_event("conv-oom", InterruptionType::AgentCrash);
        event.interruption_id = "int-oom-1".to_string();
        event.pid = Some(9999);
        event.occurred_at_ns = 7_000_000_000;
        event.detail = Some(r#"{"oom":true,"reason":"out of memory"}"#.to_string());
        store.insert(&event).unwrap();

        assert!(store.oom_event_exists(9999, 7_000_000_000));
        assert!(!store.oom_event_exists(9999, 8_000_000_000));
        assert!(!store.oom_event_exists(1111, 7_000_000_000));
        assert_eq!(store.latest_oom_event_ns(), 7_000_000_000);
    }

    #[test]
    fn latest_oom_event_ns_returns_zero_when_no_oom() {
        let store = temp_store();
        assert_eq!(store.latest_oom_event_ns(), 0);
    }

    // ── exists_for_call ──────────────────────────────────────────────────────

    #[test]
    fn exists_for_call_dedup() {
        let store = temp_store();
        let mut event = make_event("conv-efc", InterruptionType::SseTruncated);
        event.interruption_id = "int-efc-1".to_string();
        event.call_id = Some("call-abc".to_string());
        store.insert(&event).unwrap();

        assert!(store.exists_for_call("call-abc", &InterruptionType::SseTruncated));
        assert!(!store.exists_for_call("call-abc", &InterruptionType::RateLimit));
        assert!(!store.exists_for_call("call-xyz", &InterruptionType::SseTruncated));
    }

    // ── exists_for_conversation ──────────────────────────────────────────────

    #[test]
    fn exists_for_conversation_no_filter_matches_any() {
        let store = temp_store();
        let mut e = make_event("conv-efn", InterruptionType::RetryStorm);
        e.interruption_id = "int-efn-1".to_string();
        store.insert(&e).unwrap();

        // No error_msg filter: any unresolved row with same conv+type matches
        assert!(store.exists_for_conversation("conv-efn", &InterruptionType::RetryStorm, None));
        assert!(!store.exists_for_conversation("conv-efn", &InterruptionType::DeadLoop, None));
    }

    #[test]
    fn exists_for_conversation_resolved_not_matched() {
        let store = temp_store();
        let mut e = make_event("conv-efr", InterruptionType::LlmError);
        e.interruption_id = "int-efr-1".to_string();
        store.insert(&e).unwrap();
        store.resolve("int-efr-1").unwrap();

        // Resolved rows should not match (resolved=0 filter in SQL)
        assert!(!store.exists_for_conversation("conv-efr", &InterruptionType::LlmError, None));
    }

    #[test]
    fn exists_for_conversation_json_error_matching() {
        let store = temp_store();
        let mut e = make_event("conv-efj", InterruptionType::LlmError);
        e.interruption_id = "int-efj-1".to_string();
        e.detail = Some(r#"{"error":"rate limit exceeded"}"#.to_string());
        store.insert(&e).unwrap();

        // Same error substring should match
        assert!(store.exists_for_conversation(
            "conv-efj",
            &InterruptionType::LlmError,
            Some("rate limit exceeded")
        ));
        // Unrelated error should not match
        assert!(!store.exists_for_conversation(
            "conv-efj",
            &InterruptionType::LlmError,
            Some("completely different error")
        ));
    }

    // ── agent_crash_exists_recent ────────────────────────────────────────────

    #[test]
    fn agent_crash_exists_recent_finds_recent_crash() {
        let store = temp_store();
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        let mut e = make_event("conv-acer", InterruptionType::AgentCrash);
        e.interruption_id = "int-acer-1".to_string();
        e.pid = Some(5555);
        e.occurred_at_ns = now_ns;
        store.insert(&e).unwrap();

        assert!(store.agent_crash_exists_recent(5555, 60));
        assert!(!store.agent_crash_exists_recent(6666, 60));
    }

    // ── process_exits (trace → serve exit-status channel) ───────────────────

    #[test]
    fn record_process_exit_roundtrip_replace_and_window() {
        let store = temp_store();
        assert!(store.get_process_exit_recent(7777, 60).is_none());

        store.record_process_exit(7777, 0x0).unwrap();
        assert_eq!(store.get_process_exit_recent(7777, 60), Some(0x0));

        // pid reuse: the latest exit wins
        store.record_process_exit(7777, 0x9).unwrap();
        assert_eq!(store.get_process_exit_recent(7777, 60), Some(0x9));

        // other pids and an expired window stay invisible
        assert!(store.get_process_exit_recent(8888, 60).is_none());
        assert!(store.get_process_exit_recent(7777, 0).is_none());
    }

    #[test]
    fn clear_process_exit_removes_stale_record_for_reused_pid() {
        let store = temp_store();

        // A clean-exit row is left over from a previous process with this pid
        // (write of the new exit status failed, so it was never replaced).
        store.record_process_exit(9999, 0x0).unwrap();
        assert_eq!(store.get_process_exit_recent(9999, 60), Some(0x0));

        // Best-effort cleanup must drop the stale row so the serve-mode
        // reader falls back to "no record → crash handling" for a reused pid.
        store.clear_process_exit(9999).unwrap();
        assert!(store.get_process_exit_recent(9999, 60).is_none());

        // Clearing an absent pid is a no-op, not an error.
        store.clear_process_exit(9999).unwrap();
    }

    #[test]
    fn periodic_maintenance_removes_stale_process_exits_without_new_writes() {
        let store = temp_store();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO process_exits (pid, raw_exit_code, exited_at_ns) VALUES (?1, ?2, ?3)",
                params![1234, 0, 1],
            )
            .unwrap();

        let deleted = store.purge_old_and_oversized(0, 0).unwrap();

        assert_eq!(deleted, 1);
        let remaining: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM process_exits", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    // ── purge ────────────────────────────────────────────────────────────────

    #[test]
    fn purge_before_deletes_old_rows() {
        let store = temp_store();
        let mut e1 = make_event("conv-pb1", InterruptionType::LlmError);
        e1.interruption_id = "int-pb-1".to_string();
        e1.occurred_at_ns = 1_000;
        store.insert(&e1).unwrap();

        let mut e2 = make_event("conv-pb2", InterruptionType::LlmError);
        e2.interruption_id = "int-pb-2".to_string();
        e2.occurred_at_ns = 10_000_000_000;
        store.insert(&e2).unwrap();

        let deleted = store.purge_before(5_000_000_000).unwrap();
        assert_eq!(deleted, 1);

        let remaining = store
            .list(0, i64::MAX, None, None, None, None, 100)
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].interruption_id, "int-pb-2");
    }

    #[test]
    fn purge_old_and_oversized_age_based() {
        let store = temp_store();
        let mut e = make_event("conv-poo", InterruptionType::LlmError);
        e.interruption_id = "int-poo-1".to_string();
        e.occurred_at_ns = 1; // very old
        store.insert(&e).unwrap();

        let deleted = store.purge_old_and_oversized(1, 0).unwrap();
        assert_eq!(
            deleted, 1,
            "very old row should be purged with 1-day retention"
        );
    }

    /// Regression for #2818: an oversized database must be trimmed to the
    /// limit (newest rows survive) and the logical size must converge, without
    /// wiping the table. Reverting to a main-file-only size check without an
    /// in-loop checkpoint makes this fail: the loop then cannot observe
    /// progress and deletes until the table is empty.
    ///
    /// The physical file may stay at its historical peak (#2888): freed pages
    /// go to the freelist and are reused by future inserts.
    #[test]
    fn purge_size_based_keeps_newest_and_converges() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "test_interruption_size_{}_{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = InterruptionStore::new_with_path(&path).unwrap();

        // Seed ~1.3MB across 300 rows (~4KB detail each), over a 1MB limit.
        let blob = "y".repeat(4 * 1024);
        for i in 0..300 {
            let mut e = make_event("conv-size", InterruptionType::LlmError);
            e.interruption_id = format!("int-size-{i:04}");
            e.occurred_at_ns = 1_000 + i as i64; // oldest first
            e.detail = Some(blob.clone());
            store.insert(&e).unwrap();
        }
        // Flush the WAL so the pre-purge size reflects the seeded rows.
        store.checkpoint().unwrap();
        assert!(
            store.total_db_file_size() > 1024 * 1024,
            "setup must exceed the 1MB limit, got {}",
            store.total_db_file_size()
        );

        let deleted = store.purge_old_and_oversized(0, 1).unwrap();
        assert!(deleted > 0, "oversized db must be trimmed");
        assert!(
            deleted < 300,
            "must stop at the limit, not wipe the table (#2818)"
        );

        // Newest rows survive.
        let remaining = store
            .list(0, i64::MAX, None, None, None, None, 500)
            .unwrap();
        assert!(
            remaining
                .iter()
                .any(|r| r.interruption_id == "int-size-0299"),
            "newest row must survive size-based trim"
        );

        // Logical size (physical minus freelist) converges below the limit;
        // the physical file may stay at its peak since pages are not returned
        // to the OS without VACUUM (#2888).
        let size_after = store.effective_db_file_size().unwrap();
        assert!(
            size_after <= 1024 * 1024,
            "logical size must fit after trim, got {size_after}"
        );

        // Once the file fits, the next purge is a no-op.
        let deleted_again = store.purge_old_and_oversized(0, 1).unwrap();
        assert_eq!(deleted_again, 0);

        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    /// A database between 90% and 100% of the limit is not oversized and must
    /// not be trimmed: the configured limit gates triggering, the 90% mark is
    /// only the stop target once trimming started. With the 90% mark as the
    /// trigger this test fails (rows would be deleted).
    #[test]
    fn purge_size_based_below_limit_is_noop() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "test_interruption_below_limit_{}_{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = InterruptionStore::new_with_path(&path).unwrap();

        // Grow until the total size lands in (0.9MB, 1MB] with a 1MB limit.
        // An un-checkpointed WAL holds many stale page versions and
        // inflates the measurement several-fold, so always measure right
        // after a checkpoint; one 20-row batch (~90KB compact) cannot jump
        // over the 0.1MB window.
        let blob = "y".repeat(4 * 1024);
        let mut seeded = 0usize;
        loop {
            for _ in 0..20 {
                let mut e = make_event("conv-below", InterruptionType::LlmError);
                e.interruption_id = format!("int-below-{seeded:04}");
                e.occurred_at_ns = 1_000 + seeded as i64;
                e.detail = Some(blob.clone());
                store.insert(&e).unwrap();
                seeded += 1;
            }
            store.checkpoint().unwrap();
            if store.total_db_file_size() > 943_718 {
                break; // > 0.9MB
            }
            assert!(seeded < 400, "setup should reach 0.9MB within 400 rows");
        }
        let size = store.total_db_file_size();
        assert!(
            size > 943_718 && size <= 1_048_576,
            "setup must land in (90%, 100%] of the limit, got {size}"
        );

        let deleted = store.purge_old_and_oversized(0, 1).unwrap();
        assert_eq!(deleted, 0, "below the configured limit must not trim");

        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    /// When another connection holds a read snapshot, the truncating WAL
    /// checkpoint returns busy and the trim loop must stop instead of deleting
    /// against a size that can never converge (only the WAL keeps growing).
    #[test]
    fn purge_size_based_stops_when_wal_checkpoint_busy() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "test_interruption_busy_{}_{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = InterruptionStore::new_with_path(&path).unwrap();

        // ~1.2MB across 300 rows (~4KB each), over a 1MB limit.
        let blob = "z".repeat(4 * 1024);
        for i in 0..300u64 {
            let mut e = make_event("conv-busy", InterruptionType::LlmError);
            e.interruption_id = format!("int-busy-{i:04}");
            e.occurred_at_ns = 1_000 + i as i64;
            e.detail = Some(blob.clone());
            store.insert(&e).unwrap();
        }
        store.checkpoint().unwrap();

        // Hold a read snapshot on a separate connection: this blocks
        // `PRAGMA wal_checkpoint(TRUNCATE)` from completing (busy).
        let reader = rusqlite::Connection::open(&path).unwrap();
        reader
            .execute_batch("BEGIN; SELECT COUNT(*) FROM interruption_events;")
            .unwrap();

        let deleted = store.purge_old_and_oversized(0, 1).unwrap();
        let remaining = store.row_count().unwrap();
        assert!(
            remaining > 0,
            "trim must stop once the WAL cannot be truncated, not delete \
             everything (deleted {deleted}, remaining {remaining})"
        );

        drop(reader);
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    // ── pure helper functions ────────────────────────────────────────────────

    #[test]
    fn normalize_error_key_plain_string() {
        assert_eq!(
            normalize_error_key("Rate Limit Exceeded"),
            "rate limit exceeded"
        );
    }

    #[test]
    fn normalize_error_key_json_message() {
        let json = r#"{"error":{"message":"model overloaded"}}"#;
        assert_eq!(normalize_error_key(json), "model overloaded");
    }

    #[test]
    fn normalize_error_key_top_level_message() {
        let json = r#"{"message":"bad request"}"#;
        assert_eq!(normalize_error_key(json), "bad request");
    }

    #[test]
    fn normalize_error_key_embedded_json() {
        let raw = r#"curl error: {"error":{"message":"connection refused"}}"#;
        assert_eq!(normalize_error_key(raw), "connection refused");
    }

    #[test]
    fn extract_message_from_json_nested_error() {
        let json = r#"{"error":{"message":"nested msg"}}"#;
        assert_eq!(
            extract_message_from_json(json),
            Some("nested msg".to_string())
        );
    }

    #[test]
    fn extract_message_from_json_top_level() {
        let json = r#"{"message":"top msg"}"#;
        assert_eq!(extract_message_from_json(json), Some("top msg".to_string()));
    }

    #[test]
    fn extract_message_from_json_no_message() {
        let json = r#"{"error":"just a string"}"#;
        assert_eq!(extract_message_from_json(json), None);
    }

    #[test]
    fn extract_message_from_json_invalid() {
        assert_eq!(extract_message_from_json("not json"), None);
    }

    #[test]
    fn errors_match_identical() {
        assert!(errors_match("rate limit", "rate limit"));
    }

    #[test]
    fn errors_match_case_insensitive() {
        assert!(errors_match("Rate Limit", "rate limit"));
    }

    #[test]
    fn errors_match_substring_containment() {
        assert!(errors_match("rate limit exceeded", "rate limit"));
        assert!(errors_match("rate limit", "rate limit exceeded"));
    }

    #[test]
    fn errors_match_unrelated() {
        assert!(!errors_match("rate limit", "auth error"));
    }
}
