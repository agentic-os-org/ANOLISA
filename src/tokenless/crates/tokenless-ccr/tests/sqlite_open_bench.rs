//! Opt-in synthetic measurements of real store opens and refresh writes.
//!
//! Run with `cargo test --release --locked -p tokenless-ccr --test sqlite_open_bench -- --ignored --nocapture`.
//! Copy this test to a baseline checkout to compare the same workload. These
//! measurements are not complete Hook timings or cold-filesystem-cache results.

#![cfg(feature = "sqlite")]

use std::time::{Duration, Instant};

use rusqlite::Connection;
use tokenless_ccr::{SqliteStore, StashStore, compute_key};

fn median_ms(mut samples: Vec<Duration>) -> f64 {
    samples.sort_unstable();
    samples[samples.len() / 2].as_secs_f64() * 1000.0
}

#[test]
#[ignore = "synthetic performance measurements; run explicitly on an idle host"]
fn measure_stash_open_and_refresh() {
    println!(
        "rows,payload_bytes,lookup_vm_steps,upgrade_ms,reopen_median_ms,refresh_median_ms,insert_median_ms"
    );
    for (rows, payload_bytes) in [(0, 128), (100, 128), (10_000, 128), (10_000, 16_384)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stash.db");
        drop(SqliteStore::new(&path).unwrap());

        // Seed outside the measurement. Remove the additive index so the first
        // open also measures its one-time creation on an existing database.
        let mut conn = Connection::open(&path).unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute("DROP INDEX IF EXISTS idx_stash_generation", [])
            .unwrap();
        let retained = format!("{:0width$}", 0, width = payload_bytes);
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO stash(hash, payload, expires_at, generation)
                     VALUES (?, ?, 9999999999, ?)",
                )
                .unwrap();
            for i in 0..rows {
                let payload = format!("{i:0width$}", width = payload_bytes);
                insert
                    .execute(rusqlite::params![
                        compute_key(payload.as_bytes()),
                        payload,
                        i + 1
                    ])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
        drop(conn);

        let start = Instant::now();
        let upgraded = SqliteStore::new(&path).unwrap();
        let upgrade_ms = start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(upgraded.len(), rows);
        if rows > 0 {
            assert_eq!(
                upgraded
                    .retrieve(&compute_key(retained.as_bytes()))
                    .unwrap(),
                Some(retained.clone())
            );
        }
        drop(upgraded);

        let conn = Connection::open(&path).unwrap();
        // Keep in sync with MAX_GENERATION_QUERY in src/backends/sqlite.rs.
        let mut query = conn
            .prepare("SELECT COALESCE(MAX(generation), 0) FROM stash")
            .unwrap();
        let maximum: usize = query.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(maximum, rows);
        let steps = query.get_status(rusqlite::StatementStatus::VmStep);
        drop(query);
        drop(conn);

        let mut opens = Vec::new();
        for _ in 0..11 {
            let start = Instant::now();
            let store = SqliteStore::new(&path).unwrap();
            opens.push(start.elapsed());
            drop(store);
        }

        let store = SqliteStore::new(&path).unwrap();
        let mut writes = Vec::new();
        let mut previous = rows as u64;
        for _ in 0..51 {
            let start = Instant::now();
            let write = store.stash(&retained).unwrap();
            writes.push(start.elapsed());
            assert!(write.generation > previous);
            previous = write.generation;
        }
        assert_eq!(
            store.retrieve(&compute_key(retained.as_bytes())).unwrap(),
            Some(retained)
        );
        let mut inserts = Vec::new();
        for i in 0..51 {
            let payload = format!("n{i:0width$}", width = payload_bytes - 1);
            let start = Instant::now();
            let write = store.stash(&payload).unwrap();
            inserts.push(start.elapsed());
            assert!(write.created);
            assert!(write.generation > previous);
            previous = write.generation;
        }
        assert_eq!(store.len(), (rows.max(1) + 51).min(10_000));
        println!(
            "{rows},{payload_bytes},{steps},{upgrade_ms:.3},{:.3},{:.3},{:.3}",
            median_ms(opens),
            median_ms(writes),
            median_ms(inserts)
        );
    }
}
