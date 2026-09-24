//! Regressions for adopting idle snapshots through the two-phase storage path.
#![cfg(target_os = "linux")]

mod common;

use agentsight::genai::semantic::GenAISemanticEvent;
use agentsight::storage::sqlite::{GenAISqliteStore, PendingCallInfo, PendingOrigin};
use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

struct Fixture {
    dir: PathBuf,
    store: GenAISqliteStore,
    db: Connection,
}

impl Fixture {
    fn new() -> Self {
        let dir = common::temp_dir("pending-reconciliation");
        let path = dir.join("genai.db");
        Self {
            store: GenAISqliteStore::new_with_path(
                &path,
                agentsight::config::PeriodicStoragePolicy::default(),
            )
            .unwrap(),
            db: Connection::open(path).unwrap(),
            dir,
        }
    }

    fn count(&self) -> i64 {
        self.db
            .query_row("SELECT COUNT(*) FROM genai_events", [], |r| r.get(0))
            .unwrap()
    }

    fn finish(self) {
        drop(self.db);
        drop(self.store);
        std::fs::remove_dir_all(self.dir).unwrap();
    }
}

fn pending(call_id: &str, origin: PendingOrigin, key: Option<&str>) -> PendingCallInfo {
    PendingCallInfo {
        call_id: call_id.into(),
        trace_id: None,
        conversation_id: None,
        session_id: None,
        start_timestamp_ns: 1_000_000_000,
        pid: 1234,
        process_name: "fixture".into(),
        agent_name: None,
        http_method: Some("POST".into()),
        http_path: Some("/v1/chat/completions".into()),
        input_messages: Some("[]".into()),
        system_instructions: None,
        user_query: Some("hello".into()),
        is_sse: false,
        model: Some("gpt-4".into()),
        provider: Some("openai".into()),
        call_kind: "main".into(),
        pending_origin: origin,
        pending_match_key: key.map(str::to_owned),
    }
}

fn complete_event() -> GenAISemanticEvent {
    let mut call = common::make_test_llm_call("formal");
    call.metadata
        .insert("pending_match_key".into(), "match".into());
    GenAISemanticEvent::LLMCall(call)
}

#[test]
fn only_matching_unfinished_idle_rows_are_adopted() {
    for (origin, status, key, adopt) in [
        (PendingOrigin::IdleDrain, "pending", Some("match"), true),
        (PendingOrigin::IdleDrain, "interrupted", Some("match"), true),
        (PendingOrigin::IdleDrain, "complete", Some("match"), false),
        (
            PendingOrigin::RequestCapture,
            "pending",
            Some("match"),
            false,
        ),
        (PendingOrigin::DeadPidDrain, "pending", Some("match"), false),
        (
            PendingOrigin::IdleDrain,
            "pending",
            Some("different"),
            false,
        ),
        (PendingOrigin::IdleDrain, "pending", None, false),
    ] {
        let fixture = Fixture::new();
        fixture
            .store
            .insert_pending(&pending("snapshot", origin, Some("match")))
            .unwrap();
        fixture
            .db
            .execute("UPDATE genai_events SET status = ?1", params![status])
            .unwrap();
        let mut formal = pending("formal", PendingOrigin::RequestCapture, key);
        formal.trace_id = Some("response".into());
        formal.session_id = Some("session".into());
        formal.conversation_id = Some("conversation".into());
        fixture.store.insert_pending(&formal).unwrap();
        assert_eq!(
            fixture.count(),
            if adopt { 1 } else { 2 },
            "{origin:?}/{status}/{key:?}"
        );
        let (id, trace, session, conversation, origin): (i64, String, String, String, String) = fixture.db.query_row(
            "SELECT id, trace_id, session_id, conversation_id, pending_origin FROM genai_events WHERE call_id = 'formal'", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        ).unwrap();
        assert_eq!(id == 1, adopt);
        assert_eq!(
            (
                trace.as_str(),
                session.as_str(),
                conversation.as_str(),
                origin.as_str()
            ),
            ("response", "session", "conversation", "request_capture")
        );
        fixture.finish();
    }
}

#[test]
fn ambiguous_snapshots_are_preserved() {
    let fixture = Fixture::new();
    for id in ["idle-a", "idle-b"] {
        fixture
            .store
            .insert_pending(&pending(id, PendingOrigin::IdleDrain, Some("match")))
            .unwrap();
    }
    fixture
        .store
        .insert_pending(&pending(
            "formal",
            PendingOrigin::RequestCapture,
            Some("match"),
        ))
        .unwrap();
    fixture.store.complete_pending(&complete_event()).unwrap();
    assert_eq!(fixture.count(), 3);
    let idle_count: i64 = fixture.db.query_row(
        "SELECT COUNT(*) FROM genai_events WHERE pending_origin = 'idle_drain' AND status = 'pending'", [], |r| r.get(0)
    ).unwrap();
    assert_eq!(idle_count, 2);
    fixture.finish();
}

#[test]
fn concurrent_connections_do_not_duplicate_formal_calls() {
    for with_snapshot in [false, true] {
        let fixture = Fixture::new();
        if with_snapshot {
            fixture
                .store
                .insert_pending(&pending("idle", PendingOrigin::IdleDrain, Some("match")))
                .unwrap();
        }
        let stores: Vec<_> = (0..8)
            .map(|_| {
                GenAISqliteStore::new_with_path(
                    &fixture.dir.join("genai.db"),
                    agentsight::config::PeriodicStoragePolicy::default(),
                )
                .unwrap()
            })
            .collect();
        let barrier = Arc::new(Barrier::new(stores.len()));
        let threads: Vec<_> = stores
            .into_iter()
            .map(|store| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..3 {
                        store
                            .insert_pending(&pending(
                                "formal",
                                PendingOrigin::RequestCapture,
                                Some("match"),
                            ))
                            .unwrap();
                        store.complete_pending(&complete_event()).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(fixture.count(), 1);
        let status: String = fixture
            .db
            .query_row("SELECT status FROM genai_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "complete");
        fixture.finish();
    }
}

#[test]
fn failed_adoption_keeps_snapshot_and_allows_retry() {
    let fixture = Fixture::new();
    fixture
        .store
        .insert_pending(&pending("idle", PendingOrigin::IdleDrain, Some("match")))
        .unwrap();
    fixture
        .db
        .execute_batch(
            "CREATE TRIGGER reject_adoption BEFORE UPDATE OF call_id ON genai_events
        BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .unwrap();
    let formal = pending("formal", PendingOrigin::RequestCapture, Some("match"));
    assert!(fixture.store.insert_pending(&formal).is_err());
    let id: String = fixture
        .db
        .query_row("SELECT call_id FROM genai_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(id, "idle");
    fixture
        .db
        .execute_batch("DROP TRIGGER reject_adoption")
        .unwrap();
    fixture.store.insert_pending(&formal).unwrap();
    assert_eq!(fixture.count(), 1);
    fixture.finish();
}
