//! Statistics recorder for tokenless.
//!
//! Provides SQLite-based storage for compression and rewriting metrics.

use crate::diff::DiffRecords;
use crate::record::{CompressionMode, OperationType, StatsRecord};
use chrono::DateTime;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Result type for stats operations
pub type StatsResult<T> = Result<T, StatsError>;

/// Error types for stats operations
#[derive(Debug, thiserror::Error)]
pub enum StatsError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Statistics recorder that stores metrics in SQLite
pub struct StatsRecorder {
    conn: Mutex<Connection>,
}

impl StatsRecorder {
    /// Create a new recorder with database at the given path
    pub fn new<P: AsRef<Path>>(db_path: P) -> StatsResult<Self> {
        let conn = Connection::open(&db_path)?;
        // Restrict the stats DB to owner-only — before_text/after_text
        // columns may contain tool output with sensitive content.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(db_path.as_ref(), std::fs::Permissions::from_mode(0o600)).ok();
        }

        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA busy_timeout=5000;
            PRAGMA synchronous=NORMAL;
        ",
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS stats (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                operation TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                source_pid INTEGER,
                session_id TEXT,
                tool_use_id TEXT,
                before_chars INTEGER NOT NULL,
                before_tokens INTEGER NOT NULL,
                after_chars INTEGER NOT NULL,
                after_tokens INTEGER NOT NULL,
                before_text TEXT,
                after_text TEXT,
                before_output TEXT,
                after_output TEXT,
                mode TEXT,
                stash_writes INTEGER,
                stash_errors INTEGER,
                stash_size INTEGER,
                content_type TEXT,
                content_origin TEXT,
                seam TEXT,
                compressor_chain TEXT,
                applied_operations TEXT,
                recoverability TEXT,
                tokenizer_id TEXT,
                unrecoverable_truncations INTEGER
            )",
            [],
        )?;

        // Stash ownership, normalized (roadmap §4.6): one row per hash kept
        // in an applied, emitted result. Rows are inserted only after the
        // final acceptance verdict, so candidate rollback never has pending
        // artifact rows to remove.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS compression_artifacts (
                stats_id       INTEGER NOT NULL,
                hash           TEXT    NOT NULL,
                compressor_id  TEXT    NOT NULL,
                emitted        INTEGER NOT NULL,
                PRIMARY KEY (stats_id, hash)
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_compression_artifacts_hash
                ON compression_artifacts(hash)",
            [],
        )?;

        // Retrieve operations, recorded locally so attribution needs no
        // cross-database join (roadmap §4.6).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS retrieve_events (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp      TEXT    NOT NULL,
                hash           TEXT    NOT NULL,
                outcome        TEXT    NOT NULL,
                source         TEXT    NOT NULL,
                payload_tokens INTEGER,
                tokenizer_id   TEXT,
                agent_id       TEXT,
                session_id     TEXT,
                tool_use_id    TEXT
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_retrieve_events_hash
                ON retrieve_events(hash)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_timestamp ON stats(timestamp)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_operation ON stats(operation)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_agent_id ON stats(agent_id)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_id ON stats(session_id)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_tool ON stats(session_id, tool_use_id)",
            [],
        )?;

        // Schema migration: add columns introduced after the initial schema if
        // missing. Use PRAGMA table_info to check column existence before
        // ALTER TABLE instead of relying on error-message string matching,
        // which is fragile across SQLite versions and locales.
        for (col, col_type) in &[
            ("before_output", "TEXT"),
            ("after_output", "TEXT"),
            ("mode", "TEXT"),
            ("stash_writes", "INTEGER"),
            ("stash_errors", "INTEGER"),
            ("stash_size", "INTEGER"),
            ("content_type", "TEXT"),
            ("content_origin", "TEXT"),
            ("seam", "TEXT"),
            ("compressor_chain", "TEXT"),
            ("applied_operations", "TEXT"),
            ("recoverability", "TEXT"),
            ("tokenizer_id", "TEXT"),
            ("unrecoverable_truncations", "INTEGER"),
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('stats') WHERE name = ?",
                    [col],
                    |row| row.get::<_, i64>(0),
                )
                .map(|c| c > 0)
                .unwrap_or(false);
            if !exists {
                conn.execute(
                    &format!("ALTER TABLE stats ADD COLUMN {col} {col_type}"),
                    [],
                )?;
            }
        }

        for column in ["agent_id", "session_id", "tool_use_id"] {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('retrieve_events') WHERE name = ?",
                    [column],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)
                .unwrap_or(false);
            if !exists {
                conn.execute(
                    &format!("ALTER TABLE retrieve_events ADD COLUMN {column} TEXT"),
                    [],
                )?;
            }
        }

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Acquire the connection guard, recovering from poison rather than failing.
    ///
    /// A poisoned mutex means a previous holder panicked while holding the
    /// lock. For our single-statement workload (no multi-step transactions),
    /// the SQLite connection itself remains usable — so we clear the poison
    /// and reuse the underlying guard rather than dropping the call. This
    /// keeps stats recording fail-soft after a transient panic instead of
    /// permanently breaking every subsequent query.
    fn lock_conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "[tokenless-stats] WARNING: mutex was poisoned by a previous panic; recovering: {poisoned}"
            );
            self.conn.clear_poison();
            poisoned.into_inner()
        })
    }

    /// Record a statistics entry
    pub fn record(&self, record: &StatsRecord) -> StatsResult<i64> {
        let conn = self.lock_conn();
        let applied_operations = record
            .applied_operations
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        conn.execute(
            "INSERT INTO stats (
                timestamp, operation, agent_id, source_pid, session_id, tool_use_id,
                before_chars, before_tokens, after_chars, after_tokens,
                before_text, after_text,
                before_output, after_output, mode,
                stash_writes, stash_errors, stash_size,
                content_type, content_origin, applied_operations, recoverability, tokenizer_id,
                unrecoverable_truncations
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                record.timestamp.to_rfc3339(),
                record.operation.as_str(),
                record.agent_id,
                record.source_pid,
                record.session_id,
                record.tool_use_id,
                record.before_chars,
                record.before_tokens,
                record.after_chars,
                record.after_tokens,
                record.before_text,
                record.after_text,
                record.before_output,
                record.after_output,
                record.mode.as_str(),
                record.stash_writes,
                record.stash_errors,
                record.stash_size,
                record.content_type,
                record.content_origin,
                applied_operations,
                record.recoverability,
                record.tokenizer_id,
                record.unrecoverable_truncations,
            ],
        )?;

        Ok(conn.last_insert_rowid())
    }

    /// Default limit when no limit is specified — caps memory usage
    /// from unbounded loads while remaining generous for practical use.
    const DEFAULT_LIMIT: usize = 10_000;

    /// Canonical column list for `stats` SELECTs. Defined once at impl level
    /// so `row_to_record`'s positional `row.get(N)` indices stay in sync with
    /// the SELECT order — adding a column here is the only place to update.
    /// `concat!` keeps the source multi-line (one group per row_to_record
    /// index span) without leaking indentation padding into the SQL string.
    const SELECT_COLS: &str = concat!(
        "id, timestamp, operation, agent_id, source_pid, ",
        "session_id, tool_use_id, before_chars, before_tokens, ",
        "after_chars, after_tokens, before_text, after_text, ",
        "before_output, after_output, mode, stash_writes, ",
        "stash_errors, stash_size, content_type, content_origin, applied_operations, ",
        "recoverability, tokenizer_id, unrecoverable_truncations"
    );

    /// Query all records, newest first, with optional limit
    pub fn all_records(&self, limit: Option<usize>) -> StatsResult<Vec<StatsRecord>> {
        let conn = self.lock_conn();

        let n = limit.unwrap_or(Self::DEFAULT_LIMIT);
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM stats ORDER BY timestamp DESC LIMIT ?",
            Self::SELECT_COLS
        ))?;
        let rows = stmt.query_map([n as i64], Self::row_to_record)?;
        let records: Vec<_> = rows
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    static CORRUPT_LOGGED: AtomicBool = AtomicBool::new(false);
                    if !CORRUPT_LOGGED.swap(true, Ordering::Relaxed) {
                        eprintln!(
                            "[tokenless-stats] skipping corrupt row(s): {e} \
                             (further corrupt rows suppressed)"
                        );
                    }
                    None
                }
            })
            .collect();

        Ok(records)
    }

    /// Get a single record by database ID
    pub fn record_by_id(&self, id: i64) -> StatsResult<Option<StatsRecord>> {
        let conn = self.lock_conn();

        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM stats WHERE id = ?",
            Self::SELECT_COLS
        ))?;

        let mut rows = stmt.query_map([id], Self::row_to_record)?;

        if let Some(row) = rows.next() {
            Ok(Some(row?))
        } else {
            Ok(None)
        }
    }

    /// Query all records for a given session, newest first, with optional limit.
    pub fn records_by_session(
        &self,
        session_id: &str,
        limit: Option<usize>,
    ) -> StatsResult<Vec<StatsRecord>> {
        let conn = self.lock_conn();

        let n = limit.unwrap_or(Self::DEFAULT_LIMIT);
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM stats WHERE session_id = ? ORDER BY timestamp DESC LIMIT ?",
            Self::SELECT_COLS
        ))?;
        let rows = stmt.query_map(rusqlite::params![session_id, n as i64], Self::row_to_record)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Queries the newest records used to build a session or tool-use diff.
    ///
    /// Results are returned oldest first. Session overviews load only metrics
    /// and metadata; SQLite compares adjacent payloads to preserve exact chain
    /// linking without materializing every payload in the Rust process.
    /// Explicit tool-use queries retain content for their detailed diff.
    ///
    /// # Errors
    ///
    /// Returns an error when SQLite cannot prepare or execute the query, or a
    /// stored row cannot be converted into a statistics record.
    pub fn records_for_diff(
        &self,
        session_id: &str,
        tool_use_id: Option<&str>,
    ) -> StatsResult<DiffRecords> {
        match tool_use_id {
            Some(tool_use_id) => self.records_for_tool_diff(session_id, tool_use_id),
            None => self.records_for_session_diff(session_id),
        }
    }

    fn records_for_tool_diff(
        &self,
        session_id: &str,
        tool_use_id: &str,
    ) -> StatsResult<DiffRecords> {
        let conn = self.lock_conn();
        let newest_first = format!(
            "SELECT {} FROM stats
             WHERE session_id = ? AND tool_use_id = ?
             ORDER BY id DESC LIMIT ?",
            Self::SELECT_COLS
        );
        let mut stmt = conn.prepare(&newest_first)?;
        let rows = stmt.query_map(
            rusqlite::params![session_id, tool_use_id, Self::DEFAULT_LIMIT as i64],
            Self::row_to_record,
        )?;
        let mut records = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(StatsError::from)?;
        Self::sort_diff_records(&mut records);
        Ok(DiffRecords::from_records(records))
    }

    fn records_for_session_diff(&self, session_id: &str) -> StatsResult<DiffRecords> {
        let conn = self.lock_conn();
        let query = "
            WITH newest AS (
                SELECT id
                FROM stats
                WHERE session_id = ?
                ORDER BY id DESC
                LIMIT ?
            ),
            ordered AS (
                SELECT
                    stats.id, stats.timestamp, stats.operation, stats.agent_id,
                    stats.source_pid, stats.session_id, stats.tool_use_id,
                    stats.before_chars, stats.before_tokens, stats.after_chars,
                    stats.after_tokens, stats.mode, stats.stash_writes,
                    stats.stash_errors, stats.stash_size,
                    LAG(stats.id) OVER (
                        PARTITION BY stats.tool_use_id
                        ORDER BY stats.timestamp, stats.id
                    ) AS previous_id
                FROM stats
                INNER JOIN newest ON newest.id = stats.id
            )
            SELECT
                ordered.id, ordered.timestamp, ordered.operation, ordered.agent_id,
                ordered.source_pid, ordered.session_id, ordered.tool_use_id,
                ordered.before_chars, ordered.before_tokens, ordered.after_chars,
                ordered.after_tokens,
                NULL AS before_text, NULL AS after_text,
                NULL AS before_output, NULL AS after_output,
                ordered.mode, ordered.stash_writes, ordered.stash_errors,
                ordered.stash_size,
                NULL AS content_type, NULL AS content_origin,
                NULL AS applied_operations, NULL AS recoverability,
                NULL AS tokenizer_id, NULL AS unrecoverable_truncations,
                CASE
                    WHEN ordered.tool_use_id IS NOT NULL
                        AND COALESCE(previous.mode, 'active')
                            NOT IN ('dry-run', 'dryrun')
                        AND COALESCE(current.mode, 'active')
                            NOT IN ('dry-run', 'dryrun')
                        AND (
                            CASE
                                WHEN previous.operation = 'rewrite-command'
                                    AND previous.before_output IS NOT NULL
                                    AND previous.after_output IS NOT NULL
                                THEN previous.after_output
                                WHEN previous.before_text IS NOT NULL
                                    AND previous.after_text IS NOT NULL
                                THEN previous.after_text
                            END
                        ) = (
                            CASE
                                WHEN current.operation = 'rewrite-command'
                                    AND current.before_output IS NOT NULL
                                    AND current.after_output IS NOT NULL
                                THEN current.before_output
                                WHEN current.before_text IS NOT NULL
                                    AND current.after_text IS NOT NULL
                                THEN current.before_text
                            END
                        )
                    THEN 1
                    ELSE 0
                END AS linked_to_previous
            FROM ordered
            INNER JOIN stats AS current ON current.id = ordered.id
            LEFT JOIN stats AS previous ON previous.id = ordered.previous_id
            ORDER BY ordered.timestamp, ordered.id
        ";
        let mut stmt = conn.prepare(query)?;
        let rows = stmt.query_map(
            rusqlite::params![session_id, Self::DEFAULT_LIMIT as i64],
            |row| {
                let record = Self::row_to_record(row)?;
                // linked_to_previous is the last column, right after the
                // SELECT_COLS span. Asking the row for its width keeps this
                // index from drifting the next time a column is added.
                let last = row.as_ref().column_count() - 1;
                let linked_to_previous = row.get::<_, i64>(last)? != 0;
                Ok((record, linked_to_previous))
            },
        )?;
        let mut records = Vec::new();
        let mut linked_to_previous = HashSet::new();
        for row in rows {
            let (record, is_linked) = row?;
            if is_linked {
                linked_to_previous.insert(record.id);
            }
            records.push(record);
        }
        Ok(DiffRecords::from_prelinked(records, linked_to_previous))
    }

    fn sort_diff_records(records: &mut [StatsRecord]) {
        records.sort_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then_with(|| left.id.cmp(&right.id))
        });
    }

    /// Get record count
    pub fn count(&self) -> StatsResult<usize> {
        let conn = self.lock_conn();

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM stats", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Clear all records and reset auto-increment
    pub fn clear(&self) -> StatsResult<()> {
        let conn = self.lock_conn();

        conn.execute_batch(
            "DELETE FROM stats;
             DELETE FROM compression_artifacts;
             DELETE FROM retrieve_events;
             DELETE FROM sqlite_sequence WHERE name IN ('stats', 'retrieve_events');",
        )?;
        Ok(())
    }

    /// Record the stash artifacts of one applied, emitted compression
    /// (roadmap §4.6). `stats_id` is the row id `record` returned;
    /// `compressor_id` is the stable id of the compressor that created the
    /// keys — today always the chain head, the single stash writer of a
    /// pipeline run. Emitted is always true: callers insert only keys whose
    /// markers reached the final output, after the acceptance verdict, so
    /// pending artifact rows never exist and candidate rollback has nothing
    /// to remove here.
    pub fn record_artifacts(
        &self,
        stats_id: i64,
        compressor_id: &str,
        hashes: &[String],
    ) -> StatsResult<()> {
        let conn = self.lock_conn();
        let mut stmt = conn.prepare(
            "INSERT INTO compression_artifacts (stats_id, hash, compressor_id, emitted)
             VALUES (?, ?, ?, 1)",
        )?;
        for hash in hashes {
            stmt.execute(rusqlite::params![stats_id, hash, compressor_id])?;
        }
        Ok(())
    }

    /// Record one retrieve operation (roadmap §4.6). `outcome` is `hit`,
    /// `miss`, or `error`; `source` names the trusted frontend.
    // Keep the persisted event fields explicit at this storage boundary; a
    // second DTO would only duplicate the SQLite row without adding policy.
    #[allow(clippy::too_many_arguments)]
    pub fn record_retrieve_event(
        &self,
        hash: &str,
        outcome: &str,
        source: &str,
        payload_tokens: Option<i64>,
        tokenizer_id: Option<&str>,
        agent_id: Option<&str>,
        session_id: Option<&str>,
        tool_use_id: Option<&str>,
    ) -> StatsResult<i64> {
        let conn = self.lock_conn();
        conn.execute(
            "INSERT INTO retrieve_events (
                timestamp, hash, outcome, source, payload_tokens, tokenizer_id,
                agent_id, session_id, tool_use_id
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                chrono::Local::now().to_rfc3339(),
                hash,
                outcome,
                source,
                payload_tokens,
                tokenizer_id,
                agent_id,
                session_id,
                tool_use_id,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Whole-table retrieve aggregates for the summary's attribution block.
    pub fn retrieve_totals(&self) -> StatsResult<RetrieveTotals> {
        let conn = self.lock_conn();
        let mut stmt = conn.prepare(
            "SELECT outcome, COUNT(*), COALESCE(SUM(payload_tokens), 0)
             FROM retrieve_events GROUP BY outcome",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut totals = RetrieveTotals::default();
        for row in rows {
            let (outcome, count, tokens) = row?;
            match outcome.as_str() {
                "hit" => {
                    totals.hits = count as u64;
                    totals.retrieved_tokens = tokens as u64;
                }
                "miss" => totals.misses = count as u64,
                "error" => totals.errors = count as u64,
                _ => {}
            }
        }
        Ok(totals)
    }

    /// Convert a database row to StatsRecord
    fn row_to_record(row: &rusqlite::Row<'_>) -> Result<StatsRecord, rusqlite::Error> {
        let agent_id: String = row.get(3)?;
        Ok(StatsRecord {
            id: row.get(0)?,
            timestamp: DateTime::parse_from_rfc3339(&row.get::<_, String>(1)?)
                .map(|dt| dt.with_timezone(&chrono::Local))
                .unwrap_or_else(|e| {
                    eprintln!("[tokenless-stats] corrupt timestamp, using current time: {e}");
                    chrono::Local::now()
                }),
            operation: OperationType::from_str(&row.get::<_, String>(2)?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("unknown operation type: {e}"),
                    )),
                )
            })?,
            agent_id,
            source_pid: row.get(4)?,
            session_id: row.get(5)?,
            tool_use_id: row.get(6)?,
            before_chars: row.get(7)?,
            before_tokens: row.get(8)?,
            after_chars: row.get(9)?,
            after_tokens: row.get(10)?,
            before_text: row.get(11)?,
            after_text: row.get(12)?,
            before_output: row.get(13)?,
            after_output: row.get(14)?,
            mode: CompressionMode::from_db(&row.get::<_, Option<String>>(15)?.unwrap_or_default()),
            stash_writes: row.get(16)?,
            stash_errors: row.get(17)?,
            stash_size: row.get(18)?,
            content_type: row.get(19)?,
            content_origin: row.get(20)?,
            applied_operations: row
                .get::<_, Option<String>>(21)?
                .map(|value| {
                    serde_json::from_str(&value).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            21,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
                })
                .transpose()?,
            recoverability: row.get(22)?,
            tokenizer_id: row.get(23)?,
            unrecoverable_truncations: row.get(24)?,
        })
    }
}

/// Whole-table aggregates over `retrieve_events` (roadmap §4.6 report).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct RetrieveTotals {
    pub hits: u64,
    pub misses: u64,
    pub errors: u64,
    /// Sum of `payload_tokens` over hits: tokens read back out of the stash.
    pub retrieved_tokens: u64,
}

/// Summary statistics
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct StatsSummary {
    #[serde(rename = "records")]
    pub total_records: usize,
    #[serde(rename = "before_chars")]
    pub total_before_chars: usize,
    #[serde(rename = "after_chars")]
    pub total_after_chars: usize,
    #[serde(rename = "before_tokens")]
    pub total_before_tokens: usize,
    #[serde(rename = "after_tokens")]
    pub total_after_tokens: usize,
}

impl StatsSummary {
    pub fn chars_saved(&self) -> usize {
        self.total_before_chars
            .saturating_sub(self.total_after_chars)
    }

    pub fn tokens_saved(&self) -> usize {
        self.total_before_tokens
            .saturating_sub(self.total_after_tokens)
    }

    pub fn chars_percent(&self) -> f64 {
        if self.total_before_chars > 0 {
            (self.chars_saved() as f64 / self.total_before_chars as f64) * 100.0
        } else {
            0.0
        }
    }

    pub fn tokens_percent(&self) -> f64 {
        if self.total_before_tokens > 0 {
            (self.tokens_saved() as f64 / self.total_before_tokens as f64) * 100.0
        } else {
            0.0
        }
    }

    /// Actual savings rate against total session consumption.
    ///
    /// This is the number users actually perceive: saved tokens as a
    /// percentage of the entire session's token spend (LLM input + output +
    /// tool responses), not just the tool-response portion that tokenless
    /// touches.
    ///
    /// Example: if tokenless saved 1.8M tokens and the session consumed
    /// 15M tokens total, the actual savings rate is 12.0%.
    pub fn actual_savings_percent(&self, session_total_tokens: usize) -> f64 {
        if session_total_tokens > 0 {
            (self.tokens_saved() as f64 / session_total_tokens as f64) * 100.0
        } else {
            0.0
        }
    }

    /// Build summary from a slice of records
    pub fn from_records(records: &[StatsRecord]) -> Self {
        Self::from_record_refs(records)
    }

    /// Like [`Self::from_records`], but over borrowed records — lets callers
    /// summarize a filtered view without cloning text-blob-carrying rows.
    pub fn from_record_refs<'a, I: IntoIterator<Item = &'a StatsRecord>>(records: I) -> Self {
        let mut summary = Self::default();

        for record in records {
            summary.total_records += 1;
            summary.total_before_chars += record.before_chars;
            summary.total_after_chars += record.after_chars;
            summary.total_before_tokens += record.before_tokens;
            summary.total_after_tokens += record.after_tokens;
        }

        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/recorder_tests.rs");
}
