//! Statistics recorder for tokenless.
//!
//! Provides SQLite-based storage for compression and rewriting metrics.

use crate::diff::DiffRecords;
use crate::record::{CompressionMode, OperationType, StatsRecord};
use crate::warn_stats;
use chrono::{DateTime, Local};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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

/// Automatic retention policy applied to `stats.db`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct RetentionPolicy {
    /// Maximum age in days before rows are deleted (`None` or `0` disables).
    pub row_retention_days: Option<u32>,
    /// Maximum age in days before stored before/after text payloads are
    /// cleared while keeping numeric metrics (`None` or `0` disables).
    pub text_retention_days: Option<u32>,
}

impl RetentionPolicy {
    /// Built-in default retention window in days for `stats` and `retrieve_events` rows.
    pub const DEFAULT_ROW_RETENTION_DAYS: u32 = 30;
    /// Built-in default retention window in days for stored before/after text payloads.
    pub const DEFAULT_TEXT_RETENTION_DAYS: u32 = 7;

    /// Returns the built-in default automatic retention policy (`30` days for
    /// rows, `7` days for stored text payloads).
    pub const fn default_enabled() -> Self {
        Self {
            row_retention_days: Some(Self::DEFAULT_ROW_RETENTION_DAYS),
            text_retention_days: Some(Self::DEFAULT_TEXT_RETENTION_DAYS),
        }
    }

    /// Returns `true` when at least one positive retention window is active.
    pub fn is_enabled(&self) -> bool {
        self.row_retention_days.is_some_and(|days| days > 0)
            || self.text_retention_days.is_some_and(|days| days > 0)
    }
}

/// Pre-write snapshot of `stats.db` used to detect whether a subsequent
/// external write (such as `tokenless rtk`) appended a new row in the same
/// [`StatsRecorder::clear`] generation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionSnapshot {
    /// Highest `stats.id` observed before the write (`0` when absent or empty).
    pub max_row_id: i64,
    /// Monotonic generation incremented by [`StatsRecorder::clear`] (`0` before
    /// any clear).
    pub clear_epoch: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MaintenanceState {
    last_retention_row_id: i64,
    clear_epoch: i64,
}

/// Counts of rows modified or deleted by one retention or pruning pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PruneReport {
    /// Rows deleted from `stats`.
    pub stats_rows_deleted: usize,
    /// Rows whose text payloads (`before_text`, `after_text`, `before_output`,
    /// `after_output`) were cleared to `NULL`.
    pub text_rows_trimmed: usize,
    /// Orphaned rows deleted from `compression_artifacts`.
    pub artifacts_deleted: usize,
    /// Expired rows deleted from `retrieve_events`.
    pub retrieve_events_deleted: usize,
}

impl PruneReport {
    /// Total rows touched across all tables.
    pub fn total_changes(&self) -> usize {
        self.stats_rows_deleted
            + self.text_rows_trimmed
            + self.artifacts_deleted
            + self.retrieve_events_deleted
    }

    /// Returns `true` when no rows were deleted or trimmed.
    pub fn is_empty(&self) -> bool {
        self.total_changes() == 0
    }
}

/// Result of WAL checkpointing and optional database compaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ReclaimReport {
    /// Whether `VACUUM` was requested.
    pub vacuum_attempted: bool,
    /// Whether `VACUUM` finished (`false` when skipped or deferred due to busy DB).
    pub vacuum_completed: bool,
    /// Whether non-blocking WAL truncation completed without encountering an active reader/writer.
    pub wal_checkpoint_completed: bool,
    /// Whether `TRUNCATE` encountered another active reader/writer (`busy != 0`).
    pub wal_busy: bool,
    /// Total WAL frames reported by the checkpoint.
    pub wal_log_frames: i64,
    /// Checkpointed WAL frames reported by the checkpoint.
    pub wal_checkpointed_frames: i64,
    /// Combined size in bytes of `stats.db`, `-wal`, and `-shm` before reclamation.
    pub bytes_before: u64,
    /// Combined size in bytes of `stats.db`, `-wal`, and `-shm` after reclamation.
    pub bytes_after: u64,
}

const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MIN_BUSY_TIMEOUT_MS: u64 = 10;
const MAX_BUSY_TIMEOUT_MS: u64 = 5_000;
const BUSY_TIMEOUT_ENV: &str = "TOKENLESS_STATS_BUSY_TIMEOUT_MS";
const NANOS_PER_DAY: i128 = 86_400_i128 * 1_000_000_000_i128;

fn parse_busy_timeout(raw: Option<&str>) -> Duration {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|ms| (MIN_BUSY_TIMEOUT_MS..=MAX_BUSY_TIMEOUT_MS).contains(ms))
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_BUSY_TIMEOUT)
}

fn configured_busy_timeout() -> Duration {
    parse_busy_timeout(std::env::var(BUSY_TIMEOUT_ENV).ok().as_deref())
}

fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut os = OsString::from(db_path.as_os_str());
    os.push(suffix);
    PathBuf::from(os)
}

fn db_bundle_size(db_path: &Path) -> u64 {
    [
        db_path.to_path_buf(),
        sidecar_path(db_path, "-wal"),
        sidecar_path(db_path, "-shm"),
    ]
    .iter()
    .filter_map(|path| std::fs::metadata(path).ok())
    .map(|meta| meta.len())
    .sum()
}

fn ensure_owner_only_permissions(db_path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(db_path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    #[cfg(not(unix))]
    let _ = db_path;
}

fn is_busy_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

struct BusyTimeoutGuard<'a> {
    conn: &'a mut Connection,
    restore: Duration,
}

impl<'a> BusyTimeoutGuard<'a> {
    fn set(conn: &'a mut Connection, temporary: Duration, restore: Duration) -> StatsResult<Self> {
        conn.busy_timeout(temporary)?;
        Ok(Self { conn, restore })
    }
}

impl Drop for BusyTimeoutGuard<'_> {
    fn drop(&mut self) {
        let _ = self.conn.busy_timeout(self.restore);
    }
}

#[derive(Clone, Copy)]
enum InstantTable {
    Stats,
    RetrieveEvents,
}

impl InstantTable {
    fn select_unkeyed_sql(self) -> &'static str {
        match self {
            Self::Stats => {
                "SELECT id, timestamp FROM stats WHERE timestamp_ns IS NULL ORDER BY id ASC"
            }
            Self::RetrieveEvents => {
                "SELECT id, timestamp FROM retrieve_events WHERE timestamp_ns IS NULL ORDER BY id ASC"
            }
        }
    }

    fn select_unkeyed_limited_sql(self) -> &'static str {
        match self {
            Self::Stats => {
                "SELECT id, timestamp FROM stats WHERE timestamp_ns IS NULL ORDER BY id ASC LIMIT ?1"
            }
            Self::RetrieveEvents => {
                "SELECT id, timestamp FROM retrieve_events WHERE timestamp_ns IS NULL ORDER BY id ASC LIMIT ?1"
            }
        }
    }

    fn update_key_sql(self) -> &'static str {
        match self {
            Self::Stats => "UPDATE stats SET timestamp_ns = ?1 WHERE id = ?2",
            Self::RetrieveEvents => "UPDATE retrieve_events SET timestamp_ns = ?1 WHERE id = ?2",
        }
    }
}

/// Statistics recorder that stores metrics in SQLite
pub struct StatsRecorder {
    db_path: PathBuf,
    conn: Mutex<Connection>,
    busy_timeout: Duration,
    retention: RetentionPolicy,
}

impl StatsRecorder {
    /// Sampled interval (by row-ID delta watermark in `stats_maintenance`)
    /// at which automatic retention cleanup runs after a stats insert.
    pub const AUTO_RETENTION_INTERVAL: i64 = 64;
    const AUTO_RETENTION_BATCH_LIMIT: usize = 1_000;
    const AUTO_RETENTION_BUSY_TIMEOUT: Duration = Duration::from_millis(100);

    /// Create a new recorder with database at `db_path`, retention disabled
    /// (`RetentionPolicy::default()`), and eager backfill on open for queries
    /// and explicit maintenance commands. Prefer [`Self::with_retention`] on
    /// compression write paths so large backlogs backfill incrementally.
    pub fn new<P: AsRef<Path>>(db_path: P) -> StatsResult<Self> {
        Self::open_with_options(
            db_path.as_ref(),
            RetentionPolicy::default(),
            true,
            configured_busy_timeout(),
        )
    }

    /// Create a recorder with database at `db_path` and an automatic sampled
    /// retention policy applied periodically on [`Self::record`].
    ///
    /// Unlike [`Self::new`], this defers missing `timestamp_ns` backfill on
    /// open so hook and runtime constructors stay bounded; missing instant keys
    /// are backfilled in bounded batches during sampled cleanup.
    pub fn with_retention<P: AsRef<Path>>(
        db_path: P,
        retention: RetentionPolicy,
    ) -> StatsResult<Self> {
        Self::open_with_options(
            db_path.as_ref(),
            retention,
            false,
            configured_busy_timeout(),
        )
    }

    fn open_with_options(
        db_path: &Path,
        retention: RetentionPolicy,
        backfill_on_open: bool,
        busy_timeout: Duration,
    ) -> StatsResult<Self> {
        let db_path = db_path.to_path_buf();
        let mut conn = Connection::open(&db_path)?;
        // Restrict the stats DB to owner-only — before_text/after_text
        // columns may contain tool output with sensitive content.
        ensure_owner_only_permissions(&db_path);

        Self::enable_wal(&conn, busy_timeout)?;
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS stats_maintenance (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                last_retention_row_id INTEGER NOT NULL DEFAULT 0,
                clear_epoch INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS stats (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                timestamp_ns INTEGER,
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
                timestamp_ns   INTEGER,
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
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tool_use_id ON stats(tool_use_id)",
            [],
        )?;

        // Acquire the writer lock before checking columns so concurrent hook
        // processes cannot both decide to add the same missing column.
        let migration = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // Schema migration: add columns introduced after the initial schema if
        // missing. Use PRAGMA table_info to check column existence before
        // ALTER TABLE instead of relying on error-message string matching,
        // which is fragile across SQLite versions and locales.
        for (col, col_type) in &[
            ("timestamp_ns", "INTEGER"),
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
            let exists: bool = migration
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('stats') WHERE name = ?",
                    [col],
                    |row| row.get::<_, i64>(0),
                )
                .map(|c| c > 0)?;
            if !exists {
                migration.execute(
                    &format!("ALTER TABLE stats ADD COLUMN {col} {col_type}"),
                    [],
                )?;
            }
        }

        for (column, col_type) in [
            ("timestamp_ns", "INTEGER"),
            ("agent_id", "TEXT"),
            ("session_id", "TEXT"),
            ("tool_use_id", "TEXT"),
        ] {
            let exists: bool = migration
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('retrieve_events') WHERE name = ?",
                    [column],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)?;
            if !exists {
                migration.execute(
                    &format!("ALTER TABLE retrieve_events ADD COLUMN {column} {col_type}"),
                    [],
                )?;
            }
        }
        let has_clear_epoch: bool = migration
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('stats_maintenance') WHERE name = 'clear_epoch'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)?;
        if !has_clear_epoch {
            migration.execute(
                "ALTER TABLE stats_maintenance ADD COLUMN clear_epoch INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        migration.commit()?;

        // Composite index so newest-first windows read the newest rows
        // through a reverse index scan instead of scanning and sorting the
        // whole persistent history: `id` is the tie-break the window and
        // report orderings share. Created before optional open-time or
        // incremental backfill so unkeyed timestamp probes use the index.
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_stats_instant ON stats(timestamp_ns, id)",
            [],
        )?;
        // Partial index covering only rows that still hold stored text
        // payloads, keeping text-trimming sweeps bounded to untrimmed rows.
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_stats_text_instant
                ON stats(timestamp_ns, id)
                WHERE before_text IS NOT NULL
                   OR after_text IS NOT NULL
                   OR before_output IS NOT NULL
                   OR after_output IS NOT NULL",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_retrieve_events_instant
                ON retrieve_events(timestamp_ns, id)",
            [],
        )?;

        if backfill_on_open {
            // Backfill the instant key for rows written before `timestamp_ns`
            // existed (or written by external writers without `timestamp_ns`).
            let backfill_tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            Self::backfill_missing_instant_ns(&backfill_tx, InstantTable::Stats, None)?;
            Self::backfill_missing_instant_ns(&backfill_tx, InstantTable::RetrieveEvents, None)?;
            backfill_tx.commit()?;
        }

        Ok(Self {
            db_path,
            conn: Mutex::new(conn),
            busy_timeout,
            retention,
        })
    }

    fn backfill_missing_instant_ns(
        tx: &Transaction<'_>,
        table: InstantTable,
        limit: Option<usize>,
    ) -> StatsResult<usize> {
        let unkeyed: Vec<(i64, String)> = match limit {
            Some(max) => {
                let mut stmt = tx.prepare(table.select_unkeyed_limited_sql())?;
                let rows = stmt.query_map([max as i64], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?;
                rows.collect::<Result<Vec<_>, _>>()?
            }
            None => {
                let mut stmt = tx.prepare(table.select_unkeyed_sql())?;
                let rows = stmt.query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?;
                rows.collect::<Result<Vec<_>, _>>()?
            }
        };
        let count = unkeyed.len();
        if count > 0 {
            let mut stmt = tx.prepare(table.update_key_sql())?;
            for (id, text) in &unkeyed {
                stmt.execute(rusqlite::params![Self::instant_key(text), id])?;
            }
        }
        Ok(count)
    }

    fn enable_wal(conn: &Connection, timeout: Duration) -> StatsResult<()> {
        let deadline = Instant::now() + timeout;
        loop {
            conn.busy_timeout(deadline.saturating_duration_since(Instant::now()))?;
            match conn.execute_batch("PRAGMA journal_mode=WAL;") {
                Ok(()) => break,
                Err(error)
                    if matches!(
                        &error,
                        rusqlite::Error::SqliteFailure(code, _)
                            if code.code == rusqlite::ErrorCode::DatabaseBusy
                    ) && Instant::now() < deadline =>
                {
                    // Concurrent first openers can collide while upgrading
                    // journal locks without invoking SQLite's busy handler.
                    std::thread::sleep(
                        Duration::from_millis(10)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
        conn.busy_timeout(timeout)?;
        Ok(())
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
            warn_stats(&format!(
                "[tokenless-stats] WARNING: mutex was poisoned by a previous panic; recovering: {poisoned}"
            ));
            self.conn.clear_poison();
            poisoned.into_inner()
        })
    }

    fn insert_stats_row(
        conn: &Connection,
        record: &StatsRecord,
        applied_operations: Option<&str>,
    ) -> StatsResult<i64> {
        conn.execute(
            "INSERT INTO stats (
                timestamp, timestamp_ns, operation, agent_id, source_pid, session_id, tool_use_id,
                before_chars, before_tokens, after_chars, after_tokens,
                before_text, after_text,
                before_output, after_output, mode,
                stash_writes, stash_errors, stash_size,
                content_type, content_origin, applied_operations, recoverability, tokenizer_id,
                unrecoverable_truncations
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                record.timestamp.to_rfc3339(),
                Self::record_instant_ns(record),
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

    /// Record a statistics entry
    pub fn record(&self, record: &StatsRecord) -> StatsResult<i64> {
        let mut conn = self.lock_conn();
        let applied_operations = record
            .applied_operations
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        let (row_id, retention_trigger) = if self.retention.is_enabled() {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row_id = Self::insert_stats_row(&tx, record, applied_operations.as_deref())?;
            // Capture `clear_epoch` in the same write transaction as the INSERT
            // so a concurrent `clear()` cannot interleave between row
            // allocation and epoch observation.
            let state = Self::read_maintenance_state(&tx)?;
            tx.commit()?;
            let trigger = Self::retention_is_due(row_id, state.last_retention_row_id)
                .then_some((row_id, state.clear_epoch));
            (row_id, trigger)
        } else {
            let row_id = Self::insert_stats_row(&conn, record, applied_operations.as_deref())?;
            (row_id, None)
        };

        if let Some((row_id, expected_epoch)) = retention_trigger
            && let Err(error) = self.run_sampled_retention_inner(&mut conn, row_id, expected_epoch)
        {
            warn_stats(&format!(
                "[tokenless-stats] WARNING: periodic retention cleanup failed: {error}"
            ));
        }

        Ok(row_id)
    }

    fn retention_is_due(current_row_id: i64, last_row_id: i64) -> bool {
        current_row_id > 0
            && current_row_id > last_row_id
            && current_row_id.saturating_sub(last_row_id) >= Self::AUTO_RETENTION_INTERVAL
    }

    fn read_maintenance_state(conn: &Connection) -> StatsResult<MaintenanceState> {
        match conn.query_row(
            "SELECT last_retention_row_id, clear_epoch FROM stats_maintenance WHERE singleton = 1",
            [],
            |row| {
                Ok(MaintenanceState {
                    last_retention_row_id: row.get(0)?,
                    clear_epoch: row.get(1)?,
                })
            },
        ) {
            Ok(state) => Ok(state),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(MaintenanceState::default()),
            Err(error) => Err(error.into()),
        }
    }

    fn read_maintenance_state_ro(conn: &Connection) -> StatsResult<MaintenanceState> {
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('stats_maintenance')")?;
        let mut has_table = false;
        let mut has_clear_epoch = false;
        for col in stmt.query_map([], |row| row.get::<_, String>(0))? {
            has_table = true;
            if col? == "clear_epoch" {
                has_clear_epoch = true;
            }
        }
        if !has_table {
            return Ok(MaintenanceState::default());
        }
        if has_clear_epoch {
            Self::read_maintenance_state(conn)
        } else {
            let last_retention_row_id = match conn.query_row(
                "SELECT last_retention_row_id FROM stats_maintenance WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            ) {
                Ok(row_id) => row_id,
                Err(rusqlite::Error::QueryReturnedNoRows) => 0,
                Err(error) => return Err(error.into()),
            };
            Ok(MaintenanceState {
                last_retention_row_id,
                clear_epoch: 0,
            })
        }
    }

    fn read_stats_sequence_or_max_id(tx: &Transaction<'_>) -> StatsResult<i64> {
        let has_seq_table: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_sequence')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|exists| exists != 0)?;
        if has_seq_table {
            match tx.query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'stats'",
                [],
                |row| row.get::<_, i64>(0),
            ) {
                Ok(seq) => return Ok(seq),
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(0),
                Err(error) => return Err(error.into()),
            }
        }
        let max_id: i64 = tx.query_row("SELECT COALESCE(MAX(id), 0) FROM stats", [], |row| {
            row.get(0)
        })?;
        Ok(max_id)
    }

    fn write_last_retention_row_id(
        tx: &Transaction<'_>,
        row_id: i64,
        expected_epoch: i64,
    ) -> StatsResult<()> {
        tx.execute(
            "INSERT INTO stats_maintenance (singleton, last_retention_row_id, clear_epoch)
             VALUES (1, ?1, ?2)
             ON CONFLICT(singleton) DO UPDATE SET
                 last_retention_row_id = MAX(
                     stats_maintenance.last_retention_row_id,
                     excluded.last_retention_row_id
                 )
             WHERE stats_maintenance.clear_epoch = excluded.clear_epoch",
            rusqlite::params![row_id, expected_epoch],
        )?;
        Ok(())
    }

    fn run_sampled_retention_inner(
        &self,
        conn: &mut Connection,
        row_id: i64,
        expected_epoch: i64,
    ) -> StatsResult<Option<PruneReport>> {
        let timeout = Self::AUTO_RETENTION_BUSY_TIMEOUT.min(self.busy_timeout);
        let guard = BusyTimeoutGuard::set(conn, timeout, self.busy_timeout)?;
        let Some(report) = Self::apply_retention_on_conn(
            guard.conn,
            Local::now(),
            self.retention,
            Some(Self::AUTO_RETENTION_BATCH_LIMIT),
            false,
            Some((row_id, expected_epoch)),
        )?
        else {
            return Ok(None);
        };
        if !report.is_empty() {
            let _ = Self::checkpoint_wal_conn(guard.conn, timeout);
        }
        Ok(Some(report))
    }

    /// Read a consistent pre-write snapshot (`max_row_id`, `clear_epoch`)
    /// without acquiring write locks or creating/migrating the database.
    ///
    /// Returns [`RetentionSnapshot::default()`] when `db_path` does not exist
    /// yet.
    pub fn read_retention_snapshot<P: AsRef<Path>>(db_path: P) -> StatsResult<RetentionSnapshot> {
        let db_path = db_path.as_ref();
        if !db_path.is_file() {
            return Ok(RetentionSnapshot::default());
        }
        let probe_timeout = Self::AUTO_RETENTION_BUSY_TIMEOUT.min(configured_busy_timeout());
        let mut probe = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        probe.busy_timeout(probe_timeout)?;
        // Hold one deferred read transaction so `max_row_id` and `clear_epoch`
        // come from the same SQLite snapshot even if `clear()` runs concurrently.
        let tx = probe.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let has_stats_table: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'stats')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|exists| exists != 0)?;
        let maintenance = Self::read_maintenance_state_ro(&tx)?;
        if !has_stats_table {
            return Ok(RetentionSnapshot {
                max_row_id: 0,
                clear_epoch: maintenance.clear_epoch,
            });
        }
        let max_row_id: i64 =
            tx.query_row("SELECT COALESCE(MAX(id), 0) FROM stats", [], |row| {
                row.get(0)
            })?;
        Ok(RetentionSnapshot {
            max_row_id,
            clear_epoch: maintenance.clear_epoch,
        })
    }

    /// Inspect `stats.db` read-only and run one bounded sampled retention pass
    /// only when the current process (`writer_pid`) actually appended a `stats`
    /// row past `before.max_row_id` within the same `before.clear_epoch`
    /// generation and that new row is at least
    /// [`Self::AUTO_RETENTION_INTERVAL`] rows past the persisted
    /// `stats_maintenance` watermark.
    ///
    /// This ensures read-only external subcommands (such as `tokenless rtk gain`)
    /// never prune or trim statistics on exit, and a late external writer that
    /// started before [`Self::clear`] cannot write a pre-clear watermark back
    /// into the freshly cleared database. Returns `Ok(None)` without acquiring
    /// write locks when `policy` is disabled, the database file does not exist
    /// yet, `clear_epoch` advanced since `before`, this process wrote no new
    /// row after `before.max_row_id`, or the watermark threshold has not been
    /// reached.
    pub fn maybe_run_periodic_retention<P: AsRef<Path>>(
        db_path: P,
        policy: RetentionPolicy,
        before: RetentionSnapshot,
        writer_pid: i64,
    ) -> StatsResult<Option<PruneReport>> {
        if !policy.is_enabled() {
            return Ok(None);
        }
        let db_path = db_path.as_ref();
        if !db_path.is_file() {
            return Ok(None);
        }
        let probe_timeout = Self::AUTO_RETENTION_BUSY_TIMEOUT.min(configured_busy_timeout());
        let mut probe = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        probe.busy_timeout(probe_timeout)?;
        let tx = probe.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let maintenance = Self::read_maintenance_state_ro(&tx)?;
        if maintenance.clear_epoch != before.clear_epoch {
            return Ok(None);
        }
        let has_source_pid: bool = tx
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('stats') WHERE name = 'source_pid'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)?;
        if !has_source_pid {
            return Ok(None);
        }
        let written_row_id: Option<i64> = tx.query_row(
            "SELECT MAX(id) FROM stats WHERE id > ?1 AND source_pid = ?2",
            rusqlite::params![before.max_row_id, writer_pid],
            |row| row.get(0),
        )?;
        let Some(written_row_id) = written_row_id else {
            return Ok(None);
        };
        if !Self::retention_is_due(written_row_id, maintenance.last_retention_row_id) {
            return Ok(None);
        }
        drop(tx);
        drop(probe);

        let recorder = Self::open_with_options(db_path, policy, false, probe_timeout)?;
        let mut conn = recorder.lock_conn();
        recorder.run_sampled_retention_inner(&mut conn, written_row_id, before.clear_epoch)
    }

    /// Instant key for stored rfc3339 timestamps that do not parse: sorts
    /// after every real instant, keeping corrupt rows last in ascending
    /// order (and first in newest-first windows), where the previous text
    /// comparison and `row_to_record`'s now() substitution already placed
    /// them.
    const UNPARSEABLE_INSTANT_NS: i64 = i64::MAX;

    /// Instant key for a record's typed timestamp: nanoseconds since the
    /// Unix epoch at the timestamp's full precision — the same value the
    /// Rust `(instant, id)` comparators in `sort_diff_records` and
    /// `build_chains` derive, so SQL and Rust orderings agree exactly.
    fn record_instant_ns(record: &StatsRecord) -> i64 {
        record
            .timestamp
            .timestamp_nanos_opt()
            .unwrap_or(Self::UNPARSEABLE_INSTANT_NS)
    }

    /// Instant key for a stored rfc3339 text: parsed at full chrono
    /// precision, falling back to the unparseable sentinel.
    fn instant_key(text: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(text)
            .ok()
            .and_then(|dt| dt.timestamp_nanos_opt())
            .unwrap_or(Self::UNPARSEABLE_INSTANT_NS)
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
            // Newest-first by the instant a timestamp denotes, not its text:
            // rfc3339 strings only sort chronologically while their UTC
            // offset is constant, so a session spanning an offset change
            // would otherwise let older-text records displace genuinely
            // newer ones from a limited window. `timestamp_ns` holds the
            // instant at nanosecond precision — matching the Rust
            // comparator exactly (unlike SQLite's date functions, which
            // only parse to milliseconds) — and `idx_stats_instant` serves
            // the window through a reverse index scan.
            "SELECT {} FROM stats \
             ORDER BY timestamp_ns DESC, id DESC LIMIT ?",
            Self::SELECT_COLS
        ))?;
        let rows = stmt.query_map([n as i64], Self::row_to_record)?;
        let records: Vec<_> = rows
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    static CORRUPT_LOGGED: AtomicBool = AtomicBool::new(false);
                    if !CORRUPT_LOGGED.swap(true, Ordering::Relaxed) {
                        warn_stats(&format!(
                            "[tokenless-stats] skipping corrupt row(s): {e} \
                             (further corrupt rows suppressed)"
                        ));
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
            // Same instant ordering as all_records: a limited window must
            // keep the newest records even when their stored rfc3339 text
            // spans more than one UTC offset.
            "SELECT {} FROM stats WHERE session_id = ? \
             ORDER BY timestamp_ns DESC, id DESC LIMIT ?",
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
        // Order by the instant a timestamp denotes, not by its text: an
        // rfc3339 string carries a UTC offset, so a session that spans an
        // offset change (DST fall-back, travel, a moved machine) persists
        // text that no longer sorts in chronological order. The link flags
        // this query computes describe adjacent rows under this ordering,
        // and `build_chains` re-sorts the same rows by their parsed instant
        // before consuming the flags — the two orderings must agree or real
        // compression chains split apart. `timestamp_ns` is that instant in
        // nanoseconds (derived at write time, or by the open-time backfill
        // for rows written before the column existed), so the window's
        // `(timestamp_ns, id)` order matches the Rust comparator exactly —
        // including sub-millisecond records that SQLite's date functions
        // would collapse to whole milliseconds. Unparseable timestamps keep
        // the unparseable sentinel and sort last, preserving the direction
        // of `row_to_record`'s now() substitution.
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
                    stats.id, stats.timestamp, stats.timestamp_ns,
                    stats.operation, stats.agent_id,
                    stats.source_pid, stats.session_id, stats.tool_use_id,
                    stats.before_chars, stats.before_tokens, stats.after_chars,
                    stats.after_tokens, stats.mode, stats.stash_writes,
                    stats.stash_errors, stats.stash_size,
                    LAG(stats.id) OVER (
                        PARTITION BY stats.tool_use_id
                        ORDER BY stats.timestamp_ns, stats.id
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
            ORDER BY ordered.timestamp_ns, ordered.id
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

    /// Clear all records, reset auto-increment, advance `clear_epoch`, and
    /// reclaim disk space best-effort.
    pub fn clear(&self) -> StatsResult<()> {
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "DELETE FROM stats;
             DELETE FROM compression_artifacts;
             DELETE FROM retrieve_events;
             DELETE FROM sqlite_sequence WHERE name IN ('stats', 'retrieve_events');
             INSERT INTO stats_maintenance (singleton, last_retention_row_id, clear_epoch)
             VALUES (1, 0, 1)
             ON CONFLICT(singleton) DO UPDATE SET
                 last_retention_row_id = 0,
                 clear_epoch = stats_maintenance.clear_epoch + 1;",
        )?;
        tx.commit()?;

        if let Err(error) =
            Self::reclaim_space_conn(&mut conn, &self.db_path, self.busy_timeout, true)
        {
            warn_stats(&format!(
                "[tokenless-stats] WARNING: post-clear compaction failed: {error}"
            ));
        }
        Ok(())
    }

    /// Delete `stats` and `retrieve_events` rows older than `days` days along
    /// with orphaned `compression_artifacts`, then run a non-blocking WAL
    /// checkpoint when rows changed. A `days` value of `0` is a no-op.
    pub fn prune_older_than(&self, days: u32) -> StatsResult<PruneReport> {
        self.apply_retention_at(
            Local::now(),
            RetentionPolicy {
                row_retention_days: Some(days),
                text_retention_days: None,
            },
            false,
        )
    }

    /// Clear full text payloads (`before_text`, `after_text`, `before_output`,
    /// `after_output`) on `stats` rows older than `days` days while keeping
    /// numeric counts and metadata intact, remove orphaned
    /// `compression_artifacts`, and run a non-blocking WAL checkpoint when rows
    /// changed. A `days` value of `0` is a no-op.
    pub fn trim_text_older_than(&self, days: u32) -> StatsResult<PruneReport> {
        self.apply_retention_at(
            Local::now(),
            RetentionPolicy {
                row_retention_days: None,
                text_retention_days: Some(days),
            },
            true,
        )
    }

    /// Apply a combined [`RetentionPolicy`] (row expiry and/or text trimming)
    /// across the full database, removing orphaned `compression_artifacts` and
    /// running a non-blocking WAL checkpoint when rows changed.
    pub fn apply_retention(&self, policy: RetentionPolicy) -> StatsResult<PruneReport> {
        self.apply_retention_at(Local::now(), policy, true)
    }

    /// Reclaim unused pages via best-effort `VACUUM` and non-blocking
    /// `PRAGMA wal_checkpoint(TRUNCATE)`, preserving owner-only `0o600`
    /// database permissions on Unix.
    pub fn compact(&self) -> StatsResult<ReclaimReport> {
        let mut conn = self.lock_conn();
        Self::reclaim_space_conn(&mut conn, &self.db_path, self.busy_timeout, true)
    }

    /// Run a best-effort non-blocking WAL checkpoint (`PASSIVE` followed by
    /// zero-timeout `TRUNCATE`).
    pub fn checkpoint_wal(&self) -> StatsResult<ReclaimReport> {
        let mut conn = self.lock_conn();
        Self::reclaim_space_conn(&mut conn, &self.db_path, self.busy_timeout, false)
    }

    fn apply_retention_at(
        &self,
        now: DateTime<Local>,
        policy: RetentionPolicy,
        prune_orphans_on_trim: bool,
    ) -> StatsResult<PruneReport> {
        let mut conn = self.lock_conn();
        let report = Self::apply_retention_on_conn(
            &mut conn,
            now,
            policy,
            None,
            prune_orphans_on_trim,
            None,
        )?
        .unwrap_or_default();
        if !report.is_empty() {
            let _ = Self::checkpoint_wal_conn(&mut conn, self.busy_timeout);
        }
        Ok(report)
    }

    fn cutoff_instant_ns(now: DateTime<Local>, days: u32) -> i64 {
        let now_ns = now.timestamp_nanos_opt().unwrap_or(i64::MAX);
        let cutoff = i128::from(now_ns) - i128::from(days) * NANOS_PER_DAY;
        i64::try_from(cutoff).unwrap_or_else(|_| {
            if cutoff < i128::from(i64::MIN) {
                i64::MIN
            } else {
                i64::MAX
            }
        })
    }

    fn apply_retention_on_conn(
        conn: &mut Connection,
        now: DateTime<Local>,
        policy: RetentionPolicy,
        batch_limit: Option<usize>,
        prune_orphans_on_trim: bool,
        watermark: Option<(i64, i64)>,
    ) -> StatsResult<Option<PruneReport>> {
        let row_days = policy.row_retention_days.filter(|&days| days > 0);
        let text_days = policy.text_retention_days.filter(|&days| days > 0);
        if row_days.is_none() && text_days.is_none() {
            return Ok(None);
        }

        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((current_id, expected_epoch)) = watermark {
            let state = Self::read_maintenance_state(&tx)?;
            let current_seq = Self::read_stats_sequence_or_max_id(&tx)?;
            if state.clear_epoch != expected_epoch
                || current_id > current_seq
                || !Self::retention_is_due(current_id, state.last_retention_row_id)
            {
                return Ok(None);
            }
        }
        let mut report = PruneReport::default();
        let mut backfilled_stats = false;

        if let Some(days) = row_days {
            Self::backfill_missing_instant_ns(&tx, InstantTable::Stats, batch_limit)?;
            Self::backfill_missing_instant_ns(&tx, InstantTable::RetrieveEvents, batch_limit)?;
            backfilled_stats = true;
            let cutoff = Self::cutoff_instant_ns(now, days);

            if let Some(limit) = batch_limit {
                let ids: Vec<i64> = {
                    let mut stmt = tx.prepare(
                        "SELECT id FROM stats
                         WHERE timestamp_ns < ?1
                         ORDER BY timestamp_ns ASC, id ASC
                         LIMIT ?2",
                    )?;
                    let rows =
                        stmt.query_map(rusqlite::params![cutoff, limit as i64], |row| row.get(0))?;
                    rows.collect::<Result<Vec<_>, _>>()?
                };
                if !ids.is_empty() {
                    let ids_json = serde_json::to_string(&ids)?;
                    report.artifacts_deleted += tx.execute(
                        "DELETE FROM compression_artifacts
                         WHERE stats_id IN (SELECT value FROM json_each(?1))",
                        [&ids_json],
                    )?;
                    report.stats_rows_deleted += tx.execute(
                        "DELETE FROM stats
                         WHERE id IN (SELECT value FROM json_each(?1))",
                        [&ids_json],
                    )?;
                }
                report.retrieve_events_deleted += tx.execute(
                    "DELETE FROM retrieve_events
                     WHERE id IN (
                         SELECT id FROM retrieve_events
                         WHERE timestamp_ns < ?1
                         ORDER BY timestamp_ns ASC, id ASC
                         LIMIT ?2
                     )",
                    rusqlite::params![cutoff, limit as i64],
                )?;
            } else {
                report.stats_rows_deleted +=
                    tx.execute("DELETE FROM stats WHERE timestamp_ns < ?1", [cutoff])?;
                report.artifacts_deleted += tx.execute(
                    "DELETE FROM compression_artifacts
                     WHERE NOT EXISTS (
                         SELECT 1 FROM stats WHERE stats.id = compression_artifacts.stats_id
                     )",
                    [],
                )?;
                report.retrieve_events_deleted += tx.execute(
                    "DELETE FROM retrieve_events WHERE timestamp_ns < ?1",
                    [cutoff],
                )?;
            }
        }

        if let Some(days) = text_days {
            if !backfilled_stats {
                Self::backfill_missing_instant_ns(&tx, InstantTable::Stats, batch_limit)?;
            }
            let cutoff = Self::cutoff_instant_ns(now, days);

            report.text_rows_trimmed += if let Some(limit) = batch_limit {
                tx.execute(
                    "UPDATE stats
                     SET before_text = NULL,
                         after_text = NULL,
                         before_output = NULL,
                         after_output = NULL
                     WHERE id IN (
                         SELECT id FROM stats
                         WHERE timestamp_ns < ?1
                           AND (before_text IS NOT NULL
                             OR after_text IS NOT NULL
                             OR before_output IS NOT NULL
                             OR after_output IS NOT NULL)
                         ORDER BY timestamp_ns ASC, id ASC
                         LIMIT ?2
                     )",
                    rusqlite::params![cutoff, limit as i64],
                )?
            } else {
                tx.execute(
                    "UPDATE stats
                     SET before_text = NULL,
                         after_text = NULL,
                         before_output = NULL,
                         after_output = NULL
                     WHERE timestamp_ns < ?1
                       AND (before_text IS NOT NULL
                         OR after_text IS NOT NULL
                         OR before_output IS NOT NULL
                         OR after_output IS NOT NULL)",
                    [cutoff],
                )?
            };

            if prune_orphans_on_trim && row_days.is_none() {
                report.artifacts_deleted += tx.execute(
                    "DELETE FROM compression_artifacts
                     WHERE NOT EXISTS (
                         SELECT 1 FROM stats WHERE stats.id = compression_artifacts.stats_id
                     )",
                    [],
                )?;
            }
        }

        if let Some((current_id, expected_epoch)) = watermark {
            Self::write_last_retention_row_id(&tx, current_id, expected_epoch)?;
        }

        tx.commit()?;
        Ok(Some(report))
    }

    fn checkpoint_wal_conn(
        conn: &mut Connection,
        restore_timeout: Duration,
    ) -> StatsResult<(bool, i64, i64)> {
        let (_passive_busy, passive_log, passive_ckpt): (i64, i64, i64) =
            match conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            }) {
                Ok(res) => res,
                Err(error) if is_busy_error(&error) => return Ok((true, 0, 0)),
                Err(error) => return Err(error.into()),
            };

        let guard = BusyTimeoutGuard::set(conn, Duration::ZERO, restore_timeout)?;
        match guard
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            }) {
            Ok((busy, log_frames, ckpt_frames)) => Ok((busy != 0, log_frames, ckpt_frames)),
            Err(error) if is_busy_error(&error) => Ok((true, passive_log, passive_ckpt)),
            Err(error) => Err(error.into()),
        }
    }

    fn reclaim_space_conn(
        conn: &mut Connection,
        db_path: &Path,
        restore_timeout: Duration,
        run_vacuum: bool,
    ) -> StatsResult<ReclaimReport> {
        let bytes_before = db_bundle_size(db_path);
        let mut vacuum_completed = false;

        if run_vacuum {
            let _ = conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            });
            let reclaim_timeout = Self::AUTO_RETENTION_BUSY_TIMEOUT.min(restore_timeout);
            let guard = BusyTimeoutGuard::set(conn, reclaim_timeout, restore_timeout)?;
            match guard.conn.execute_batch("VACUUM;") {
                Ok(()) => vacuum_completed = true,
                Err(error) if is_busy_error(&error) => {
                    warn_stats(&format!(
                        "[tokenless-stats] WARNING: stats database busy during VACUUM; skipping compaction: {error}"
                    ));
                }
                Err(error) => return Err(error.into()),
            }
        }

        let (wal_busy, wal_log_frames, wal_checkpointed_frames) =
            Self::checkpoint_wal_conn(conn, restore_timeout)?;
        ensure_owner_only_permissions(db_path);
        let bytes_after = db_bundle_size(db_path);

        Ok(ReclaimReport {
            vacuum_attempted: run_vacuum,
            vacuum_completed,
            wal_checkpoint_completed: !wal_busy,
            wal_busy,
            wal_log_frames,
            wal_checkpointed_frames,
            bytes_before,
            bytes_after,
        })
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
        let now = chrono::Local::now();
        let now_ns = now
            .timestamp_nanos_opt()
            .unwrap_or(Self::UNPARSEABLE_INSTANT_NS);
        conn.execute(
            "INSERT INTO retrieve_events (
                timestamp, timestamp_ns, hash, outcome, source, payload_tokens, tokenizer_id,
                agent_id, session_id, tool_use_id
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                now.to_rfc3339(),
                now_ns,
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
                    warn_stats(&format!(
                        "[tokenless-stats] corrupt timestamp, using current time: {e}"
                    ));
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
    /// UTF-8 bytes removed across all recorded operations (before minus after;
    /// the production recording paths pass `str::len()` byte lengths).
    pub fn chars_saved(&self) -> usize {
        self.total_before_chars
            .saturating_sub(self.total_after_chars)
    }

    /// Tokens saved across all recorded operations (before minus after).
    pub fn tokens_saved(&self) -> usize {
        self.total_before_tokens
            .saturating_sub(self.total_after_tokens)
    }

    /// Byte savings as a percentage of total before-chars (UTF-8 bytes); 0.0 when no data.
    pub fn chars_percent(&self) -> f64 {
        if self.total_before_chars > 0 {
            (self.chars_saved() as f64 / self.total_before_chars as f64) * 100.0
        } else {
            0.0
        }
    }

    /// Token savings as a percentage of total before-tokens; 0.0 when no data.
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
mod tests;
