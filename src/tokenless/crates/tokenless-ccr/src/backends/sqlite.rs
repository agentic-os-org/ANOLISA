//! SQLite-backed stash store (default production backend).
//!
//! Persists stashed payloads to a single file so state survives across the
//! short-lived processes that tokenless hooks fork+exec on every call. Uses
//! WAL mode and `BEGIN IMMEDIATE` for database-wide write serialization.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, ErrorCode, OpenFlags, Transaction, TransactionBehavior, ffi};

use crate::key::compute_key;
use crate::store::{MAX_GENERATION, StashError, StashStore, StashWrite};
use crate::warn_soft;

/// Default time-to-live for an entry: 1 hour. A retrieve window of an hour
/// comfortably covers a typical agent session's compress→retrieve round trip.
const DEFAULT_TTL_SECONDS: u64 = 60 * 60;

/// Default maximum number of live entries before FIFO eviction.
const DEFAULT_CAPACITY: usize = 10_000;

/// How long one connection waits for locks held by other processes, both
/// while opening the database and for every later statement.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Shared by initialization and `reopening_a_full_stash_uses_bounded_generation_query_work`;
/// keep both on this query so the regression test measures production SQL.
const MAX_GENERATION_QUERY: &str = "SELECT COALESCE(MAX(generation), 0) FROM stash";

pub struct SqliteStore {
    conn: Mutex<Connection>,
    ttl_seconds: u64,
    capacity: usize,
    read_only: bool,
}

impl SqliteStore {
    /// Open an existing stash for lookups without initializing or cleaning it.
    ///
    /// SQLite requires readable WAL and shared-memory files when the containing
    /// directory is not writable. Updated stash writers preserve these files
    /// on close. If a legacy writer removed them, reopen the stash with an
    /// updated writer before running a lookup from a read-only directory.
    /// SQLite can create the files itself when the directory is writable.
    pub fn open_read_only(path: &Path) -> Result<Self, StashError> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        Ok(Self {
            conn: Mutex::new(conn),
            ttl_seconds: DEFAULT_TTL_SECONDS,
            capacity: DEFAULT_CAPACITY,
            read_only: true,
        })
    }

    /// Open (or create) a stash database at `path` with default TTL and
    /// capacity. The file and its `-wal`/`-shm` sidecars are created on first
    /// write; the parent directory must already exist.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, StashError> {
        Self::with_limits(path, DEFAULT_TTL_SECONDS, DEFAULT_CAPACITY)
    }

    /// Open (or create) a stash database with a custom TTL and capacity.
    pub fn with_limits<P: AsRef<Path>>(
        path: P,
        ttl_seconds: u64,
        capacity: usize,
    ) -> Result<Self, StashError> {
        let path = path.as_ref();
        // Pre-create with owner-only access to avoid a permissive window before
        // SQLite opens the file. Reapply the mode for existing databases.
        #[cfg(unix)]
        {
            use std::fs::OpenOptions;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)
                .map_err(|error| {
                    StashError::Backend(format!(
                        "cannot create stash database '{}': {error}",
                        path.display(),
                    ))
                })?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|error| {
                    StashError::Backend(format!(
                        "cannot restrict stash database '{}': {error}",
                        path.display(),
                    ))
                })?;
        }
        let mut conn = Connection::open(path)?;
        enable_wal(&conn)?;
        // Read-only retrieval after the last writer closes needs both WAL sidecars.
        let mut persist_wal = 1;
        // SAFETY: The live connection and mutable integer are valid for this call.
        let result = unsafe {
            ffi::sqlite3_file_control(
                conn.handle(),
                c"main".as_ptr(),
                ffi::SQLITE_FCNTL_PERSIST_WAL,
                std::ptr::from_mut(&mut persist_wal).cast(),
            )
        };
        if result != ffi::SQLITE_OK {
            return Err(rusqlite::Error::SqliteFailure(ffi::Error::new(result), None).into());
        }
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "CREATE TABLE IF NOT EXISTS stash (
                hash TEXT PRIMARY KEY,
                payload TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                generation INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;
        let has_generation: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM pragma_table_info('stash') WHERE name = 'generation'
             )",
            [],
            |row| row.get(0),
        )?;
        if !has_generation {
            tx.execute(
                "ALTER TABLE stash ADD COLUMN generation INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_stash_expires_at ON stash(expires_at)",
            [],
        )?;
        // Writable opens reconcile the high-water mark. Keep this lookup off
        // the payload table, including on existing databases.
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_stash_generation ON stash(generation)",
            [],
        )?;
        tx.execute(
            "CREATE TABLE IF NOT EXISTS stash_metadata (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                last_generation INTEGER NOT NULL CHECK(last_generation >= 0)
            )",
            [],
        )?;
        let max_generation: i64 = tx.query_row(MAX_GENERATION_QUERY, [], |row| row.get(0))?;
        tx.execute(
            "INSERT INTO stash_metadata(singleton, last_generation)
             VALUES (1, ?)
             ON CONFLICT(singleton) DO UPDATE SET
                 last_generation = max(stash_metadata.last_generation, excluded.last_generation)",
            [max_generation],
        )?;
        tx.commit()?;
        Ok(Self {
            conn: Mutex::new(conn),
            ttl_seconds,
            capacity,
            read_only: false,
        })
    }

    /// Acquire the connection guard, recovering from poison rather than
    /// failing. A poisoned mutex means a prior holder panicked; for our
    /// single-statement workload the SQLite connection itself stays usable,
    /// so we clear the poison and reuse the underlying guard. This mirrors
    /// the fail-soft policy in `tokenless-stats::recorder::StatsRecorder`.
    fn lock_conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| {
            warn_soft(&format!(
                "[tokenless-ccr] WARNING: sqlite mutex poisoned by a previous panic; recovering: {poisoned}"
            ));
            self.conn.clear_poison();
            poisoned.into_inner()
        })
    }

    /// Evict oldest live entries once the live count exceeds `capacity`.
    /// The caller holds the `BEGIN IMMEDIATE` transaction used for the stash
    /// write, so no other connection can change ownership between counting
    /// and eviction.
    fn enforce_capacity(&self, tx: &Transaction<'_>, now: i64) -> Result<usize, StashError> {
        let live: i64 = tx.query_row(
            "SELECT COUNT(*) FROM stash WHERE expires_at >= ?",
            [now],
            |row| row.get(0),
        )?;
        let capacity = i64::try_from(self.capacity).unwrap_or(i64::MAX);
        let surplus = live.saturating_sub(capacity);
        if surplus <= 0 {
            return Ok(0);
        }
        let evicted = tx.execute(
            "DELETE FROM stash
             WHERE hash IN (
                 SELECT hash FROM stash
                 WHERE expires_at >= ?
                 ORDER BY expires_at ASC, generation ASC
                 LIMIT ?
             )",
            rusqlite::params![now, surplus],
        )?;
        Ok(evicted)
    }
}

impl SqliteStore {
    fn stash_inner(
        &self,
        payload: &str,
        #[cfg(test)] after_live_read: Option<&dyn Fn()>,
        #[cfg(not(test))] _after_live_read: Option<&dyn Fn()>,
    ) -> Result<StashWrite, StashError> {
        let key = compute_key(payload.as_bytes());
        let now = now_unix();
        let expires_at = now
            .checked_add(self.ttl_seconds)
            .and_then(|expires_at| i64::try_from(expires_at).ok())
            .ok_or_else(|| StashError::Backend("stash expiry overflow".to_string()))?;
        let mut conn = self.lock_conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Compression can write indefinitely without retrieving. Reclaim
        // expired payloads here as well, under the same write transaction.
        tx.execute("DELETE FROM stash WHERE expires_at < ?", [now as i64])?;
        let previous_generation: Option<u64> = match tx.query_row(
            "SELECT generation FROM stash WHERE hash = ? AND expires_at >= ?",
            rusqlite::params![key, now as i64],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(generation) => Some(generation as u64),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(StashError::from(e)),
        };
        let had_live = previous_generation.is_some();
        #[cfg(test)]
        if let Some(after_live_read) = after_live_read {
            after_live_read();
        }
        let generation: i64 = match tx.query_row(
            "UPDATE stash_metadata
             SET last_generation = last_generation + 1
             WHERE singleton = 1 AND last_generation < ?
             RETURNING last_generation",
            [MAX_GENERATION as i64],
            |row| row.get(0),
        ) {
            Ok(value) => value,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(StashError::Backend(
                    "stash generation exhausted".to_string(),
                ));
            }
            Err(e) => return Err(StashError::from(e)),
        };
        tx.execute(
            "INSERT INTO stash (hash, payload, expires_at, generation)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(hash) DO UPDATE SET
                 payload = excluded.payload,
                 expires_at = excluded.expires_at,
                 generation = excluded.generation",
            rusqlite::params![key, payload, expires_at, generation],
        )?;
        self.enforce_capacity(&tx, now as i64)?;
        tx.commit()?;
        Ok(StashWrite {
            key,
            created: !had_live,
            generation: generation as u64,
            previous_generation,
        })
    }
}

impl StashStore for SqliteStore {
    fn stash(&self, payload: &str) -> Result<StashWrite, StashError> {
        self.stash_inner(payload, None)
    }

    fn retrieve(&self, hash: &str) -> Result<Option<String>, StashError> {
        let now = now_unix();
        // Keys are stored as lowercase BLAKE3 hex; accept a marker the LLM may
        // have uppercased by lowercasing the lookup (case-insensitive retrieve).
        let key = hash.to_ascii_lowercase();
        let conn = self.lock_conn();
        // Writable stores reclaim expired rows lazily as well as on writes.
        // Readers rely on the writer's cleanup and only filter out expired rows.
        // A purge failure must not block the lookup.
        if !self.read_only
            && let Err(e) = conn.execute("DELETE FROM stash WHERE expires_at < ?", [now as i64])
        {
            warn_soft(&format!(
                "[tokenless-ccr] WARNING: stash lazy-purge failed: {e}"
            ));
        }
        match conn.query_row(
            "SELECT payload FROM stash WHERE hash = ? AND expires_at >= ?",
            rusqlite::params![key, now as i64],
            |row| row.get::<_, String>(0),
        ) {
            Ok(payload) => Ok(Some(payload)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StashError::from(e)),
        }
    }

    fn len(&self) -> usize {
        let now = now_unix();
        let conn = self.lock_conn();
        conn.query_row(
            "SELECT COUNT(*) FROM stash WHERE expires_at >= ?",
            [now as i64],
            |row| row.get(0),
        )
        .unwrap_or(0) as usize
    }

    fn evict_expired(&self) -> Result<usize, StashError> {
        let now = now_unix();
        let conn = self.lock_conn();
        let evicted = conn.execute("DELETE FROM stash WHERE expires_at < ?", [now as i64])?;
        Ok(evicted)
    }

    fn delete(&self, hash: &str, generation: u64) -> Result<bool, StashError> {
        let key = hash.to_ascii_lowercase();
        let generation = match i64::try_from(generation) {
            Ok(generation) => generation,
            Err(_) => return Ok(false),
        };
        let now = now_unix();
        let conn = self.lock_conn();
        let removed = conn.execute(
            "DELETE FROM stash WHERE hash = ? AND generation = ? AND expires_at >= ?",
            rusqlite::params![key, generation, now as i64],
        )?;
        Ok(removed > 0)
    }
}

/// Current wall-clock seconds since the Unix epoch. Used for expiry math so
/// the stash does not depend on `chrono`.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Switches the connection to WAL within [`BUSY_TIMEOUT`], then leaves that
/// timeout in place.
///
/// Concurrent first openers of a new database can collide while upgrading
/// journal locks, and SQLite reports that as busy without invoking its busy
/// handler. Hook processes of concurrent sessions open the stash exactly that
/// way, and a lost race would leave a process without any stash, so the
/// switch is retried until the shared deadline.
fn enable_wal(conn: &Connection) -> Result<(), StashError> {
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        conn.busy_timeout(deadline.saturating_duration_since(Instant::now()))?;
        match conn.execute_batch("PRAGMA journal_mode=WAL;") {
            Ok(()) => break,
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == ErrorCode::DatabaseBusy && Instant::now() < deadline =>
            {
                std::thread::sleep(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(error) => return Err(error.into()),
        }
    }
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(())
}

#[cfg(test)]
mod tests;
