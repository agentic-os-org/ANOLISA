//! SQLite persistence for collected trajectories (`trajectories.db`).
//!
//! One row per session file. Queryable columns are derived from the converted
//! ATIF v1.7 document (never from the raw JSONL) so they always agree with
//! `atif_json`; bookkeeping columns (`file_*`) drive incremental scanning.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use agentsight_sqlite_lifecycle::{
    checkpoint_truncate, enforce_size_policy, measure_database, open_connection,
    retention_cutoff_ns, CheckpointOutcome, ConnectionMode, ConnectionOptions, MaintenanceReport,
    MaintenanceStatus, SizeBasis, SizePolicy,
};
use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use agentsight_atif::{AtifTrajectory, Step, StepCategory};

/// A row of `collected_trajectories` ready for upsert.
#[derive(Debug, Clone)]
pub struct TrajectoryRecord {
    // ATIF-derived columns
    pub session_id: String,
    pub schema_version: String,
    pub agent_name: String,
    pub model_name: Option<String>,
    pub num_steps: i64,
    pub total_prompt_tokens: Option<i64>,
    pub total_completion_tokens: Option<i64>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    /// First user-authored message preview (≤ 200 chars), from ATIF steps.
    pub first_user_message: Option<String>,
    /// Last user-authored message preview (≤ 200 chars), from ATIF steps.
    pub last_user_message: Option<String>,
    pub atif_json: String,
    // Collection bookkeeping columns
    pub project: String,
    pub source: String,
    pub is_subagent: bool,
    pub file_path: String,
    pub file_size: i64,
    pub file_mtime_ns: i64,
}

/// Lightweight list row of `collected_trajectories` — deliberately excludes
/// the (potentially large) `atif_json` column; fetch it via
/// [`TrajectoryStore::get_atif_json`] for the detail view.
#[derive(Debug, Clone, Serialize)]
pub struct TrajectorySummary {
    pub session_id: String,
    pub schema_version: String,
    pub agent_name: String,
    pub model_name: Option<String>,
    pub num_steps: i64,
    pub total_prompt_tokens: Option<i64>,
    pub total_completion_tokens: Option<i64>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub first_user_message: Option<String>,
    pub last_user_message: Option<String>,
    pub project: String,
    /// Which product wrote the file: "qoder" or "qoderwork".
    pub source: String,
    pub is_subagent: bool,
    pub collected_at_ns: i64,
}

/// Distinct filter values for the trajectory list UI dropdowns.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TrajectoryFilters {
    pub projects: Vec<String>,
    pub sources: Vec<String>,
    pub agent_names: Vec<String>,
}

/// Per-agent activity aggregated from collected trajectories.
#[derive(Debug, Clone, Serialize)]
pub struct TrajectoryAgentActivitySummary {
    /// Canonical display name observed in stored trajectories.
    pub agent_name: String,
    /// Most recent source-file modification timestamp in nanoseconds.
    pub last_seen_ns: i64,
    /// Number of recorded trajectory steps.
    pub total_steps: i64,
    /// Prompt and completion tokens across all trajectories.
    pub total_tokens: i64,
}

/// Retention and capacity limits for collected trajectories.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrajectoryMaintenancePolicy {
    /// Maximum trajectory age in days; zero disables age retention.
    pub retention_days: u64,
    /// Maximum logical database size in MiB; zero disables size maintenance.
    pub max_db_size_mb: u64,
}

/// Result of one trajectory maintenance pass.
#[derive(Debug, Clone, Copy)]
pub struct TrajectoryMaintenanceReport {
    /// Trajectory rows removed by age retention.
    pub expired_trajectories: usize,
    /// Stale bookkeeping rows removed.
    pub removed_skipped_files: usize,
    /// Result of capacity enforcement.
    pub size: MaintenanceReport,
}

/// Filter for [`TrajectoryStore::scan_steps`].
///
/// The trajectory-level fields (`project` / `source` / `agent_name` /
/// `session_id`) narrow rows in SQL *before* any `atif_json` is parsed;
/// `categories` is applied per step afterwards.
#[derive(Debug, Clone, Default)]
pub struct StepScanFilter {
    pub project: Option<String>,
    pub source: Option<String>,
    pub agent_name: Option<String>,
    pub session_id: Option<String>,
    /// Match steps carrying **any** of these labels; empty means "any".
    pub categories: Vec<StepCategory>,
    /// Stop after this many hits.
    pub limit: i64,
    /// How many neighbouring steps to return on each side of a hit.
    pub context_radius: i64,
    /// Upper bound on trajectories read from disk, capping query cost.
    pub max_scan: i64,
}

/// One step projected for the query response: the bulky `message` is reduced
/// to a preview and tool/reasoning presence to booleans.
#[derive(Debug, Clone, Serialize)]
pub struct StepView {
    pub step_id: usize,
    /// Step-level role: "user", "agent" or "system".
    pub source: String,
    /// Derived labels; see [`agentsight_atif::StepCategory`].
    pub categories: Vec<String>,
    pub timestamp: Option<String>,
    pub message_preview: String,
    pub message_truncated: bool,
    pub tool_names: Vec<String>,
    pub has_observation: bool,
    pub has_reasoning: bool,
}

/// Neighbouring steps around a hit, for reading it in context.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StepContext {
    pub before: Vec<StepView>,
    pub after: Vec<StepView>,
}

/// A matching step plus the trajectory it belongs to.
#[derive(Debug, Clone, Serialize)]
pub struct StepHit {
    pub session_id: String,
    pub agent_name: String,
    pub project: String,
    /// Which product wrote the file: "qoder" or "qoderwork". Distinct from
    /// `step.source`, which is the step's role.
    pub source: String,
    pub step: StepView,
    pub context: StepContext,
}

/// Result of a step scan.
///
/// No total-match count is reported: the scan stops early once `limit` is
/// reached, so a total would be a guess.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StepScanOutcome {
    pub hits: Vec<StepHit>,
    pub scanned_trajectories: i64,
    /// `true` when the scan stopped early (reached `limit`) or exhausted
    /// `max_scan` with candidates left unread: `hits` is then a prefix of the
    /// full match set, not all of it.
    pub truncated: bool,
    /// Rows whose `atif_json` could not be parsed and were skipped.
    pub skipped_unparsable: i64,
}

/// Thread-safe store over a dedicated `trajectories.db`.
pub struct TrajectoryStore {
    conn: Mutex<Connection>,
    db_path: PathBuf,
}

impl TrajectoryStore {
    /// Opens (creating if needed) the database at `path` and ensures the schema.
    ///
    /// # Errors
    /// Returns an error if the database cannot be opened or the schema
    /// cannot be created.
    pub fn new_with_path(path: &Path) -> Result<Self> {
        let conn = open_connection(path, ConnectionOptions::default())
            .with_context(|| format!("open trajectories db {}", path.display()))?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS collected_trajectories (
                session_id TEXT PRIMARY KEY,
                schema_version TEXT NOT NULL,
                agent_name TEXT NOT NULL,
                model_name TEXT,
                num_steps INTEGER NOT NULL,
                total_prompt_tokens INTEGER,
                total_completion_tokens INTEGER,
                start_time TEXT,
                end_time TEXT,
                atif_json TEXT NOT NULL,
                project TEXT NOT NULL,
                source TEXT NOT NULL,
                is_subagent INTEGER NOT NULL,
                file_path TEXT NOT NULL,
                file_size INTEGER NOT NULL,
                file_mtime_ns INTEGER NOT NULL,
                collected_at_ns INTEGER NOT NULL,
                first_user_message TEXT,
                last_user_message TEXT
            )",
            [],
        )?;
        // Lightweight bookkeeping for files that failed conversion (corrupted
        // JSONL, empty events, etc.) so they are not re-read every scan round.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS skipped_files (
                file_path TEXT PRIMARY KEY,
                file_size INTEGER NOT NULL,
                file_mtime_ns INTEGER NOT NULL
            )",
            [],
        )?;
        migrate_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: path.to_path_buf(),
        })
    }

    /// Opens an existing trajectory database without creating or modifying it.
    ///
    /// # Errors
    /// Returns an error when the file is absent or SQLite cannot open it read-only.
    pub fn open_read_only_existing(path: &Path) -> Result<Self> {
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

    /// Returns `(file_size, file_mtime_ns)` recorded for `file_path`, if any.
    /// Drives the incremental scan: unchanged files are skipped.
    /// Checks both successfully ingested trajectories and skipped (corrupted)
    /// files.
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn get_file_state(&self, file_path: &str) -> Result<Option<(i64, i64)>> {
        let conn = self.lock_conn()?;
        // First check successfully ingested files.
        let state = conn
            .query_row(
                "SELECT file_size, file_mtime_ns FROM collected_trajectories
                 WHERE file_path = ?1",
                params![file_path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if state.is_some() {
            return Ok(state);
        }
        // Then check skipped (corrupted) files.
        let skipped = conn
            .query_row(
                "SELECT file_size, file_mtime_ns FROM skipped_files
                 WHERE file_path = ?1",
                params![file_path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(skipped)
    }

    /// Records the file state for a file that failed conversion, preventing
    /// re-reads until the file changes (size/mtime differ).
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn set_file_state(
        &self,
        file_path: &str,
        file_size: i64,
        file_mtime_ns: i64,
    ) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO skipped_files (file_path, file_size, file_mtime_ns)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(file_path) DO UPDATE SET file_size = ?2, file_mtime_ns = ?3",
            params![file_path, file_size, file_mtime_ns],
        )?;
        Ok(())
    }

    /// Inserts or updates one trajectory row (keyed by `session_id`).
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn upsert_trajectory(&self, record: &TrajectoryRecord) -> Result<()> {
        let now_ns = now_ns();
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO collected_trajectories (
                session_id, schema_version, agent_name, model_name, num_steps,
                total_prompt_tokens, total_completion_tokens, start_time, end_time,
                atif_json, project, source, is_subagent, file_path, file_size,
                file_mtime_ns, collected_at_ns, first_user_message, last_user_message
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
             ON CONFLICT(session_id) DO UPDATE SET
                schema_version = ?2, agent_name = ?3, model_name = ?4, num_steps = ?5,
                total_prompt_tokens = ?6, total_completion_tokens = ?7,
                start_time = ?8, end_time = ?9, atif_json = ?10, project = ?11,
                source = ?12, is_subagent = ?13, file_path = ?14, file_size = ?15,
                file_mtime_ns = ?16, collected_at_ns = ?17,
                first_user_message = ?18, last_user_message = ?19",
            params![
                record.session_id,
                record.schema_version,
                record.agent_name,
                record.model_name,
                record.num_steps,
                record.total_prompt_tokens,
                record.total_completion_tokens,
                record.start_time,
                record.end_time,
                record.atif_json,
                record.project,
                record.source,
                record.is_subagent as i64,
                record.file_path,
                record.file_size,
                record.file_mtime_ns,
                now_ns,
                record.first_user_message,
                record.last_user_message,
            ],
        )?;
        conn.execute(
            "DELETE FROM skipped_files WHERE file_path = ?1",
            params![record.file_path],
        )?;
        Ok(())
    }

    /// Fetches a stored trajectory row by session id (test/inspection helper).
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn get(&self, session_id: &str) -> Result<Option<TrajectoryRecord>> {
        let conn = self.lock_conn()?;
        let record = conn
            .query_row(
                "SELECT session_id, schema_version, agent_name, model_name, num_steps,
                        total_prompt_tokens, total_completion_tokens, start_time, end_time,
                        atif_json, project, source, is_subagent, file_path, file_size,
                        file_mtime_ns, first_user_message, last_user_message
                 FROM collected_trajectories WHERE session_id = ?1",
                params![session_id],
                |row| {
                    Ok(TrajectoryRecord {
                        session_id: row.get(0)?,
                        schema_version: row.get(1)?,
                        agent_name: row.get(2)?,
                        model_name: row.get(3)?,
                        num_steps: row.get(4)?,
                        total_prompt_tokens: row.get(5)?,
                        total_completion_tokens: row.get(6)?,
                        start_time: row.get(7)?,
                        end_time: row.get(8)?,
                        atif_json: row.get(9)?,
                        project: row.get(10)?,
                        source: row.get(11)?,
                        is_subagent: row.get::<_, i64>(12)? != 0,
                        file_path: row.get(13)?,
                        file_size: row.get(14)?,
                        file_mtime_ns: row.get(15)?,
                        first_user_message: row.get(16)?,
                        last_user_message: row.get(17)?,
                    })
                },
            )
            .optional()?;
        Ok(record)
    }

    /// Number of stored trajectories (test/inspection helper).
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn count(&self) -> Result<i64> {
        let conn = self.lock_conn()?;
        let n = conn.query_row("SELECT COUNT(*) FROM collected_trajectories", [], |row| {
            row.get(0)
        })?;
        Ok(n)
    }

    /// Lists trajectory summaries (without `atif_json`), newest first.
    ///
    /// All filters are optional equality matches; `limit` caps the row count.
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn list_summaries(
        &self,
        project: Option<&str>,
        source: Option<&str>,
        agent_name: Option<&str>,
        limit: i64,
    ) -> Result<Vec<TrajectorySummary>> {
        let conn = self.lock_conn()?;
        let mut sql = String::from(
            "SELECT session_id, schema_version, agent_name, model_name, num_steps,
                    total_prompt_tokens, total_completion_tokens, start_time, end_time,
                    project, source, is_subagent, collected_at_ns,
                    first_user_message, last_user_message
             FROM collected_trajectories",
        );
        let mut clauses: Vec<String> = Vec::new();
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(p) = project {
            clauses.push("project = ?".to_string());
            args.push(Box::new(p.to_string()));
        }
        if let Some(s) = source {
            clauses.push("source = ?".to_string());
            args.push(Box::new(s.to_string()));
        }
        if let Some(a) = agent_name {
            clauses.push("agent_name = ?".to_string());
            args.push(Box::new(a.to_string()));
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        // `limit` is bound as a parameter (never interpolated) to stay injection-free.
        sql.push_str(&format!(
            " ORDER BY collected_at_ns DESC LIMIT ?{}",
            args.len() + 1
        ));
        args.push(Box::new(limit));

        let params_ref: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_ref.as_slice(), |row| {
            Ok(TrajectorySummary {
                session_id: row.get(0)?,
                schema_version: row.get(1)?,
                agent_name: row.get(2)?,
                model_name: row.get(3)?,
                num_steps: row.get(4)?,
                total_prompt_tokens: row.get(5)?,
                total_completion_tokens: row.get(6)?,
                start_time: row.get(7)?,
                end_time: row.get(8)?,
                project: row.get(9)?,
                source: row.get(10)?,
                is_subagent: row.get::<_, i64>(11)? != 0,
                collected_at_ns: row.get(12)?,
                first_user_message: row.get(13)?,
                last_user_message: row.get(14)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Returns `(session_id, atif_json)` of the most recent main-agent
    /// trajectories collected at or after `since_collected_at_ns`, newest
    /// first, capped at `limit`.
    ///
    /// Read-only, built for preference analysis: subagent rows are excluded
    /// because their "user" steps are the parent agent's instructions, not
    /// genuine user input. The window filter uses `collected_at_ns` (always
    /// present, monotonic i64) rather than the optional ISO `start_time` /
    /// `end_time` strings.
    ///
    /// Memory / locking: the result holds the complete ATIF JSON string of
    /// every matched row — a single call can return tens of MB — and the
    /// store's connection mutex stays held while the rows are collected.
    /// Callers must keep `limit` conservative and avoid concurrent calls.
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn list_recent_atif_jsons(
        &self,
        since_collected_at_ns: i64,
        limit: i64,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, atif_json FROM collected_trajectories
             WHERE collected_at_ns >= ?1 AND is_subagent = 0
             ORDER BY collected_at_ns DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since_collected_at_ns, limit], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Returns the stored ATIF v1.7 JSON for one session, if present.
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn get_atif_json(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.lock_conn()?;
        let json = conn
            .query_row(
                "SELECT atif_json FROM collected_trajectories WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(json)
    }

    /// Returns the ATIF JSON strings of all subagent trajectories belonging to
    /// the given parent session (matching `<parent>:subagent:%`).
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn get_subagent_atif_jsons(&self, parent_session_id: &str) -> Result<Vec<String>> {
        let conn = self.lock_conn()?;
        let pattern = format!("{parent_session_id}:subagent:%");
        let mut stmt = conn.prepare(
            "SELECT atif_json FROM collected_trajectories WHERE session_id LIKE ?1 \
             ORDER BY session_id",
        )?;
        let rows = stmt.query_map(params![pattern], |row| row.get(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Deletes the subagent rows of `parent_session_id` whose session ids are
    /// not listed in `keep`, returning how many rows were removed.
    ///
    /// Used when a parent is re-generated with a canonical child set: rows left
    /// over from superseded runs would otherwise keep showing up in the parent's
    /// subagent list forever.
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn retain_subagents(&self, parent_session_id: &str, keep: &[String]) -> Result<usize> {
        let conn = self.lock_conn()?;
        let pattern = format!("{parent_session_id}:subagent:%");
        let mut stmt =
            conn.prepare("SELECT session_id FROM collected_trajectories WHERE session_id LIKE ?1")?;
        let existing: Vec<String> = stmt
            .query_map(params![pattern], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        drop(stmt);

        let mut removed = 0;
        for session_id in existing {
            if keep.contains(&session_id) {
                continue;
            }
            conn.execute(
                "DELETE FROM collected_trajectories WHERE session_id = ?1",
                params![session_id],
            )?;
            removed += 1;
        }
        Ok(removed)
    }

    /// Returns distinct project / source / agent_name values for UI filters.
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn list_filters(&self) -> Result<TrajectoryFilters> {
        let conn = self.lock_conn()?;
        Ok(TrajectoryFilters {
            projects: distinct_column(&conn, "project")?,
            sources: distinct_column(&conn, "source")?,
            agent_names: distinct_column(&conn, "agent_name")?,
        })
    }

    /// Aggregates historical activity by agent name, case-insensitively.
    ///
    /// Trajectory steps are the closest available call-level activity measure;
    /// `file_mtime_ns` reflects when the source session was last active.
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn list_agent_activity_summaries(&self) -> Result<Vec<TrajectoryAgentActivitySummary>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT MIN(agent_name) AS display_name,
                    MAX(CASE WHEN file_mtime_ns > 0
                             THEN file_mtime_ns ELSE collected_at_ns END) AS last_seen_ns,
                    COALESCE(SUM(num_steps), 0) AS total_steps,
                    COALESCE(SUM(COALESCE(total_prompt_tokens, 0)
                               + COALESCE(total_completion_tokens, 0)), 0) AS total_tokens
             FROM collected_trajectories
             WHERE TRIM(agent_name) != ''
             GROUP BY agent_name COLLATE NOCASE
             ORDER BY last_seen_ns DESC, display_name ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TrajectoryAgentActivitySummary {
                agent_name: row.get(0)?,
                last_seen_ns: row.get(1)?,
                total_steps: row.get(2)?,
                total_tokens: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Applies age retention, removes stale scan bookkeeping, and enforces capacity.
    ///
    /// # Errors
    /// Returns an error on filesystem inspection, SQLite, or mutex failure.
    pub fn maintain(
        &self,
        policy: TrajectoryMaintenancePolicy,
    ) -> Result<TrajectoryMaintenanceReport> {
        let cutoff_ns = if policy.retention_days == 0 {
            None
        } else {
            Some(
                i64::try_from(retention_cutoff_ns(
                    u64::try_from(now_ns()).unwrap_or(0),
                    policy.retention_days,
                )?)
                .unwrap_or(i64::MAX),
            )
        };
        let removed_skipped_files = self.remove_stale_skipped_files(cutoff_ns)?;
        let expired_trajectories = match cutoff_ns {
            Some(cutoff) => self.delete_trajectories_before(cutoff)?,
            None => 0,
        };
        let limit_bytes = policy.max_db_size_mb.saturating_mul(1024 * 1024);
        if (expired_trajectories > 0 || removed_skipped_files > 0 || limit_bytes > 0)
            && self.checkpoint()? == CheckpointOutcome::Busy
        {
            let snapshot = self.size_snapshot()?;
            return Ok(TrajectoryMaintenanceReport {
                expired_trajectories,
                removed_skipped_files,
                size: MaintenanceReport {
                    status: MaintenanceStatus::CheckpointBusy,
                    rounds: 0,
                    deleted_rows: 0,
                    before: snapshot,
                    after: snapshot,
                },
            });
        }

        let size = enforce_size_policy::<anyhow::Error>(
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
        Ok(TrajectoryMaintenanceReport {
            expired_trajectories,
            removed_skipped_files,
            size,
        })
    }

    fn delete_trajectories_before(&self, cutoff_ns: i64) -> Result<usize> {
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        transaction.execute(
            "INSERT OR REPLACE INTO skipped_files (file_path, file_size, file_mtime_ns)
             SELECT file_path, file_size, file_mtime_ns FROM collected_trajectories
             WHERE collected_at_ns < ?1",
            params![cutoff_ns],
        )?;
        let deleted = transaction.execute(
            "DELETE FROM collected_trajectories WHERE collected_at_ns < ?1",
            params![cutoff_ns],
        )?;
        transaction.commit()?;
        Ok(deleted)
    }

    fn delete_oldest_fraction(&self, fraction: f64) -> Result<usize> {
        let mut conn = self.lock_conn()?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM collected_trajectories", [], |row| {
                row.get(0)
            })?;
        if count == 0 || fraction <= 0.0 {
            return Ok(0);
        }
        let limit = ((count as f64 * fraction.clamp(0.0, 1.0)) as i64).max(1);
        let transaction = conn.transaction()?;
        transaction.execute(
            "INSERT OR REPLACE INTO skipped_files (file_path, file_size, file_mtime_ns)
             SELECT file_path, file_size, file_mtime_ns FROM collected_trajectories
             WHERE session_id IN (
                 SELECT session_id FROM collected_trajectories
                 ORDER BY collected_at_ns ASC, session_id ASC LIMIT ?1
             )",
            params![limit],
        )?;
        let deleted = transaction.execute(
            "DELETE FROM collected_trajectories WHERE session_id IN (
                SELECT session_id FROM collected_trajectories
                ORDER BY collected_at_ns ASC, session_id ASC LIMIT ?1
            )",
            params![limit],
        )?;
        transaction.commit()?;
        Ok(deleted)
    }

    fn remove_stale_skipped_files(&self, cutoff_ns: Option<i64>) -> Result<usize> {
        let stale_paths = {
            let conn = self.lock_conn()?;
            let mut statement = conn.prepare(
                "SELECT skipped.file_path, skipped.file_mtime_ns,
                        EXISTS(SELECT 1 FROM collected_trajectories AS trajectories
                               WHERE trajectories.file_path = skipped.file_path)
                 FROM skipped_files AS skipped",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .filter_map(|(path, modified_ns, has_trajectory)| {
                    let expired = cutoff_ns.is_some_and(|cutoff| modified_ns < cutoff);
                    (expired || has_trajectory || !Path::new(&path).exists()).then_some(path)
                })
                .collect::<Vec<_>>()
        };

        let conn = self.lock_conn()?;
        let mut removed = 0usize;
        for path in stale_paths {
            removed += conn.execute(
                "DELETE FROM skipped_files WHERE file_path = ?1",
                params![path],
            )?;
        }
        Ok(removed)
    }

    fn size_snapshot(&self) -> Result<agentsight_sqlite_lifecycle::SizeSnapshot> {
        let conn = self.lock_conn()?;
        measure_database(&self.db_path, &conn).map_err(Into::into)
    }

    fn checkpoint(&self) -> Result<CheckpointOutcome> {
        let conn = self.lock_conn()?;
        checkpoint_truncate(&conn).map_err(Into::into)
    }

    /// Finds steps matching `filter`, each with its neighbouring steps.
    ///
    /// Steps are not stored as rows: they live inside the `atif_json` column,
    /// so matching requires deserializing candidate trajectories. Cost is
    /// bounded three ways — the SQL `WHERE` narrows candidates first, newest
    /// trajectories are visited first, and `max_scan` caps how many are read
    /// at all. Rows whose JSON fails to parse are counted and skipped rather
    /// than failing the query, since one corrupt document must not hide every
    /// other result.
    ///
    /// # Errors
    /// Returns an error on SQL failure or poisoned mutex.
    pub fn scan_steps(&self, filter: &StepScanFilter) -> Result<StepScanOutcome> {
        let conn = self.lock_conn()?;
        let mut sql = String::from(
            "SELECT session_id, agent_name, project, source, atif_json
             FROM collected_trajectories",
        );
        let mut clauses: Vec<String> = Vec::new();
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(p) = &filter.project {
            clauses.push("project = ?".to_string());
            args.push(Box::new(p.clone()));
        }
        if let Some(s) = &filter.source {
            clauses.push("source = ?".to_string());
            args.push(Box::new(s.clone()));
        }
        if let Some(a) = &filter.agent_name {
            clauses.push("agent_name = ?".to_string());
            args.push(Box::new(a.clone()));
        }
        if let Some(sid) = &filter.session_id {
            clauses.push("session_id = ?".to_string());
            args.push(Box::new(sid.clone()));
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        // One row beyond `max_scan` is requested purely to detect whether the
        // cap hid further candidates. `limit` is bound as a parameter (never
        // interpolated) to stay injection-free.
        sql.push_str(&format!(
            " ORDER BY collected_at_ns DESC LIMIT ?{}",
            args.len() + 1
        ));
        args.push(Box::new(filter.max_scan.saturating_add(1)));

        let params_ref: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(params_ref.as_slice())?;

        let mut outcome = StepScanOutcome::default();
        while let Some(row) = rows.next()? {
            if outcome.scanned_trajectories >= filter.max_scan {
                // This row exists beyond the cap, so candidates were left unread.
                outcome.truncated = true;
                break;
            }
            let session_id: String = row.get(0)?;
            let agent_name: String = row.get(1)?;
            let project: String = row.get(2)?;
            let source: String = row.get(3)?;
            let atif_json: String = row.get(4)?;
            outcome.scanned_trajectories += 1;

            let Ok(trajectory) = serde_json::from_str::<AtifTrajectory>(&atif_json) else {
                outcome.skipped_unparsable += 1;
                continue;
            };
            let reached_limit = collect_step_hits(
                &trajectory,
                &session_id,
                &agent_name,
                &project,
                &source,
                filter,
                &mut outcome.hits,
            );
            if reached_limit {
                outcome.truncated = true;
                break;
            }
        }
        Ok(outcome)
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow!("trajectory store mutex poisoned"))
    }
}

/// Appends the steps of one trajectory that match `filter` to `hits`.
///
/// Returns `true` once `filter.limit` is reached so the caller stops reading
/// further trajectories.
fn collect_step_hits(
    trajectory: &AtifTrajectory,
    session_id: &str,
    agent_name: &str,
    project: &str,
    source: &str,
    filter: &StepScanFilter,
    hits: &mut Vec<StepHit>,
) -> bool {
    let radius = usize::try_from(filter.context_radius.max(0)).unwrap_or(0);
    let limit = usize::try_from(filter.limit.max(0)).unwrap_or(0);
    let steps = &trajectory.steps;
    for (idx, step) in steps.iter().enumerate() {
        if hits.len() >= limit {
            return true;
        }
        if !filter.categories.is_empty() {
            let categories = step.categories();
            if !filter.categories.iter().any(|c| categories.contains(c)) {
                continue;
            }
        }
        // Neighbours come from the same in-memory slice, so context is free.
        // Saturating/min keeps the ranges valid at the first and last step.
        let before_start = idx.saturating_sub(radius);
        let after_end = idx
            .saturating_add(radius)
            .saturating_add(1)
            .min(steps.len());
        hits.push(StepHit {
            session_id: session_id.to_string(),
            agent_name: agent_name.to_string(),
            project: project.to_string(),
            source: source.to_string(),
            step: step_view(step),
            context: StepContext {
                before: steps[before_start..idx].iter().map(step_view).collect(),
                after: steps[idx + 1..after_end].iter().map(step_view).collect(),
            },
        });
    }
    false
}

/// Projects a step into the response shape.
fn step_view(step: &Step) -> StepView {
    let (message_preview, message_truncated) =
        preview_chars(&step.message, STEP_MESSAGE_PREVIEW_CHARS);
    StepView {
        step_id: step.step_id,
        source: step.source.as_str().to_string(),
        categories: step
            .categories()
            .iter()
            .map(|c| c.as_str().to_string())
            .collect(),
        timestamp: step.timestamp.clone(),
        message_preview,
        message_truncated,
        tool_names: step.tool_names().iter().map(|n| (*n).to_string()).collect(),
        has_observation: step.has_observation(),
        has_reasoning: step.has_reasoning(),
    }
}

/// Truncates `text` to at most `max` **characters**, returning the preview and
/// whether anything was cut.
///
/// Counting characters (not bytes) is mandatory: trajectory content is largely
/// CJK, and slicing by byte offset would split a multi-byte codepoint and panic.
fn preview_chars(text: &str, max: usize) -> (String, bool) {
    let preview: String = text.chars().take(max).collect();
    let truncated = text.chars().nth(max).is_some();
    (preview, truncated)
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Current schema version recorded in `PRAGMA user_version`.
/// v1: preview columns added; v2: previews recomputed with system-context
/// stripping (see [`extract_user_message_previews`]).
const SCHEMA_USER_VERSION: i32 = 2;

/// Max characters kept per user-message preview column.
const MESSAGE_PREVIEW_CHARS: usize = 200;

/// Max characters kept per step message in [`TrajectoryStore::scan_steps`]
/// results. Larger than [`MESSAGE_PREVIEW_CHARS`] because these previews are
/// read by a human inspecting a step and its surrounding context.
const STEP_MESSAGE_PREVIEW_CHARS: usize = 500;

/// XML-style tag pairs injected into user turns by IDEs/CLIs (Qoder slash
/// commands, Claude Code local-command transcripts, system reminders). Their
/// content is not genuine user text and must not surface in previews.
const SYSTEM_TAG_PAIRS: &[(&str, &str)] = &[
    ("<system-reminder>", "</system-reminder>"),
    ("<current_notes_content>", "</current_notes_content>"),
    ("<command-message>", "</command-message>"),
    ("<command-name>", "</command-name>"),
    ("<command-args>", "</command-args>"),
    ("<local-command-caveat>", "</local-command-caveat>"),
    ("<local-command-stdout>", "</local-command-stdout>"),
    ("<local-command-stderr>", "</local-command-stderr>"),
];

/// Remove system-injected tag blocks from a user message. An unterminated
/// opening tag swallows the rest of the text (matches AgentOpt semantics).
/// Public because preference analysis needs the same cleaning when it reads
/// user turns straight out of stored ATIF steps.
///
/// Stable contract for external callers: only the `SYSTEM_TAG_PAIRS`
/// blocks are removed; user-authored text outside those blocks is preserved
/// verbatim, apart from collapsing the blank runs the removal leaves behind
/// and trimming the edges.
pub fn strip_system_context(text: &str) -> String {
    let mut result = text.to_string();
    for &(open, close) in SYSTEM_TAG_PAIRS {
        while let Some(start) = result.find(open) {
            if let Some(end_rel) = result[start..].find(close) {
                let end = start + end_rel + close.len();
                result.replace_range(start..end, "");
            } else {
                result.truncate(start);
                break;
            }
        }
    }
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    result.trim().to_string()
}

/// One-shot schema migration for databases created before the user-message
/// preview columns existed: adds the columns (no-op on fresh databases where
/// `CREATE TABLE` already includes them) and (re)computes them from the
/// stored ATIF JSON. v1→v2 recomputes every row because v2 added
/// system-context stripping to the extractor.
fn migrate_schema(conn: &Connection) -> Result<()> {
    let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version >= SCHEMA_USER_VERSION {
        return Ok(());
    }
    for col in ["first_user_message", "last_user_message"] {
        let sql = format!("ALTER TABLE collected_trajectories ADD COLUMN {col} TEXT");
        if let Err(e) = conn.execute(&sql, []) {
            // Fresh databases already have the column via CREATE TABLE.
            if !e.to_string().contains("duplicate column name") {
                return Err(e.into());
            }
        }
    }
    backfill_message_previews(conn)?;
    conn.pragma_update(None, "user_version", SCHEMA_USER_VERSION)?;
    Ok(())
}

/// Recomputes the preview columns for every row from its `atif_json`.
fn backfill_message_previews(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT session_id, atif_json FROM collected_trajectories")?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    drop(stmt);
    for (session_id, atif_json) in rows {
        let (first, last) = extract_user_message_previews(&atif_json);
        conn.execute(
            "UPDATE collected_trajectories
             SET first_user_message = ?2, last_user_message = ?3
             WHERE session_id = ?1",
            params![session_id, first, last],
        )?;
    }
    Ok(())
}

/// Extracts first/last user-authored message previews (≤ 200 chars) from an
/// ATIF JSON document. System-injected tag blocks are stripped first; user
/// steps left empty after stripping are skipped. Parses generically
/// (tolerant of schema drift) and returns `(None, None)` on malformed input
/// or when no user step carries genuine text.
pub fn extract_user_message_previews(atif_json: &str) -> (Option<String>, Option<String>) {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(atif_json) else {
        return (None, None);
    };
    let Some(steps) = doc.get("steps").and_then(|s| s.as_array()) else {
        return (None, None);
    };
    let mut first = None;
    let mut last = None;
    for step in steps {
        if step.get("source").and_then(|s| s.as_str()) != Some("user") {
            continue;
        }
        let Some(msg) = step.get("message").and_then(|m| m.as_str()) else {
            continue;
        };
        let cleaned = strip_system_context(msg);
        if cleaned.is_empty() {
            continue;
        }
        let preview: String = cleaned.chars().take(MESSAGE_PREVIEW_CHARS).collect();
        if first.is_none() {
            first = Some(preview.clone());
        }
        last = Some(preview);
    }
    (first, last)
}

/// SELECT DISTINCT on a fixed column name (hard-coded, never user input).
fn distinct_column(conn: &Connection, column: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT {column} FROM collected_trajectories ORDER BY {column}"
    ))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_db(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("traj-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("trajectories.db")
    }

    fn sample_record() -> TrajectoryRecord {
        TrajectoryRecord {
            session_id: "s-1".into(),
            schema_version: "ATIF-v1.7".into(),
            agent_name: "qoder".into(),
            model_name: Some("qwen-max".into()),
            num_steps: 3,
            total_prompt_tokens: Some(250),
            total_completion_tokens: Some(30),
            start_time: Some("2026-07-25T10:00:00Z".into()),
            end_time: Some("2026-07-25T10:00:05Z".into()),
            first_user_message: Some("修复登录 bug".into()),
            last_user_message: Some("再跑一遍测试".into()),
            atif_json: "{\"schema_version\":\"ATIF-v1.7\"}".into(),
            project: "myapp".into(),
            source: "qoder".into(),
            is_subagent: false,
            file_path: "/root/.qoder/projects/-data-myapp/s-1.jsonl".into(),
            file_size: 1024,
            file_mtime_ns: 42,
        }
    }

    #[test]
    fn test_upsert_twice_updates_row() {
        let store = TrajectoryStore::new_with_path(&tmp_db("upsert")).unwrap();
        let mut rec = sample_record();
        store.upsert_trajectory(&rec).unwrap();

        rec.num_steps = 5;
        rec.file_size = 2048;
        rec.file_mtime_ns = 99;
        store.upsert_trajectory(&rec).unwrap();

        assert_eq!(store.count().unwrap(), 1);
        let got = store.get("s-1").unwrap().unwrap();
        assert_eq!(got.num_steps, 5);
        assert_eq!(got.file_size, 2048);
    }

    #[test]
    fn test_retain_subagents_deletes_only_unlisted_children() {
        let store = TrajectoryStore::new_with_path(&tmp_db("retain")).unwrap();
        for id in [
            "p-1",
            "p-1:subagent:keep",
            "p-1:subagent:drop",
            "p-2:subagent:other",
        ] {
            let mut rec = sample_record();
            rec.session_id = id.into();
            store.upsert_trajectory(&rec).unwrap();
        }

        let removed = store
            .retain_subagents("p-1", &["p-1:subagent:keep".to_string()])
            .unwrap();

        assert_eq!(removed, 1);
        assert!(store.get("p-1:subagent:keep").unwrap().is_some());
        assert!(store.get("p-1:subagent:drop").unwrap().is_none());
        // The parent row itself and other parents' children are untouched.
        assert!(store.get("p-1").unwrap().is_some());
        assert!(store.get("p-2:subagent:other").unwrap().is_some());
    }

    #[test]
    fn test_file_state_roundtrip() {
        let store = TrajectoryStore::new_with_path(&tmp_db("state")).unwrap();
        let rec = sample_record();
        assert!(store.get_file_state(&rec.file_path).unwrap().is_none());

        store.upsert_trajectory(&rec).unwrap();
        assert_eq!(
            store.get_file_state(&rec.file_path).unwrap(),
            Some((1024, 42))
        );
    }

    #[test]
    fn test_list_summaries_filters_and_limit() {
        let store = TrajectoryStore::new_with_path(&tmp_db("list")).unwrap();
        let mut a = sample_record();
        a.session_id = "a".into();
        a.project = "p1".into();
        a.source = "qoder".into();
        a.agent_name = "qoder".into();
        store.upsert_trajectory(&a).unwrap();

        let mut b = sample_record();
        b.session_id = "b".into();
        b.project = "p2".into();
        b.source = "qoderwork".into();
        b.agent_name = "qoder".into();
        b.file_path = "/root/.qoderwork/projects/x/b.jsonl".into();
        store.upsert_trajectory(&b).unwrap();

        // No filter → both rows.
        assert_eq!(
            store.list_summaries(None, None, None, 100).unwrap().len(),
            2
        );
        // project filter
        let p1 = store.list_summaries(Some("p1"), None, None, 100).unwrap();
        assert_eq!(p1.len(), 1);
        assert_eq!(p1[0].session_id, "a");
        // source filter
        let qw = store
            .list_summaries(None, Some("qoderwork"), None, 100)
            .unwrap();
        assert_eq!(qw.len(), 1);
        assert_eq!(qw[0].session_id, "b");
        // combined filters (AND)
        assert_eq!(
            store
                .list_summaries(Some("p1"), Some("qoderwork"), None, 100)
                .unwrap()
                .len(),
            0
        );
        // limit caps the result
        assert_eq!(store.list_summaries(None, None, None, 1).unwrap().len(), 1);
    }

    #[test]
    fn test_list_recent_atif_jsons_window_limit_and_subagents() {
        let store = TrajectoryStore::new_with_path(&tmp_db("recent")).unwrap();
        for (id, is_subagent) in [("m-1", false), ("m-2", false), ("m-1:subagent:x", true)] {
            let mut rec = sample_record();
            rec.session_id = id.into();
            rec.is_subagent = is_subagent;
            store.upsert_trajectory(&rec).unwrap();
        }

        // Subagent rows never appear; the newest row comes first.
        let rows = store.list_recent_atif_jsons(0, 100).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(id, _)| !id.contains("subagent")));
        assert_eq!(rows[0].1, sample_record().atif_json);

        // The limit caps the result set.
        assert_eq!(store.list_recent_atif_jsons(0, 1).unwrap().len(), 1);

        // A window bound in the future excludes everything.
        assert!(store
            .list_recent_atif_jsons(i64::MAX, 100)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_get_atif_json_hit_and_miss() {
        let store = TrajectoryStore::new_with_path(&tmp_db("atif")).unwrap();
        store.upsert_trajectory(&sample_record()).unwrap();
        assert_eq!(
            store.get_atif_json("s-1").unwrap().as_deref(),
            Some("{\"schema_version\":\"ATIF-v1.7\"}")
        );
        assert!(store.get_atif_json("nope").unwrap().is_none());
    }

    #[test]
    fn test_extract_user_message_previews() {
        let atif = r#"{"steps":[
            {"step_id":1,"source":"system","message":"sys prompt"},
            {"step_id":2,"source":"user","message":"  第一条用户消息  "},
            {"step_id":3,"source":"agent","message":"好的"},
            {"step_id":4,"source":"user","message":"最后一条"}
        ]}"#;
        let (first, last) = extract_user_message_previews(atif);
        assert_eq!(first.as_deref(), Some("第一条用户消息"));
        assert_eq!(last.as_deref(), Some("最后一条"));

        // Malformed / empty inputs degrade to (None, None).
        assert_eq!(extract_user_message_previews("not json"), (None, None));
        assert_eq!(extract_user_message_previews("{}"), (None, None));
        let no_user = r#"{"steps":[{"step_id":1,"source":"agent","message":"x"}]}"#;
        assert_eq!(extract_user_message_previews(no_user), (None, None));

        // Long messages are truncated to 200 chars.
        let long_msg = "啦".repeat(300);
        let atif_long =
            format!(r#"{{"steps":[{{"step_id":1,"source":"user","message":"{long_msg}"}}]}}"#);
        let (first, _) = extract_user_message_previews(&atif_long);
        assert_eq!(first.map(|s| s.chars().count()), Some(200));
    }

    #[test]
    fn test_extract_strips_system_injected_tags() {
        // Slash-command turns and local-command caveats are not user text;
        // the preview must fall through to the first genuine message.
        let atif = r#"{"steps":[
            {"step_id":1,"source":"user","message":"<command-message>clear</command-message> <command-name>/clear</command-name>"},
            {"step_id":2,"source":"user","message":"<local-command-caveat>Caveat: The messages below were generated by the user while running local commands.</local-command-caveat>\n那你看看当前能做到么"},
            {"step_id":3,"source":"user","message":"<system-reminder>injected memo</system-reminder>再跑一遍测试"}
        ]}"#;
        let (first, last) = extract_user_message_previews(atif);
        assert_eq!(first.as_deref(), Some("那你看看当前能做到么"));
        assert_eq!(last.as_deref(), Some("再跑一遍测试"));

        // Unterminated tag swallows the rest of the block.
        let atif2 = r#"{"steps":[{"step_id":1,"source":"user","message":"<local-command-caveat>Caveat: truncated"}]}"#;
        assert_eq!(extract_user_message_previews(atif2), (None, None));
    }

    #[test]
    fn test_migration_backfills_legacy_rows() {
        let db = tmp_db("migrate");
        // Seed a row with stale previews (as written by schema v1, without
        // system-context stripping), then reset user_version to simulate a
        // legacy database.
        {
            let store = TrajectoryStore::new_with_path(&db).unwrap();
            let mut rec = sample_record();
            rec.first_user_message = Some("<command-message>clear</command-message>".into());
            rec.last_user_message = None;
            rec.atif_json = r#"{"steps":[
                {"step_id":1,"source":"user","message":"<command-message>clear</command-message>"},
                {"step_id":2,"source":"user","message":"首条"},
                {"step_id":3,"source":"user","message":"末条"}
            ]}"#
            .into();
            store.upsert_trajectory(&rec).unwrap();
        }
        {
            let conn = Connection::open(&db).unwrap();
            conn.pragma_update(None, "user_version", 0).unwrap();
        }

        // Reopen → migration recomputes previews from atif_json, replacing
        // the stale tag-polluted value.
        let store = TrajectoryStore::new_with_path(&db).unwrap();
        let got = store.get("s-1").unwrap().unwrap();
        assert_eq!(got.first_user_message.as_deref(), Some("首条"));
        assert_eq!(got.last_user_message.as_deref(), Some("末条"));

        let rows = store.list_summaries(None, None, None, 10).unwrap();
        assert_eq!(rows[0].first_user_message.as_deref(), Some("首条"));
        assert_eq!(rows[0].last_user_message.as_deref(), Some("末条"));
    }

    #[test]
    fn test_list_filters_distinct() {
        let store = TrajectoryStore::new_with_path(&tmp_db("filters")).unwrap();
        let mut a = sample_record();
        a.session_id = "a".into();
        a.project = "p1".into();
        a.source = "qoder".into();
        a.agent_name = "qoder".into();
        store.upsert_trajectory(&a).unwrap();

        let mut b = sample_record();
        b.session_id = "b".into();
        b.project = "p1".into();
        b.source = "qoderwork".into();
        b.agent_name = "qoder".into();
        b.file_path = "/root/.qoderwork/projects/x/b.jsonl".into();
        store.upsert_trajectory(&b).unwrap();

        let f = store.list_filters().unwrap();
        assert_eq!(f.projects, vec!["p1".to_string()]);
        assert_eq!(
            f.sources,
            vec!["qoder".to_string(), "qoderwork".to_string()]
        );
        assert_eq!(f.agent_names, vec!["qoder".to_string()]);
    }

    #[test]
    fn read_only_open_requires_an_existing_database() {
        let path = tmp_db("read-only");
        assert!(TrajectoryStore::open_read_only_existing(&path).is_err());

        let writable = TrajectoryStore::new_with_path(&path).unwrap();
        writable.upsert_trajectory(&sample_record()).unwrap();
        drop(writable);

        let read_only = TrajectoryStore::open_read_only_existing(&path).unwrap();
        assert!(read_only.get("s-1").unwrap().is_some());
    }

    #[test]
    fn maintenance_removes_expired_trajectory() {
        let store = TrajectoryStore::new_with_path(&tmp_db("maintenance-age")).unwrap();
        store.upsert_trajectory(&sample_record()).unwrap();
        store
            .lock_conn()
            .unwrap()
            .execute(
                "UPDATE collected_trajectories SET collected_at_ns = 1 WHERE session_id = ?1",
                params!["s-1"],
            )
            .unwrap();

        let report = store
            .maintain(TrajectoryMaintenancePolicy {
                retention_days: 1,
                max_db_size_mb: 0,
            })
            .unwrap();

        assert_eq!(report.expired_trajectories, 1);
        assert!(store.get("s-1").unwrap().is_none());

        let cleanup = store
            .maintain(TrajectoryMaintenancePolicy {
                retention_days: 1,
                max_db_size_mb: 0,
            })
            .unwrap();
        assert_eq!(cleanup.removed_skipped_files, 1);
    }

    #[test]
    fn maintenance_trims_oldest_trajectories_to_size_target() {
        let store = TrajectoryStore::new_with_path(&tmp_db("maintenance-size")).unwrap();
        let payload = "x".repeat(20_000);
        for index in 0..100 {
            let mut record = sample_record();
            record.session_id = format!("session-{index:03}");
            record.file_path = format!("/tmp/session-{index:03}.jsonl");
            record.atif_json = payload.clone();
            store.upsert_trajectory(&record).unwrap();
        }

        let report = store
            .maintain(TrajectoryMaintenancePolicy {
                retention_days: 0,
                max_db_size_mb: 1,
            })
            .unwrap();

        assert!(report.size.deleted_rows > 0);
        assert!(store.get("session-099").unwrap().is_some());
    }

    #[test]
    fn test_agent_activity_summaries_group_names_and_aggregate_steps() {
        let store = TrajectoryStore::new_with_path(&tmp_db("agent-activity")).unwrap();
        let mut first = sample_record();
        first.session_id = "first".into();
        first.agent_name = "Qoder".into();
        first.num_steps = 3;
        first.total_prompt_tokens = Some(100);
        first.total_completion_tokens = Some(20);
        first.file_mtime_ns = 100;
        store.upsert_trajectory(&first).unwrap();

        let mut second = sample_record();
        second.session_id = "second".into();
        second.agent_name = "qoder".into();
        second.num_steps = 5;
        second.total_prompt_tokens = Some(200);
        second.total_completion_tokens = None;
        second.file_path = "/root/.qoder/projects/myapp/second.jsonl".into();
        second.file_mtime_ns = 300;
        store.upsert_trajectory(&second).unwrap();

        let summaries = store.list_agent_activity_summaries().unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].agent_name, "Qoder");
        assert_eq!(summaries[0].last_seen_ns, 300);
        assert_eq!(summaries[0].total_steps, 8);
        assert_eq!(summaries[0].total_tokens, 320);
    }

    // ─── scan_steps ──────────────────────────────────────────────────

    /// Baseline filter: no narrowing, generous limits.
    fn scan_all() -> StepScanFilter {
        StepScanFilter {
            limit: 100,
            context_radius: 3,
            max_scan: 100,
            ..Default::default()
        }
    }

    /// A 5-step trajectory covering every category:
    /// 1 user, 2 agent+thinking+tool_call, 3 tool_result, 4 agent, 5 system.
    fn five_step_atif() -> String {
        serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "agent": {"name": "qoder", "version": "1.0"},
            "session_id": "s-1",
            "steps": [
                {"step_id": 1, "source": "user", "message": "list files"},
                {"step_id": 2, "source": "agent", "message": "Checking.",
                 "reasoning_content": "need ls",
                 "tool_calls": [{"tool_call_id": "t1", "function_name": "bash",
                                 "arguments": {"cmd": "ls"}}]},
                {"step_id": 3, "source": "user", "message": "",
                 "observation": {"results": [{"source_call_id": "t1", "content": "a.txt"}]}},
                {"step_id": 4, "source": "agent", "message": "Only a.txt exists."},
                {"step_id": 5, "source": "system", "message": "session ended"}
            ]
        })
        .to_string()
    }

    fn seed_five_steps(store: &TrajectoryStore) {
        let mut rec = sample_record();
        rec.num_steps = 5;
        rec.atif_json = five_step_atif();
        store.upsert_trajectory(&rec).unwrap();
    }

    #[test]
    fn test_scan_steps_single_category() {
        let store = TrajectoryStore::new_with_path(&tmp_db("scan-single")).unwrap();
        seed_five_steps(&store);

        let filter = StepScanFilter {
            categories: vec![StepCategory::ToolCall],
            ..scan_all()
        };
        let out = store.scan_steps(&filter).unwrap();
        assert_eq!(out.hits.len(), 1);
        assert_eq!(out.hits[0].step.step_id, 2);
        assert_eq!(out.hits[0].step.tool_names, vec!["bash".to_string()]);
        assert_eq!(out.scanned_trajectories, 1);
        assert!(!out.truncated);
        assert_eq!(out.skipped_unparsable, 0);
        // Trajectory-level source stays the product name, not the step role.
        assert_eq!(out.hits[0].source, "qoder");
        assert_eq!(out.hits[0].step.source, "agent");
    }

    #[test]
    fn test_scan_steps_multi_category_is_or() {
        let store = TrajectoryStore::new_with_path(&tmp_db("scan-multi")).unwrap();
        seed_five_steps(&store);

        let filter = StepScanFilter {
            categories: vec![StepCategory::System, StepCategory::ToolResult],
            ..scan_all()
        };
        let out = store.scan_steps(&filter).unwrap();
        let ids: Vec<usize> = out.hits.iter().map(|h| h.step.step_id).collect();
        assert_eq!(ids, vec![3, 5]);
    }

    #[test]
    fn test_scan_steps_no_category_returns_every_step() {
        let store = TrajectoryStore::new_with_path(&tmp_db("scan-nocat")).unwrap();
        seed_five_steps(&store);

        let out = store.scan_steps(&scan_all()).unwrap();
        assert_eq!(out.hits.len(), 5);
    }

    #[test]
    fn test_scan_steps_context_window_and_boundaries() {
        let store = TrajectoryStore::new_with_path(&tmp_db("scan-ctx")).unwrap();
        seed_five_steps(&store);

        // Middle step: full window on both sides.
        let mid = store
            .scan_steps(&StepScanFilter {
                categories: vec![StepCategory::ToolResult],
                context_radius: 2,
                ..scan_all()
            })
            .unwrap();
        let hit = &mid.hits[0];
        assert_eq!(hit.step.step_id, 3);
        assert_eq!(
            hit.context
                .before
                .iter()
                .map(|s| s.step_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            hit.context
                .after
                .iter()
                .map(|s| s.step_id)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );

        // First step: nothing before it.
        let first = store
            .scan_steps(&StepScanFilter {
                categories: vec![StepCategory::UserInput],
                context_radius: 3,
                ..scan_all()
            })
            .unwrap();
        let first_hit = &first.hits[0];
        assert_eq!(first_hit.step.step_id, 1);
        assert!(first_hit.context.before.is_empty());
        assert_eq!(first_hit.context.after.len(), 3);

        // Last step: nothing after it.
        let last = store
            .scan_steps(&StepScanFilter {
                categories: vec![StepCategory::System],
                context_radius: 3,
                ..scan_all()
            })
            .unwrap();
        let last_hit = &last.hits[0];
        assert_eq!(last_hit.step.step_id, 5);
        assert_eq!(last_hit.context.before.len(), 3);
        assert!(last_hit.context.after.is_empty());
    }

    #[test]
    fn test_scan_steps_zero_context() {
        let store = TrajectoryStore::new_with_path(&tmp_db("scan-ctx0")).unwrap();
        seed_five_steps(&store);

        let out = store
            .scan_steps(&StepScanFilter {
                categories: vec![StepCategory::ToolResult],
                context_radius: 0,
                ..scan_all()
            })
            .unwrap();
        assert!(out.hits[0].context.before.is_empty());
        assert!(out.hits[0].context.after.is_empty());
    }

    #[test]
    fn test_scan_steps_limit_marks_truncated() {
        let store = TrajectoryStore::new_with_path(&tmp_db("scan-limit")).unwrap();
        seed_five_steps(&store);

        let out = store
            .scan_steps(&StepScanFilter {
                limit: 2,
                ..scan_all()
            })
            .unwrap();
        assert_eq!(out.hits.len(), 2);
        assert!(out.truncated);
    }

    #[test]
    fn test_scan_steps_max_scan_marks_truncated() {
        let store = TrajectoryStore::new_with_path(&tmp_db("scan-maxscan")).unwrap();
        for id in ["s-1", "s-2", "s-3"] {
            let mut rec = sample_record();
            rec.session_id = id.into();
            rec.file_path = format!("/root/.qoder/projects/x/{id}.jsonl");
            rec.atif_json = five_step_atif();
            store.upsert_trajectory(&rec).unwrap();
        }

        let out = store
            .scan_steps(&StepScanFilter {
                max_scan: 2,
                ..scan_all()
            })
            .unwrap();
        assert_eq!(out.scanned_trajectories, 2);
        assert!(out.truncated);

        // Exactly as many candidates as the cap is not truncation.
        let exact = store
            .scan_steps(&StepScanFilter {
                max_scan: 3,
                ..scan_all()
            })
            .unwrap();
        assert_eq!(exact.scanned_trajectories, 3);
        assert!(!exact.truncated);
    }

    #[test]
    fn test_scan_steps_skips_unparsable_without_failing() {
        let store = TrajectoryStore::new_with_path(&tmp_db("scan-bad")).unwrap();
        let mut bad = sample_record();
        bad.session_id = "bad".into();
        bad.file_path = "/root/.qoder/projects/x/bad.jsonl".into();
        bad.atif_json = "{not valid json".into();
        store.upsert_trajectory(&bad).unwrap();
        seed_five_steps(&store);

        // One corrupt document must not hide the healthy trajectory's steps.
        let out = store.scan_steps(&scan_all()).unwrap();
        assert_eq!(out.skipped_unparsable, 1);
        assert_eq!(out.scanned_trajectories, 2);
        assert_eq!(out.hits.len(), 5);
    }

    #[test]
    fn test_scan_steps_narrows_by_trajectory_fields() {
        let store = TrajectoryStore::new_with_path(&tmp_db("scan-narrow")).unwrap();
        seed_five_steps(&store);
        let mut other = sample_record();
        other.session_id = "s-2".into();
        other.agent_name = "qoderwork".into();
        other.project = "otherapp".into();
        other.file_path = "/root/.qoderwork/projects/y/s-2.jsonl".into();
        other.atif_json = five_step_atif();
        store.upsert_trajectory(&other).unwrap();

        let by_session = store
            .scan_steps(&StepScanFilter {
                session_id: Some("s-2".into()),
                ..scan_all()
            })
            .unwrap();
        assert_eq!(by_session.scanned_trajectories, 1);
        assert!(by_session.hits.iter().all(|h| h.session_id == "s-2"));

        let by_agent = store
            .scan_steps(&StepScanFilter {
                agent_name: Some("qoder".into()),
                ..scan_all()
            })
            .unwrap();
        assert_eq!(by_agent.scanned_trajectories, 1);
        assert!(by_agent.hits.iter().all(|h| h.agent_name == "qoder"));

        let by_project = store
            .scan_steps(&StepScanFilter {
                project: Some("otherapp".into()),
                ..scan_all()
            })
            .unwrap();
        assert_eq!(by_project.scanned_trajectories, 1);

        let no_match = store
            .scan_steps(&StepScanFilter {
                session_id: Some("nope".into()),
                ..scan_all()
            })
            .unwrap();
        assert!(no_match.hits.is_empty());
        assert_eq!(no_match.scanned_trajectories, 0);
    }

    #[test]
    fn test_scan_steps_truncates_cjk_preview_on_char_boundary() {
        let store = TrajectoryStore::new_with_path(&tmp_db("scan-cjk")).unwrap();
        // Multi-byte content longer than the preview cap: byte slicing here
        // would split a codepoint and panic.
        let long = "\u{4fee}".repeat(STEP_MESSAGE_PREVIEW_CHARS + 50);
        let mut rec = sample_record();
        rec.atif_json = serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "agent": {"name": "qoder", "version": "1.0"},
            "steps": [{"step_id": 1, "source": "user", "message": long}]
        })
        .to_string();
        store.upsert_trajectory(&rec).unwrap();

        let out = store.scan_steps(&scan_all()).unwrap();
        let view = &out.hits[0].step;
        assert!(view.message_truncated);
        assert_eq!(
            view.message_preview.chars().count(),
            STEP_MESSAGE_PREVIEW_CHARS
        );

        // A short message is returned whole and not flagged.
        let mut short = sample_record();
        short.session_id = "s-short".into();
        short.file_path = "/root/.qoder/projects/x/short.jsonl".into();
        short.atif_json = serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "agent": {"name": "qoder", "version": "1.0"},
            "steps": [{"step_id": 1, "source": "user", "message": "\u{4fee}\u{590d} bug"}]
        })
        .to_string();
        store.upsert_trajectory(&short).unwrap();

        let out = store
            .scan_steps(&StepScanFilter {
                session_id: Some("s-short".into()),
                ..scan_all()
            })
            .unwrap();
        assert_eq!(out.hits[0].step.message_preview, "\u{4fee}\u{590d} bug");
        assert!(!out.hits[0].step.message_truncated);
    }

    #[test]
    fn test_scan_steps_view_exposes_derived_flags() {
        let store = TrajectoryStore::new_with_path(&tmp_db("scan-flags")).unwrap();
        seed_five_steps(&store);

        let out = store.scan_steps(&scan_all()).unwrap();
        let by_id: std::collections::HashMap<usize, &StepView> =
            out.hits.iter().map(|h| (h.step.step_id, &h.step)).collect();

        let agent_tool_step = by_id[&2];
        assert!(agent_tool_step.has_reasoning);
        assert!(!agent_tool_step.has_observation);
        assert_eq!(
            agent_tool_step.categories,
            vec![
                "agent_message".to_string(),
                "thinking".to_string(),
                "tool_call".to_string()
            ]
        );

        let result_step = by_id[&3];
        assert!(result_step.has_observation);
        assert!(result_step.tool_names.is_empty());
        // Tool results ride on a user-role turn, so the role label applies too.
        assert_eq!(
            result_step.categories,
            vec!["user_input".to_string(), "tool_result".to_string()]
        );
    }

    #[test]
    fn test_scan_steps_empty_store() {
        let store = TrajectoryStore::new_with_path(&tmp_db("scan-empty")).unwrap();
        let out = store.scan_steps(&scan_all()).unwrap();
        assert!(out.hits.is_empty());
        assert_eq!(out.scanned_trajectories, 0);
        assert!(!out.truncated);
    }
}
