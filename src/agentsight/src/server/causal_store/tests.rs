//! Behaviour tests for [`super::CausalCaseStore`].
//!
//! The store exists for one promise: what was persisted comes back unchanged
//! after the process that wrote it is long gone. Everything else — key
//! distinctness, overwrite semantics — protects that promise from being
//! silently narrowed.

use std::path::PathBuf;

use agentsight_sqlite_lifecycle::MaintenanceStatus;
use rusqlite::params;

use super::{CaseKey, CausalCaseStore, CausalMaintenancePolicy};

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("causal-store-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn store_at(path: &std::path::Path) -> CausalCaseStore {
    CausalCaseStore::open_private(path).unwrap()
}

fn set_updated_at(store: &CausalCaseStore, session_key: &str, updated_at_ns: i64) {
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE causal_cases SET updated_at_ns = ?1 WHERE session_key = ?2",
            params![updated_at_ns, session_key],
        )
        .unwrap();
}

#[test]
fn a_case_round_trips_byte_for_byte() {
    // The stored value is replayed verbatim to the client, so even formatting
    // differences would be visible. Byte equality is the contract.
    let dir = tmp_dir("roundtrip");
    let store = store_at(&dir);
    let json = r#"{"title":"案例","verdict":"fail","nodes":[]}"#;
    store
        .put(
            CaseKey {
                session_key: "conv:s1",
                round: Some(2),
                complaint: "为什么失败",
            },
            json,
        )
        .unwrap();
    assert_eq!(
        store
            .get(CaseKey {
                session_key: "conv:s1",
                round: Some(2),
                complaint: "为什么失败"
            })
            .unwrap()
            .as_deref(),
        Some(json)
    );
}

#[test]
fn a_case_survives_reopening_the_database() {
    // The whole point: restart loses the memory cache, not the answer.
    let dir = tmp_dir("reopen");
    let json = r#"{"verdict":"success"}"#;
    {
        let store = store_at(&dir);
        store
            .put(
                CaseKey {
                    session_key: "sess:s1",
                    round: None,
                    complaint: "c",
                },
                json,
            )
            .unwrap();
    }
    let store = store_at(&dir);
    assert_eq!(
        store
            .get(CaseKey {
                session_key: "sess:s1",
                round: None,
                complaint: "c"
            })
            .unwrap()
            .as_deref(),
        Some(json)
    );
}

#[test]
fn a_rerun_replaces_the_stored_answer() {
    // `force` re-runs the pipeline; the stored case must reflect the newest
    // answer, not keep both and guess.
    let dir = tmp_dir("overwrite");
    let store = store_at(&dir);
    let key = CaseKey {
        session_key: "conv:s1",
        round: None,
        complaint: "c",
    };
    store.put(key, r#"{"run":1}"#).unwrap();
    store.put(key, r#"{"run":2}"#).unwrap();
    assert_eq!(store.get(key).unwrap().as_deref(), Some(r#"{"run":2}"#));
    assert_eq!(store.count().unwrap(), 1, "replacement, not accumulation");
}

#[test]
fn a_different_complaint_is_a_different_case() {
    // The memory cache requires an exact complaint match; the store must not
    // be looser, or a new question would resurrect an old answer.
    let dir = tmp_dir("complaints");
    let store = store_at(&dir);
    store
        .put(
            CaseKey {
                session_key: "conv:s1",
                round: None,
                complaint: "慢",
            },
            r#"{"a":1}"#,
        )
        .unwrap();
    store
        .put(
            CaseKey {
                session_key: "conv:s1",
                round: None,
                complaint: "错",
            },
            r#"{"a":2}"#,
        )
        .unwrap();
    assert_eq!(store.count().unwrap(), 2);
    assert_eq!(
        store
            .get(CaseKey {
                session_key: "conv:s1",
                round: None,
                complaint: "错"
            })
            .unwrap()
            .as_deref(),
        Some(r#"{"a":2}"#)
    );
}

#[test]
fn the_last_round_and_round_zero_are_distinct() {
    // `None` means "the last round" and is a different question from round 0,
    // because the last round moves as a session grows.
    let dir = tmp_dir("rounds");
    let store = store_at(&dir);
    store
        .put(
            CaseKey {
                session_key: "conv:s1",
                round: None,
                complaint: "c",
            },
            r#"{"r":"last"}"#,
        )
        .unwrap();
    store
        .put(
            CaseKey {
                session_key: "conv:s1",
                round: Some(0),
                complaint: "c",
            },
            r#"{"r":0}"#,
        )
        .unwrap();
    assert_eq!(store.count().unwrap(), 2);
    assert_eq!(
        store
            .get(CaseKey {
                session_key: "conv:s1",
                round: Some(0),
                complaint: "c"
            })
            .unwrap()
            .as_deref(),
        Some(r#"{"r":0}"#)
    );
}

#[test]
fn a_missing_case_is_none_not_an_error() {
    let store = store_at(&tmp_dir("missing"));
    assert!(
        store
            .get(CaseKey {
                session_key: "conv:ghost",
                round: None,
                complaint: "c"
            })
            .unwrap()
            .is_none()
    );
}

#[test]
fn maintenance_removes_cases_older_than_retention() {
    let dir = tmp_dir("retention");
    let store = store_at(&dir);
    let expired = CaseKey {
        session_key: "sess:expired",
        round: None,
        complaint: "c",
    };
    let recent = CaseKey {
        session_key: "sess:recent",
        round: None,
        complaint: "c",
    };
    store.put(expired, r#"{"age":"expired"}"#).unwrap();
    store.put(recent, r#"{"age":"recent"}"#).unwrap();
    set_updated_at(&store, expired.session_key, 1);

    let report = store
        .maintain(CausalMaintenancePolicy {
            retention_days: 1,
            max_db_size_mb: 0,
        })
        .unwrap();

    assert_eq!(report.expired_cases, 1);
    assert_eq!(report.size.status, MaintenanceStatus::Disabled);
    assert!(store.get(expired).unwrap().is_none());
    assert!(store.get(recent).unwrap().is_some());
}

#[test]
fn size_maintenance_deletes_oldest_cases() {
    let dir = tmp_dir("size");
    let store = store_at(&dir);
    let payload = "x".repeat(20_000);
    for index in 0..100 {
        let session_key = format!("sess:{index:03}");
        store
            .put(
                CaseKey {
                    session_key: &session_key,
                    round: None,
                    complaint: "c",
                },
                &payload,
            )
            .unwrap();
        set_updated_at(&store, &session_key, index + 1);
    }
    let newest = CaseKey {
        session_key: "sess:099",
        round: None,
        complaint: "c",
    };

    let report = store
        .maintain(CausalMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 1,
        })
        .unwrap();

    assert!(report.size.deleted_rows > 0);
    assert!(report.size.after.logical_bytes <= 1024 * 1024 * 9 / 10);
    assert!(
        store
            .get(CaseKey {
                session_key: "sess:000",
                round: None,
                complaint: "c",
            })
            .unwrap()
            .is_none()
    );
    assert!(store.get(newest).unwrap().is_some());
    assert!(store.count().unwrap() < 100);
}

#[test]
fn size_maintenance_keeps_the_only_latest_case() {
    let dir = tmp_dir("latest");
    let store = store_at(&dir);
    let latest = CaseKey {
        session_key: "sess:latest",
        round: None,
        complaint: "c",
    };
    store.put(latest, &"x".repeat(2 * 1024 * 1024)).unwrap();

    let report = store
        .maintain(CausalMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 1,
        })
        .unwrap();

    assert_eq!(report.size.status, MaintenanceStatus::NoRows);
    assert_eq!(report.size.deleted_rows, 0);
    assert!(store.get(latest).unwrap().is_some());
}

#[test]
fn size_maintenance_does_not_delete_below_the_threshold() {
    let dir = tmp_dir("below-size-threshold");
    let store = store_at(&dir);
    let key = CaseKey {
        session_key: "sess:small",
        round: None,
        complaint: "c",
    };
    store.put(key, r#"{"small":true}"#).unwrap();

    let report = store
        .maintain(CausalMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 1,
        })
        .unwrap();

    assert_eq!(report.size.status, MaintenanceStatus::BelowTrigger);
    assert_eq!(report.size.deleted_rows, 0);
    assert!(store.get(key).unwrap().is_some());
}

#[test]
fn busy_retention_checkpoint_stops_before_size_pruning() {
    let dir = tmp_dir("busy-retention-checkpoint");
    let store = store_at(&dir);
    let payload = "x".repeat(20_000);
    for index in 0..100 {
        let session_key = format!("sess:expired-{index:03}");
        store
            .put(
                CaseKey {
                    session_key: &session_key,
                    round: None,
                    complaint: "c",
                },
                &payload,
            )
            .unwrap();
        set_updated_at(&store, &session_key, 1);
    }
    let retained = CaseKey {
        session_key: "sess:retained",
        round: None,
        complaint: "c",
    };
    store.put(retained, &payload).unwrap();
    assert_eq!(
        store.checkpoint().unwrap(),
        agentsight_sqlite_lifecycle::CheckpointOutcome::Completed
    );

    let reader = rusqlite::Connection::open(dir.join("causal.db")).unwrap();
    reader
        .execute_batch("BEGIN; SELECT COUNT(*) FROM causal_cases;")
        .unwrap();

    let report = store
        .maintain(CausalMaintenancePolicy {
            retention_days: 1,
            max_db_size_mb: 1,
        })
        .unwrap();

    assert_eq!(report.expired_cases, 100);
    assert_eq!(report.size.status, MaintenanceStatus::CheckpointBusy);
    assert_eq!(report.size.deleted_rows, 0);
    assert!(store.get(retained).unwrap().is_some());
}
