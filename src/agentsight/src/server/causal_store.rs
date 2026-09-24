//! Durable storage for causal attribution cases.
//!
//! Without this, an attribution result lived only in [`super::causal`]'s
//! in-memory cache: capped at 50 entries and gone on restart. Every case is a
//! paid pipeline run, so losing it meant paying again to see the same answer,
//! and nothing could be audited after the fact.
//!
//! The case is stored as the exact JSON sent to the client, and restored by
//! replaying those bytes. That is deliberate: `CausalCase` derives `Serialize`
//! only, and threading `Deserialize` through its whole nested tree would be a
//! larger change than this store needs. Lossless by construction, and the day
//! a consumer wants typed reads is the day to add the derive.
//!
//! The in-memory cache stays, in front of this store. It is a genuine
//! optimisation for reopening a panel moments after running it; this store is
//! what makes the result survive the restart in between. A miss here is a
//! point read, not a pipeline run.
//!
//! `causal.db` lives in the private state directory beside `security.db`,
//! `enforcement.db` and `reuse.db`, opened through the same tightening path.

use std::path::{Path, PathBuf};

use agentsight_sqlite_lifecycle::{
    CheckpointOutcome, MaintenanceReport, MaintenanceStatus, SizeBasis, SizePolicy,
    checkpoint_truncate, enforce_size_policy, measure_database, retention_cutoff_ns,
};
use rusqlite::{Connection, params};

use crate::private_sqlite;

/// Current schema version, stored in `PRAGMA user_version`.
const SCHEMA_VERSION: i64 = 1;

/// Marker stored for `round_index == None`, meaning "the last round".
///
/// The in-memory cache keys rounds by `Option<usize>`; SQLite primary keys
/// cannot hold NULL, so the absent round takes a sentinel no real round can
/// collide with (round indices are zero-based and non-negative).
const LAST_ROUND: i64 = -1;

/// Errors this store can produce.
#[derive(Debug, thiserror::Error)]
pub enum CausalStoreError {
    /// A shared SQLite lifecycle operation failed.
    #[error(transparent)]
    Lifecycle(#[from] agentsight_sqlite_lifecycle::LifecycleError),
    /// A schema or query operation failed.
    #[error("causal store: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The private database could not be opened safely.
    #[error("opening causal.db failed: {0}")]
    Open(String),
    /// Another thread panicked while holding the store lock.
    #[error("another thread panicked while holding the store lock")]
    Poisoned,
    /// The system clock precedes the Unix epoch.
    #[error("system clock precedes the Unix epoch: {0}")]
    Clock(#[from] std::time::SystemTimeError),
    /// A nanosecond timestamp cannot be represented by SQLite's signed integer.
    #[error("nanosecond timestamp exceeds SQLite integer range")]
    TimestampOutOfRange,
    /// The connection does not refer to a file that lifecycle code can measure.
    #[error("causal database connection has no file path")]
    MissingDatabasePath,
    /// A stored round cannot be decoded as a supported cache key.
    #[error("stored round {0} is not a valid round or {LAST_ROUND}")]
    CorruptRound(i64),
    /// The database was written by a newer binary. Refusing is the only safe
    /// option: the rows may mean something this version does not understand.
    #[error("causal.db schema {found} is newer than supported {supported}")]
    SchemaTooNew {
        /// Schema version found in the database.
        found: i64,
        /// Highest schema version supported by this binary.
        supported: i64,
    },
    /// The migration chain does not contain the required next step.
    #[error("missing migration step from schema {0}")]
    MissingMigration(i64),
}

type Result<T> = std::result::Result<T, CausalStoreError>;

/// Retention and capacity limits for persisted causal cases.
#[derive(Debug, Clone, Copy, Default)]
pub struct CausalMaintenancePolicy {
    /// Maximum case age in days; zero disables age retention.
    pub retention_days: u64,
    /// Maximum database size in MiB; zero disables size maintenance.
    pub max_db_size_mb: u64,
}

/// Result of one causal-case maintenance pass.
#[derive(Debug, Clone, Copy)]
pub struct CausalMaintenanceReport {
    /// Cases removed because their last update preceded the retention cutoff.
    pub expired_cases: usize,
    /// Result of capacity enforcement after age retention.
    pub size: MaintenanceReport,
}

/// Key of one stored case.
///
/// Mirrors the in-memory cache key: which session (with its scope tag), which
/// round, and which complaint. A different complaint is a different question
/// and gets its own case; the same complaint re-run with `force` replaces the
/// stored answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaseKey<'a> {
    /// Scope-tagged session id, e.g. `conv:…` or `sess:…`.
    pub session_key: &'a str,
    /// `None` means the last round, stored as [`LAST_ROUND`].
    pub round: Option<usize>,
    /// User complaint whose attribution result is cached.
    pub complaint: &'a str,
}

/// Persistent store over `causal.db`.
pub struct CausalCaseStore {
    /// Behind a mutex because the store lives in the shared server state, which
    /// actix requires to be `Send + Sync`; a bare connection is neither.
    conn: std::sync::Mutex<Connection>,
}

impl CausalCaseStore {
    /// Opens `causal.db` in the private state directory, creating it if needed.
    ///
    /// # Errors
    /// Returns an error when the database cannot be opened or migrated.
    pub fn open_private(state_dir: impl AsRef<Path>) -> Result<Self> {
        let connection = private_sqlite::open_private_connection(
            state_dir.as_ref(),
            crate::config::CAUSAL_DB_NAME,
        )
        .map_err(|error| CausalStoreError::Open(error.to_string()))?;
        Self::from_connection(connection)
    }

    /// Opens from an existing connection; the seam tests use.
    fn from_connection(mut conn: Connection) -> Result<Self> {
        ensure_schema(&mut conn)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// The stored case JSON for this key, if one was ever persisted.
    ///
    /// # Errors
    /// Returns an error on SQL failure or when the connection lock is poisoned.
    pub fn get(&self, key: CaseKey<'_>) -> Result<Option<String>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare_cached(
            "SELECT case_json FROM causal_cases
             WHERE session_key = ?1 AND round = ?2 AND complaint = ?3",
        )?;
        let row = stmt.query_row(
            params![key.session_key, encode_round(key.round), key.complaint],
            |row| row.get::<_, String>(0),
        );
        match row {
            Ok(json) => Ok(Some(json)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Persists a case, replacing any earlier answer for the same question.
    ///
    /// Called with the exact JSON the client received, so a later restore
    /// replays the identical response.
    ///
    /// # Errors
    /// Returns an error on clock, timestamp, SQL, or poisoned-lock failure.
    pub fn put(&self, key: CaseKey<'_>, case_json: &str) -> Result<()> {
        let now = now_ns()?;
        self.lock()?.execute(
            "INSERT INTO causal_cases (session_key, round, complaint, case_json,
                                        created_at_ns, updated_at_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(session_key, round, complaint) DO UPDATE SET
                 case_json = ?4, updated_at_ns = ?5",
            params![
                key.session_key,
                encode_round(key.round),
                key.complaint,
                case_json,
                now
            ],
        )?;
        Ok(())
    }

    /// Deletes cases last updated before `cutoff_ns`.
    ///
    /// # Errors
    /// Returns an error on SQL failure or when the connection lock is poisoned.
    pub fn prune_before(&self, cutoff_ns: i64) -> Result<usize> {
        let deleted = self.lock()?.execute(
            "DELETE FROM causal_cases WHERE updated_at_ns < ?1",
            params![cutoff_ns],
        )?;
        Ok(deleted)
    }

    /// Applies age retention and trims the oldest cache entries for capacity.
    ///
    /// Physical allocation triggers size maintenance, while logical size must
    /// converge to 90% of the configured limit. Capacity pruning orders equal
    /// timestamps by the complete primary key and always preserves the newest
    /// case. Freed pages remain reusable because maintenance never runs
    /// `VACUUM`.
    ///
    /// If age retention deletes rows but the following WAL checkpoint is busy,
    /// size pruning stops immediately so repeated deletes cannot run against a
    /// stale physical measurement.
    ///
    /// # Errors
    /// Returns an error on lifecycle, SQL, timestamp conversion, database path,
    /// or poisoned-lock failure.
    pub fn maintain(&self, policy: CausalMaintenancePolicy) -> Result<CausalMaintenanceReport> {
        let expired_cases = if policy.retention_days == 0 {
            0
        } else {
            let cutoff = retention_cutoff_ns(current_time_ns()?, policy.retention_days)?;
            self.prune_before(sqlite_timestamp(cutoff)?)?
        };

        let limit_bytes = policy.max_db_size_mb.saturating_mul(1024 * 1024);
        if (expired_cases > 0 || limit_bytes > 0) && self.checkpoint()? == CheckpointOutcome::Busy {
            let snapshot = self.size_snapshot()?;
            return Ok(CausalMaintenanceReport {
                expired_cases,
                size: MaintenanceReport {
                    status: MaintenanceStatus::CheckpointBusy,
                    rounds: 0,
                    deleted_rows: 0,
                    before: snapshot,
                    after: snapshot,
                },
            });
        }

        let size = enforce_size_policy::<CausalStoreError>(
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

        Ok(CausalMaintenanceReport {
            expired_cases,
            size,
        })
    }

    /// Rows currently stored; used by tests and worth having when someone asks
    /// how big the thing has grown.
    ///
    /// # Errors
    /// Returns an error on SQL failure or when the connection lock is poisoned.
    pub fn count(&self) -> Result<usize> {
        let n: i64 = self
            .lock()?
            .query_row("SELECT COUNT(*) FROM causal_cases", [], |row| row.get(0))?;
        Ok(usize::try_from(n.max(0)).unwrap_or(usize::MAX))
    }
}

impl CausalCaseStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| CausalStoreError::Poisoned)
    }

    fn delete_oldest_fraction(&self, fraction: f64) -> Result<usize> {
        let conn = self.lock()?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM causal_cases", [], |row| row.get(0))?;
        if count <= 1 || fraction <= 0.0 {
            return Ok(0);
        }
        let deletable = count - 1;
        let limit = ((deletable as f64 * fraction.clamp(0.0, 1.0)) as i64)
            .max(1)
            .min(deletable);
        let deleted = conn.execute(
            "DELETE FROM causal_cases
             WHERE (session_key, round, complaint) IN (
                 SELECT session_key, round, complaint FROM causal_cases
                 ORDER BY updated_at_ns ASC, session_key ASC, round ASC, complaint ASC
                 LIMIT ?1
             )",
            params![limit],
        )?;
        Ok(deleted)
    }

    fn size_snapshot(&self) -> Result<agentsight_sqlite_lifecycle::SizeSnapshot> {
        let conn = self.lock()?;
        let path = conn
            .path()
            .map(PathBuf::from)
            .ok_or(CausalStoreError::MissingDatabasePath)?;
        measure_database(&path, &conn).map_err(Into::into)
    }

    fn checkpoint(&self) -> Result<CheckpointOutcome> {
        let conn = self.lock()?;
        checkpoint_truncate(&conn).map_err(Into::into)
    }
}

fn encode_round(round: Option<usize>) -> i64 {
    match round {
        Some(index) => index as i64,
        None => LAST_ROUND,
    }
}

#[allow(dead_code)] // Symmetric with encode; a listing endpoint will need it.
fn decode_round(raw: i64) -> Result<Option<usize>> {
    if raw == LAST_ROUND {
        return Ok(None);
    }
    usize::try_from(raw)
        .map(Some)
        .map_err(|_| CausalStoreError::CorruptRound(raw))
}

fn now_ns() -> Result<i64> {
    sqlite_timestamp(current_time_ns()?)
}

fn current_time_ns() -> Result<u64> {
    let nanoseconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    u64::try_from(nanoseconds).map_err(|_| CausalStoreError::TimestampOutOfRange)
}

fn sqlite_timestamp(timestamp_ns: u64) -> Result<i64> {
    i64::try_from(timestamp_ns).map_err(|_| CausalStoreError::TimestampOutOfRange)
}

/// Brings `conn` to [`SCHEMA_VERSION`], one step at a time.
///
/// Each step runs in its own transaction, so an interrupted upgrade leaves the
/// database at either the previous version or the next, never in between.
fn ensure_schema(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap_or(0);
    if current > SCHEMA_VERSION {
        return Err(CausalStoreError::SchemaTooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }
    let mut at = current;
    while at < SCHEMA_VERSION {
        let tx = conn.transaction()?;
        match at {
            0 => migrate_0_to_1(&tx)?,
            n => return Err(CausalStoreError::MissingMigration(n)),
        }
        at += 1;
        tx.pragma_update(None, "user_version", at)?;
        tx.commit()?;
    }
    Ok(())
}

/// One row per (session, round, complaint): the case, as the client saw it.
fn migrate_0_to_1(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS causal_cases (
            session_key TEXT NOT NULL,
            round INTEGER NOT NULL,
            complaint TEXT NOT NULL,
            case_json TEXT NOT NULL,
            created_at_ns INTEGER NOT NULL,
            updated_at_ns INTEGER NOT NULL,
            PRIMARY KEY (session_key, round, complaint)
        );
        "#,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "causal_store/tests.rs"]
mod tests;
