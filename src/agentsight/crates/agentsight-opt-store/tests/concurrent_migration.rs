use agentsight_opt_store::OptimizationStore;
use rusqlite::Connection;
use std::sync::{Arc, Barrier};

#[test]
fn concurrent_opens_of_a_legacy_store_are_idempotent() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("sight-migration-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut errors = Vec::new();
    for iteration in 0..60 {
        let path = root.join(format!("legacy-{iteration}.db"));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL;
                CREATE TABLE optimization_results (
                    session_id TEXT PRIMARY KEY, perf TEXT, perf_issues TEXT, cost TEXT,
                    cost_waste TEXT, accuracy TEXT, created_at_ns INTEGER NOT NULL,
                    updated_at_ns INTEGER NOT NULL
                );
                INSERT INTO optimization_results VALUES ('fixture-session','fixture-perf',NULL,NULL,NULL,NULL,1,1);").unwrap();
        }
        let barrier = Arc::new(Barrier::new(4));
        let handles = (0..4)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    OptimizationStore::new_with_path(&path)
                        .map(|store| {
                            assert_eq!(
                                store
                                    .get("fixture-session")
                                    .unwrap()
                                    .unwrap()
                                    .perf
                                    .as_deref(),
                                Some("fixture-perf")
                            );
                        })
                        .map_err(|error| error.to_string())
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            if let Err(error) = handle.join().unwrap() {
                errors.push(error);
            }
        }
    }
    std::fs::remove_dir_all(&root).unwrap();
    assert!(
        errors.is_empty(),
        "valid concurrent opens failed: {errors:?}"
    );
}
