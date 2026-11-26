use super::*;
use std::thread;

fn tmp_store(ttl: u64, cap: usize) -> (SqliteStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::with_limits(dir.path().join("stash.db"), ttl, cap).unwrap();
    (store, dir)
}

fn expire_all(store: &SqliteStore) {
    let expired_at = now_unix().saturating_sub(1) as i64;
    store
        .lock_conn()
        .execute("UPDATE stash SET expires_at = ?", [expired_at])
        .expect("expire stash rows");
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

#[test]
fn closed_writer_leaves_sidecars_for_read_only_retrieval() {
    let (writer, dir) = tmp_store(60, 100);
    let write = writer.stash("read-only shell payload").unwrap();
    drop(writer);
    assert!(dir.path().join("stash.db-wal").is_file());
    assert!(dir.path().join("stash.db-shm").is_file());

    let reader = SqliteStore::open_read_only(&dir.path().join("stash.db")).unwrap();
    assert_eq!(
        reader.retrieve(&write.key).unwrap().as_deref(),
        Some("read-only shell payload")
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
    let before = now_unix();
    let (store, _dir) = tmp_store(60, 100);
    let key = store.stash("ephemeral").unwrap().key;
    let after = now_unix();
    let expires_at: i64 = store
        .lock_conn()
        .query_row(
            "SELECT expires_at FROM stash WHERE hash = ?",
            [&key],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        ((before + 60) as i64..=(after + 60) as i64).contains(&expires_at),
        "unexpected expires_at {expires_at} outside [{}, {}]",
        before + 60,
        after + 60,
    );
    assert_eq!(store.retrieve(&key).unwrap(), Some("ephemeral".to_string()));
    expire_all(&store);
    assert_eq!(store.retrieve(&key).unwrap(), None);
}

#[test]
fn evict_expired_reports_count() {
    let (store, _dir) = tmp_store(60, 100);
    let _ = store.stash("a").unwrap();
    let _ = store.stash("b").unwrap();
    expire_all(&store);
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
    let (store, _dir) = tmp_store(60, 100);
    let write = store.stash("ephemeral").unwrap();
    expire_all(&store);
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
fn concurrent_fresh_openers_all_get_a_store() {
    // Hook processes of concurrent sessions open a new stash database
    // at the same moment. Every opener must get a store: a lost race
    // used to fail with "database is locked" and leave that process
    // without reversible compression.
    for _ in 0..20 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stash.db");
        let barrier = std::sync::Barrier::new(8);
        thread::scope(|scope| {
            for index in 0..8 {
                let (path, barrier) = (&path, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    let store = SqliteStore::new(path).unwrap();
                    store.stash(&format!("payload-{index}")).unwrap();
                });
            }
        });
        assert_eq!(SqliteStore::new(&path).unwrap().len(), 8);
    }
}

#[test]
fn expired_rows_physically_deleted_after_retrieve() {
    // Lazy purge: a retrieve must physically DELETE expired rows, not
    // merely filter them via the WHERE clause. Mirrors headroom's
    // `SqliteCcrStore::get` lazy-purge sweep.
    let (store, _dir) = tmp_store(60, 100);
    let _ = store.stash("a").unwrap();
    let _ = store.stash("b").unwrap();
    assert_eq!(store.len(), 2);
    expire_all(&store);
    // Both rows are now expired. A retrieve for any (absent) key triggers
    // the purge sweep; `len()` must then read 0 because the rows were
    // physically deleted, not just hidden.
    assert_eq!(store.retrieve("000000000000000000000000").unwrap(), None);
    assert_eq!(store.len(), 0);
}

#[test]
fn retrieve_succeeds_when_lazy_purge_fails() {
    let (store, _dir) = tmp_store(60, 100);
    let live = store.stash("live payload").unwrap();
    let expired = store.stash("expired payload").unwrap();
    {
        let conn = store.lock_conn();
        conn.execute(
            "UPDATE stash SET expires_at = 0 WHERE hash = ?",
            [&expired.key],
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER block_lazy_purge BEFORE DELETE ON stash
             BEGIN SELECT RAISE(ABORT, 'lazy purge blocked'); END;",
        )
        .unwrap();
    }

    assert_eq!(
        store.retrieve(&live.key).unwrap().as_deref(),
        Some("live payload")
    );
    assert_eq!(store.retrieve(&expired.key).unwrap(), None);
    let still_present: bool = store
        .lock_conn()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM stash WHERE hash = ?)",
            [&expired.key],
            |row| row.get(0),
        )
        .unwrap();
    assert!(still_present);
}

#[test]
fn write_only_sessions_reclaim_expired_payloads() {
    let (store, dir) = tmp_store(60, 3);
    let live = store.stash("still needed").unwrap();
    let path = dir.path().join("stash.db");

    for session in 0..3 {
        let expired = store.stash(&format!("old session {session}")).unwrap();
        store
            .lock_conn()
            .execute(
                "UPDATE stash SET expires_at = 0 WHERE hash = ?",
                [&expired.key],
            )
            .unwrap();

        // Each hook opens a new connection. Check physical rows before
        // any retrieve, which would otherwise hide write-only growth.
        let writer = SqliteStore::with_limits(&path, 60, 3).unwrap();
        let current = writer.stash("current session").unwrap();
        let conn = Connection::open(&path).unwrap();
        let rows: usize = conn
            .query_row("SELECT COUNT(*) FROM stash", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 2, "expired payload survived session {session}");
        let retained: Vec<String> = conn
            .prepare("SELECT payload FROM stash ORDER BY payload")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(retained, ["current session", "still needed"]);
        assert!(!writer.delete(&expired.key, expired.generation).unwrap());
        assert_eq!(writer.len(), 2);
        assert_ne!(current.key, live.key);
    }
}

#[test]
fn write_cleanup_recreation_preserves_generation_ownership() {
    let (store, dir) = tmp_store(60, 3);
    let first = store.stash("payload").unwrap();
    store
        .lock_conn()
        .execute("UPDATE stash SET expires_at = 0", [])
        .unwrap();

    let writer = SqliteStore::with_limits(dir.path().join("stash.db"), 60, 3).unwrap();
    writer.stash("new session").unwrap();
    let second = writer.stash("payload").unwrap();
    assert!(second.created);
    assert_eq!(second.previous_generation, None);
    assert!(second.generation > first.generation);
    assert!(!store.delete(&first.key, first.generation).unwrap());
    assert_eq!(
        writer.retrieve(&second.key).unwrap(),
        Some("payload".into())
    );
}

#[test]
fn failed_stash_rolls_back_expired_cleanup() {
    let (store, _dir) = tmp_store(60, 3);
    let expired = store.stash("expired").unwrap();
    store
        .lock_conn()
        .execute("UPDATE stash SET expires_at = 0", [])
        .unwrap();
    store
        .lock_conn()
        .execute(
            "UPDATE stash_metadata SET last_generation = ? WHERE singleton = 1",
            [MAX_GENERATION as i64],
        )
        .unwrap();

    assert!(store.stash("new session").is_err());
    let retained: String = store
        .lock_conn()
        .query_row(
            "SELECT payload FROM stash WHERE hash = ?",
            [&expired.key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained, "expired");
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
fn reopening_a_full_stash_uses_bounded_generation_query_work() {
    let (store, dir) = tmp_store(60, DEFAULT_CAPACITY);
    {
        let mut conn = store.lock_conn();
        let tx = conn.transaction().unwrap();
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO stash(hash, payload, expires_at, generation)
                     VALUES (?, 'payload', 9999999999, ?)",
                )
                .unwrap();
            for generation in 1..=DEFAULT_CAPACITY as i64 {
                insert
                    .execute(rusqlite::params![generation.to_string(), generation])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
    }
    drop(store);

    let reopened = SqliteStore::new(dir.path().join("stash.db")).unwrap();
    {
        let conn = reopened.lock_conn();
        let mut query = conn.prepare(MAX_GENERATION_QUERY).unwrap();
        let maximum: i64 = query.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(maximum, DEFAULT_CAPACITY as i64);
        // Count SQLite work rather than wall time: scanning 10,000 rows
        // must fail this check even on a fast or lightly loaded host.
        let steps = query.get_status(rusqlite::StatementStatus::VmStep);
        assert!(steps < 100, "generation lookup took {steps} VM steps");
    }
    assert_eq!(reopened.retrieve("1").unwrap(), Some("payload".into()));
    assert_eq!(
        reopened.stash("next").unwrap().generation,
        DEFAULT_CAPACITY as u64 + 1
    );
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
fn concurrent_upgrade_preserves_payloads_and_generation_high_water() {
    for high_water in [3_u64, 20] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stash.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE stash (
                hash TEXT PRIMARY KEY,
                payload TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                generation INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO stash VALUES ('old', 'retained payload', 9999999999, 7);
            CREATE TABLE stash_metadata (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                last_generation INTEGER NOT NULL CHECK(last_generation >= 0)
            );",
        )
        .unwrap();
        conn.execute("INSERT INTO stash_metadata VALUES (1, ?)", [high_water])
            .unwrap();
        drop(conn);

        let barrier = std::sync::Barrier::new(8);
        let mut generations = thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|i| {
                    let (path, barrier) = (&path, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        let store = SqliteStore::new(path).unwrap();
                        assert_eq!(
                            store.retrieve("old").unwrap(),
                            Some("retained payload".into())
                        );
                        store.stash(&format!("new payload {i}")).unwrap().generation
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        generations.sort_unstable();
        let first = high_water.max(7) + 1;
        assert_eq!(generations, (first..first + 8).collect::<Vec<_>>());
        assert_eq!(SqliteStore::new(&path).unwrap().len(), 9);
    }
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

/// A stash warning must not be able to fail the command when stderr
/// itself cannot be written (a full filesystem behind redirected
/// logs, a closed descriptor). `eprintln!` panics on that shape,
/// turning the fail-soft warning into a process failure while the
/// compression result is fine; `warn_soft` must survive it. Redirect
/// fd 2 to /dev/full, call the helper, restore, and assert the call
/// returned — a panicking write makes this test fail.
#[test]
#[cfg(target_os = "linux")]
fn warn_soft_survives_an_unwritable_stderr() {
    use std::os::unix::io::AsRawFd;

    let dev_full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("open /dev/full");
    let saved = unsafe { libc::dup(2) };
    assert!(saved >= 0, "dup stderr");
    unsafe {
        libc::dup2(dev_full.as_raw_fd(), 2);
    }
    // The helper's whole contract: no panic, the write error is
    // discarded.
    warn_soft("[tokenless-ccr] WARNING: simulated stderr failure");
    // Restore before any assert can print.
    let restored = unsafe { libc::dup2(saved, 2) };
    let _ = std::io::Write::flush(&mut std::io::stderr());
    assert!(restored >= 0, "restore stderr");
    unsafe {
        libc::close(saved);
    }
    drop(dev_full);
}
