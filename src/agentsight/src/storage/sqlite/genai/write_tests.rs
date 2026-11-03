//! Regressions for managed checkpoints and bounded completion transactions.

use super::*;
use crate::genai::semantic::{GenAISemanticEvent, LLMCall};

impl GenAISqliteStore {
    /// Exercise retry admission without a five-second SQLite busy timeout per attempt.
    pub(crate) fn set_test_busy_timeout(&self, timeout: std::time::Duration) {
        self.conn.lock().unwrap().busy_timeout(timeout).unwrap();
    }
}

fn fixture() -> (GenAISqliteStore, PathBuf) {
    let dir = std::env::temp_dir().join(format!("genai-writes-{}", uuid::Uuid::new_v4()));
    let store =
        GenAISqliteStore::new_with_path(&dir.join("genai.db"), PeriodicStoragePolicy::default())
            .unwrap();
    (store, dir)
}

fn event(id: &str) -> GenAISemanticEvent {
    let request =
        serde_json::from_value(serde_json::json!({"messages":[],"stream":false})).unwrap();
    GenAISemanticEvent::LLMCall(LLMCall::new(
        id.into(),
        1000,
        "openai".into(),
        "fixture".into(),
        request,
        1,
        "fixture".into(),
    ))
}

#[test]
fn paced_retention_covers_every_owned_table_and_yields_to_capture_priority() {
    let (mut store, dir) = fixture();
    store.storage_policy.retention_days = 1;
    store.complete_captured(&event("old-event")).unwrap();
    let mut recent = event("recent-event");
    if let GenAISemanticEvent::LLMCall(call) = &mut recent {
        call.start_timestamp_ns = i64::MAX as u64;
        call.end_timestamp_ns = i64::MAX as u64;
    }
    store.complete_captured(&recent).unwrap();
    {
        let conn = store.conn.lock().unwrap();
        conn.execute_batch("CREATE TABLE evaluation_runs (id INTEGER PRIMARY KEY, created_at TEXT, result_json TEXT);
            INSERT INTO evaluation_runs VALUES (1,'2000-01-01','{}'),(2,'2999-01-01','{}');
            INSERT INTO agent_resource_samples(timestamp_ns,pid,cpu_percent,memory_bytes) VALUES (1,1,0,0),(9223372036854775807,1,0,0);
            INSERT INTO tokenless_statistics(source,record_id,record_timestamp,timestamp_ns,content_included,record_json)
            VALUES ('fixture',1,'old',1,0,'{}'),('fixture',2,'recent',9223372036854775807,0,'{}');").unwrap();
    }
    let mut cursor = MaintenanceCursor::default();
    let guard = store.prioritize_capture();
    assert!(!store.maintain_step(0, &mut cursor).unwrap());
    drop(guard);
    assert!((0..32).any(|_| store.maintain_step(0, &mut cursor).unwrap()));
    let conn = store.conn.lock().unwrap();
    for table in [
        "genai_events",
        "agent_resource_samples",
        "evaluation_runs",
        "tokenless_statistics",
    ] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 1,
            "retention must visit {table} and keep its recent row"
        );
    }
    drop(conn);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stepped_maintenance_resumes_to_capacity_without_deleting_pending_rows() {
    let (mut store, dir) = fixture();
    store.storage_policy.retention_days = 0;
    for index in 0..8 {
        let mut value = event(&format!("bulk-{index}"));
        if let GenAISemanticEvent::LLMCall(call) = &mut value {
            call.request.raw_body = Some("x".repeat(300 * 1024));
        }
        store.complete_captured(&value).unwrap();
    }
    store.conn.lock().unwrap().execute("INSERT INTO genai_events(event_type,call_id,status,start_timestamp_ns,event_json) VALUES ('llm_call','pending','pending',1000,'{}')", []).unwrap();
    let count = || {
        store
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM genai_events", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    let mut cursor = MaintenanceCursor::default();
    let mut previous = count();
    let mut finished = false;
    for _ in 0..200 {
        finished = store.maintain_step(1, &mut cursor).unwrap();
        let current = count();
        assert!(
            previous - current <= 1,
            "a small dataset must not be wiped in one step"
        );
        previous = current;
        if finished {
            break;
        }
    }
    assert!(finished);
    assert!(count() > 1 && count() < 9);
    assert_eq!(
        store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT status FROM genai_events WHERE call_id='pending'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "pending"
    );
    assert!(store.size_snapshot().unwrap().logical_bytes <= 1024 * 1024 * 9 / 10);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn managed_capture_disables_auto_checkpoint_without_changing_sync() {
    let (store, dir) = fixture();
    let pragma = |name| {
        store
            .conn
            .lock()
            .unwrap()
            .query_row(name, [], |row| row.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(pragma("PRAGMA wal_autocheckpoint"), 1000);
    let synchronous = pragma("PRAGMA synchronous");
    store.use_managed_checkpoints().unwrap();
    assert_eq!(pragma("PRAGMA wal_autocheckpoint"), 0);
    assert_eq!(pragma("PRAGMA synchronous"), synchronous);
    let maintenance = GenAISqliteStore::open_maintenance_existing(
        &dir.join("genai.db"),
        PeriodicStoragePolicy::default(),
        &store,
    )
    .unwrap();
    store.complete_captured(&event("managed")).unwrap();
    assert_eq!(
        maintenance.checkpoint_passive().unwrap(),
        agentsight_sqlite_lifecycle::CheckpointOutcome::Completed
    );
    drop(maintenance);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn completion_batch_preserves_pending_row_and_is_idempotent() {
    let (store, dir) = fixture();
    store.conn.lock().unwrap().execute(
        "INSERT INTO genai_events(event_type,call_id,status,start_timestamp_ns,event_json) VALUES ('llm_call','pending','pending',1000,'{}')", []
    ).unwrap();
    let events = vec![
        event("new"),
        event("pending"),
        event("last"),
        event("new"),
        event("tail"),
    ];
    store.complete_captured_batch(&events).unwrap();
    store.complete_captured_batch(&events).unwrap();
    {
        let conn = store.conn.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM genai_events", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            4
        );
        let pending: (i64, String, String) = conn
            .query_row(
                "SELECT id,status,event_json FROM genai_events WHERE call_id='pending'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            pending,
            (
                1,
                "complete".into(),
                serde_json::to_string(&events[1]).unwrap()
            )
        );
    }
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_batch_rolls_back_and_flush_keeps_only_failed_events() {
    let (store, dir) = fixture();
    store.conn.lock().unwrap().execute_batch(
        "CREATE TRIGGER reject_event BEFORE INSERT ON genai_events WHEN NEW.call_id='reject' BEGIN SELECT RAISE(ABORT, 'injected failure'); END;"
    ).unwrap();
    let events = vec![event("first"), event("reject"), event("last")];
    assert!(store.try_store_batch(&events, false).is_err());
    {
        let conn = store.conn.lock().unwrap();
        assert!(conn.is_autocommit());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM genai_events", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    store.pending.lock().unwrap().extend(events);
    store.flush();
    assert_eq!(store.pending.lock().unwrap().len(), 1);
    store
        .conn
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_event")
        .unwrap();
    store.flush();
    assert!(store.pending.lock().unwrap().is_empty());
    assert_eq!(
        store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(DISTINCT call_id),COUNT(*) FROM genai_events",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        (3, 3)
    );
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shared_call_id_on_a_tool_row_does_not_suppress_the_llm_call() {
    // A stream_chunk/tool_use row carries its parent LLM call id, and it can
    // be written before the completed call. Dedup must key on the LLM row, not
    // on any event that happens to share the id: matching the tool row made
    // `complete_captured` take the promotion path, fail its status-guarded
    // UPDATE, and return "row already exists" without ever inserting the call.
    let (store, dir) = fixture();
    store
        .conn
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO genai_events(event_type,call_id,status,start_timestamp_ns,event_json)
             VALUES ('tool_use','shared','complete',500,'{}')",
            [],
        )
        .unwrap();

    store.complete_captured(&event("shared")).unwrap();

    let conn = store.conn.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM genai_events
             WHERE event_type='llm_call' AND call_id='shared'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "the completed LLM call must still be stored");
    drop(conn);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}
