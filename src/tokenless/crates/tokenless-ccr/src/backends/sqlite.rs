//! SQLite-backed stash store (default production backend).
//!
//! Persists stashed payloads to a single file so state survives across the
//! short-lived processes that tokenless hooks fork+exec on every call. Uses
//! WAL mode and `BEGIN IMMEDIATE` for database-wide write serialization.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::key::compute_key;
use crate::store::{MAX_GENERATION, StashError, StashStore, StashWrite};

/// Default time-to-live for an entry: 1 hour. A retrieve window of an hour
/// comfortably covers a typical agent session's compress→retrieve round trip.
const DEFAULT_TTL_SECONDS: u64 = 60 * 60;

/// Default maximum number of live entries before FIFO eviction.
const DEFAULT_CAPACITY: usize = 10_000;

pub struct SqliteStore {
    conn: Mutex<Connection>,
    ttl_seconds: u64,
    capacity: usize,
}

impl SqliteStore {
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
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             PRAGMA synchronous=NORMAL;",
        )?;
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
        tx.execute(
            "CREATE TABLE IF NOT EXISTS stash_metadata (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                last_generation INTEGER NOT NULL CHECK(last_generation >= 0)
            )",
            [],
        )?;
        let max_generation: i64 = tx.query_row(
            "SELECT COALESCE(MAX(generation), 0) FROM stash",
            [],
            |row| row.get(0),
        )?;
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
        })
    }

    /// Acquire the connection guard, recovering from poison rather than
    /// failing. A poisoned mutex means a prior holder panicked; for our
    /// single-statement workload the SQLite connection itself stays usable,
    /// so we clear the poison and reuse the underlying guard. This mirrors
    /// the fail-soft policy in `tokenless-stats::recorder::StatsRecorder`.
    fn lock_conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| {
            eprintln!(
                "[tokenless-ccr] WARNING: sqlite mutex poisoned by a previous panic; recovering: {poisoned}"
            );
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
        // Lazy purge sweep: physically drop expired rows on every retrieve so
        // the DB file does not grow unbounded by stale entries that the
        // `expires_at >= ?` filter only hides, never deletes. Mirrors headroom's
        // `SqliteCcrStore::get`. Best-effort — a purge failure must not block
        // the lookup; it falls through to the SELECT below.
        if let Err(e) = conn.execute("DELETE FROM stash WHERE expires_at < ?", [now as i64]) {
            eprintln!("[tokenless-ccr] WARNING: stash lazy-purge failed: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn tmp_store(ttl: u64, cap: usize) -> (SqliteStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::with_limits(dir.path().join("stash.db"), ttl, cap).unwrap();
        (store, dir)
    }

    #[test]
    fn retrieve_is_case_insensitive() {
        let (store, _dir) = tmp_store(60, 100);
        let key = store.stash("payload").unwrap().key;
        assert_eq!(key, key.to_ascii_lowercase());
        let upper = key.to_uppercase();
        assert_ne!(upper, key);
        assert_eq!(store.retrieve(&upper).unwrap(), Some("payload".to_string()));
    }

    #[test]
    fn round_trip_persists_across_connections() {
        let (store, dir) = tmp_store(60, 100);
        let key = store.stash("payload-A").unwrap().key;
        assert_eq!(store.retrieve(&key).unwrap(), Some("payload-A".to_string()));

        // A second connection to the same file sees the entry: proves the
        // store survives across processes (the hook fork+exec case).
        let store2 = SqliteStore::new(dir.path().join("stash.db")).unwrap();
        assert_eq!(
            store2.retrieve(&key).unwrap(),
            Some("payload-A".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn database_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("stash.db");
        std::fs::write(&path, []).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let _store = SqliteStore::new(&path).unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600,
        );
    }

    #[test]
    fn retrieve_missing_returns_none() {
        let (store, _dir) = tmp_store(60, 100);
        assert_eq!(store.retrieve("000000000000000000000000").unwrap(), None);
    }

    #[test]
    fn expired_entry_not_retrievable() {
        let (store, _dir) = tmp_store(1, 100);
        let key = store.stash("ephemeral").unwrap().key;
        thread::sleep(std::time::Duration::from_secs(2));
        assert_eq!(store.retrieve(&key).unwrap(), None);
    }

    #[test]
    fn evict_expired_reports_count() {
        let (store, _dir) = tmp_store(1, 100);
        let _ = store.stash("a").unwrap();
        let _ = store.stash("b").unwrap();
        thread::sleep(std::time::Duration::from_secs(2));
        assert_eq!(store.evict_expired().unwrap(), 2);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn fifo_eviction_when_over_capacity() {
        let (store, _dir) = tmp_store(60, 3);
        let k0 = store.stash("0").unwrap().key;
        let _ = store.stash("1").unwrap();
        let _ = store.stash("2").unwrap();
        let _ = store.stash("3").unwrap(); // surplus=1, evicts oldest live (k0)
        assert_eq!(store.retrieve(&k0).unwrap(), None);
        assert!(store.len() <= 3);
    }

    #[test]
    fn delete_live_entry_returns_true() {
        let (store, _dir) = tmp_store(60, 100);
        let write = store.stash("payload").unwrap();
        assert!(store.delete(&write.key, write.generation).unwrap());
        assert_eq!(store.retrieve(&write.key).unwrap(), None);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn delete_missing_returns_false() {
        let (store, _dir) = tmp_store(60, 100);
        assert!(!store.delete("000000000000000000000000", 1).unwrap());
    }

    #[test]
    fn delete_expired_returns_false() {
        // Trait: Ok(false) when already expired. DELETE must include
        // `expires_at >= now` so an expired row is not reported as a live delete.
        let (store, _dir) = tmp_store(1, 100);
        let write = store.stash("ephemeral").unwrap();
        thread::sleep(std::time::Duration::from_secs(2));
        assert!(!store.delete(&write.key, write.generation).unwrap());
        assert_eq!(store.retrieve(&write.key).unwrap(), None);
    }

    #[test]
    fn delete_stale_generation_across_connections_returns_false() {
        // Two independent connections on one file (hook fork+exec). A creates;
        // B refreshes (as if emitting a marker); A's stale-generation delete
        // must not remove B's live row.
        let (store_a, dir) = tmp_store(60, 100);
        let first = store_a.stash("payload").unwrap();
        assert!(first.created);
        let store_b = SqliteStore::new(dir.path().join("stash.db")).unwrap();
        let second = store_b.stash("payload").unwrap();
        assert!(!second.created);
        assert_ne!(first.generation, second.generation);
        assert_eq!(first.previous_generation, None);
        assert_eq!(second.previous_generation, Some(first.generation));
        assert!(!store_a.delete(&first.key, first.generation).unwrap());
        assert_eq!(
            store_b.retrieve(&second.key).unwrap(),
            Some("payload".to_string())
        );
        assert!(store_b.delete(&second.key, second.generation).unwrap());
    }

    #[test]
    fn concurrent_writes_no_deadlock() {
        let (store, _dir) = tmp_store(60, 10_000);
        let store = std::sync::Arc::new(store);
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let s = store.clone();
                thread::spawn(move || {
                    for j in 0..50 {
                        let _ = s.stash(&format!("p-{i}-{j}")).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(store.len() <= 10_000);
    }

    #[test]
    fn expired_rows_physically_deleted_after_retrieve() {
        // Lazy purge: a retrieve must physically DELETE expired rows, not
        // merely filter them via the WHERE clause. Mirrors headroom's
        // `SqliteCcrStore::get` lazy-purge sweep.
        let (store, _dir) = tmp_store(1, 100);
        let _ = store.stash("a").unwrap();
        let _ = store.stash("b").unwrap();
        assert_eq!(store.len(), 2);
        thread::sleep(std::time::Duration::from_secs(2));
        // Both rows are now expired. A retrieve for any (absent) key triggers
        // the purge sweep; `len()` must then read 0 because the rows were
        // physically deleted, not just hidden.
        assert_eq!(store.retrieve("000000000000000000000000").unwrap(), None);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn lazy_purge_recreate_never_reuses_generation() {
        let (store, dir) = tmp_store(60, 100);
        let first = store.stash("payload").unwrap();
        store
            .lock_conn()
            .execute("UPDATE stash SET expires_at = 0", [])
            .unwrap();
        assert_eq!(store.retrieve(&first.key).unwrap(), None);
        let second = store.stash("payload").unwrap();
        assert!(second.generation > first.generation);
        assert!(!store.delete(&first.key, first.generation).unwrap());
        assert_eq!(store.retrieve(&second.key).unwrap(), Some("payload".into()));
        drop(dir);
    }

    #[test]
    fn evict_expired_recreate_never_reuses_generation() {
        let (store, _dir) = tmp_store(60, 100);
        let first = store.stash("payload").unwrap();
        store
            .lock_conn()
            .execute("UPDATE stash SET expires_at = 0", [])
            .unwrap();
        assert_eq!(store.evict_expired().unwrap(), 1);
        let second = store.stash("payload").unwrap();
        assert!(second.generation > first.generation);
        assert!(!store.delete(&first.key, first.generation).unwrap());
    }

    #[test]
    fn capacity_eviction_recreate_never_reuses_generation() {
        let (store, _dir) = tmp_store(60, 1);
        let first = store.stash("payload").unwrap();
        let _other = store.stash("other").unwrap();
        assert_eq!(store.retrieve(&first.key).unwrap(), None);
        let second = store.stash("payload").unwrap();
        assert!(second.generation > first.generation);
        assert!(!store.delete(&first.key, first.generation).unwrap());
    }

    #[test]
    fn delete_recreate_never_reuses_generation() {
        let (store, _dir) = tmp_store(60, 100);
        let first = store.stash("payload").unwrap();
        assert!(store.delete(&first.key, first.generation).unwrap());
        let second = store.stash("payload").unwrap();
        assert!(second.generation > first.generation);
        assert!(!store.delete(&first.key, first.generation).unwrap());
        assert_eq!(store.retrieve(&second.key).unwrap(), Some("payload".into()));
    }

    #[test]
    fn current_generation_schema_migrates_high_water_mark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stash.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "CREATE TABLE stash (
                hash TEXT PRIMARY KEY,
                payload TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                generation INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO stash(hash, payload, expires_at, generation)
             VALUES ('hash', 'payload', 9999999999, 7)",
            [],
        )
        .unwrap();
        drop(conn);
        let store = SqliteStore::with_limits(&path, 60, 100).unwrap();
        assert_eq!(store.stash("next").unwrap().generation, 8);
    }

    #[test]
    fn pre_generation_schema_migrates_and_starts_at_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stash.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "CREATE TABLE stash (
                hash TEXT PRIMARY KEY,
                payload TEXT NOT NULL,
                expires_at INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO stash(hash, payload, expires_at)
             VALUES ('hash', 'payload', 9999999999)",
            [],
        )
        .unwrap();
        drop(conn);
        let store = SqliteStore::with_limits(&path, 60, 100).unwrap();
        assert_eq!(store.stash("next").unwrap().generation, 1);
        let conn = Connection::open(&path).unwrap();
        let generation: i64 = conn
            .query_row(
                "SELECT generation FROM stash WHERE hash = 'hash'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generation, 0);
    }

    #[test]
    fn generation_exhaustion_does_not_mutate_stash() {
        let (store, _dir) = tmp_store(60, 100);
        let existing = store.stash("existing").unwrap();
        store
            .lock_conn()
            .execute(
                "UPDATE stash_metadata SET last_generation = ? WHERE singleton = 1",
                [MAX_GENERATION as i64],
            )
            .unwrap();
        let before = store.retrieve(&existing.key).unwrap();
        let error = store.stash("new").unwrap_err();
        assert!(error.to_string().contains("generation exhausted"));
        assert_eq!(store.retrieve(&existing.key).unwrap(), before);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn concurrent_stash_serializes_ownership_across_connections() {
        let (store_a, dir) = tmp_store(60, 100);
        let store_b = std::sync::Arc::new(SqliteStore::new(dir.path().join("stash.db")).unwrap());
        let store_a = std::sync::Arc::new(store_a);
        let (read_done_tx, read_done_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let a = {
            let store_a = store_a.clone();
            thread::spawn(move || {
                let hook = || {
                    read_done_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                };
                store_a.stash_inner("payload", Some(&hook)).unwrap()
            })
        };
        read_done_rx.recv().unwrap();
        let (b_done_tx, b_done_rx) = std::sync::mpsc::channel();
        let b = {
            let store_b = store_b.clone();
            thread::spawn(move || {
                let result = store_b.stash("payload");
                b_done_tx.send(result).unwrap();
            })
        };
        // B cannot complete while A's IMMEDIATE transaction is held after
        // its live-row read; releasing A makes the overlap deterministic.
        assert!(
            b_done_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err()
        );
        release_tx.send(()).unwrap();
        let first = a.join().unwrap();
        let second = b_done_rx.recv().unwrap().unwrap();
        b.join().unwrap();
        assert!(first.created);
        assert!(!second.created);
        assert!(second.generation > first.generation);
        assert_eq!(first.previous_generation, None);
        assert_eq!(second.previous_generation, Some(first.generation));
        assert!(!store_a.delete(&first.key, first.generation).unwrap());
        assert_eq!(
            store_b.retrieve(&second.key).unwrap(),
            Some("payload".into())
        );
    }
}
