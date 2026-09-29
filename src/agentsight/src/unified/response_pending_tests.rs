//! Parser-to-storage lifecycle regressions without loading or attaching probes.

use super::*;
use crate::aggregator::{AggregatedResult, ConnectionState};
use crate::config::RuntimeLimits;
use crate::probes::sslsniff::SslEvent;
use crate::storage::sqlite::PendingOrigin;

const PID: u32 = 3_999_989;

fn feed(aggregator: &mut Aggregator, rw: i32, bytes: &[u8]) -> Vec<AggregatedResult> {
    let event = SslEvent {
        source: 0,
        timestamp_ns: 1_000_000_000,
        delta_ns: 0,
        pid: PID,
        tid: PID,
        uid: 0,
        len: bytes.len() as u32,
        rw,
        comm: "fixture".into(),
        buf: bytes.to_vec(),
        is_handshake: false,
        ssl_ptr: 123,
    };
    aggregator.process_result(Parser::new().parse_event(Event::Ssl(event)))
}

fn fixture(prefix: &[u8]) -> Aggregator {
    let mut aggregator = Aggregator::with_limits(
        4,
        &RuntimeLimits {
            connection_idle_timeout_secs: 0,
            ..Default::default()
        },
    );
    let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"hello"}]}"#;
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: api.openai.com\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    assert!(feed(&mut aggregator, 1, request.as_bytes()).is_empty());
    assert!(feed(&mut aggregator, 0, prefix).is_empty());
    aggregator
}

fn prefixes() -> [&'static [u8]; 3] {
    [
        b"HTTP/1.1 200 OK\r\nContent-Typ",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{",
    ]
}

#[test]
fn response_pending_exit_and_dead_pid_preserve_crash_evidence() {
    for prefix in prefixes() {
        for dead_pid in [false, true] {
            for exit_status in [0, 9] {
                let mut aggregator = fixture(prefix);
                // Periodic eviction must not race either persistence path.
                aggregator.http_mut().evict_idle_and_oversized();
                let drained = if dead_pid {
                    assert!(!crate::utils::procfs::proc_pid(PID).exists());
                    aggregator.drain_dead_pid_connections()
                } else {
                    aggregator.drain_connections_for_pid(PID)
                };
                assert_eq!(drained.len(), 1);
                let (id, state) = &drained[0];
                assert!(matches!(state, ConnectionState::ResponsePending { .. }));
                let pending = GenAIBuilder::new()
                    .build_pending_from_request(
                        state.pending_request().expect("retained request"),
                        id,
                        &ResponseSessionMapper::new(),
                        &HashMap::new(),
                    )
                    .expect("pending LLM call");
                let dir = super::tests::unique_tmp_dir("response-pending");
                let store = GenAISqliteStore::new_with_path(
                    &dir.join("genai.db"),
                    crate::config::PeriodicStoragePolicy::default(),
                )
                .unwrap();
                let interruptions =
                    InterruptionStore::new_with_path(&dir.join("interruptions.db")).unwrap();
                store.insert_pending(&pending).unwrap();
                let rows = store.list_pending_for_pids(&[PID as i32]).unwrap();
                assert_eq!(rows.len(), 1);
                record_agent_crash_interruptions(
                    PID,
                    "fixture",
                    ProcessExitStatus::decode(exit_status),
                    &rows,
                    &interruptions,
                    Some(&store),
                );
                let crashes = interruptions
                    .list(0, i64::MAX, None, Some("agent_crash"), None, None, 100)
                    .unwrap();
                assert_eq!(crashes.len(), usize::from(exit_status == 9));
                assert_eq!(
                    store.list_pending_for_pids(&[PID as i32]).unwrap().len(),
                    usize::from(exit_status == 0)
                );
                std::fs::remove_dir_all(&dir).unwrap();
            }
        }
    }
}

#[test]
fn response_pending_idle_snapshot_persists_once_and_can_resume() {
    let body = r#"{"id":"fixture","object":"chat.completion","created":0,"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let sse_headers = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
    let sse_body = b"data: {\"id\":\"fixture\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3,\"total_tokens\":8}}\n\ndata: [DONE]\n\n";
    for (prefix, remaining, is_sse) in [
        (&response.as_bytes()[..0], response.as_bytes(), false),
        (
            &response.as_bytes()[..26],
            &response.as_bytes()[26..],
            false,
        ),
        (
            &response.as_bytes()[..response.len() - 1],
            &response.as_bytes()[response.len() - 1..],
            false,
        ),
        (sse_headers.as_slice(), sse_body.as_slice(), true),
    ] {
        for deferred in [false, true] {
            let mut aggregator = fixture(prefix);
            aggregator.http_mut().evict_idle_and_oversized();
            let snapshots = aggregator.snapshot_idle_connections();
            assert_eq!(snapshots.len(), 1);
            assert!(aggregator.snapshot_idle_connections().is_empty());
            let (id, state) = &snapshots[0];
            let mut pending = GenAIBuilder::new()
                .build_pending_from_request(
                    state.pending_request().expect("retained request"),
                    id,
                    &ResponseSessionMapper::new(),
                    &HashMap::new(),
                )
                .unwrap();
            pending.pending_origin = PendingOrigin::IdleDrain;
            let dir = super::tests::unique_tmp_dir("response-pending");
            let store = GenAISqliteStore::new_with_path(
                &dir.join("genai.db"),
                crate::config::PeriodicStoragePolicy::default(),
            )
            .unwrap();
            store.insert_pending(&pending).unwrap();
            let db = rusqlite::Connection::open(dir.join("genai.db")).unwrap();
            let snapshot_id: i64 = db
                .query_row("SELECT id FROM genai_events", [], |r| r.get(0))
                .unwrap();
            let count = || {
                db.query_row("SELECT COUNT(*) FROM genai_events", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap()
            };
            aggregator.http_mut().evict_idle_and_oversized();
            let completed = feed(&mut aggregator, 0, remaining);
            assert_eq!(completed.len(), 1);
            let mut mapper = ResponseSessionMapper::new();
            let session = "11111111-1111-4111-8111-111111111111";
            if !deferred {
                let buf = br#"{"responseId":"fixture"}"#.to_vec();
                mapper.process_filewrite(&crate::probes::FileWriteEvent {
                    pid: PID,
                    tid: PID,
                    uid: 0,
                    timestamp_ns: 1,
                    write_size: buf.len() as u32,
                    comm: "fixture".into(),
                    filename: format!("{session}.jsonl"),
                    cgroup_id: 0,
                    buf,
                });
            }
            let analyzed = Analyzer::new().analyze_aggregated(&completed[0]);
            let (mut output, formal) =
                GenAIBuilder::new().build_with_pending(&analyzed, &mapper, &HashMap::new());
            assert_eq!(output.pending_response_id.is_some(), deferred);
            assert!(!output.events.is_empty());
            let formal = formal.unwrap();
            assert_eq!(formal.pending_match_key, pending.pending_match_key);
            // Both immediate export and deferred session resolution insert first.
            store.insert_pending(&formal).unwrap();
            store.insert_pending(&formal).unwrap();
            assert_eq!(count(), 1);
            assert_eq!(store.list_pending_for_pids(&[PID as i32]).unwrap().len(), 1);
            for event in &mut output.events {
                if let GenAISemanticEvent::LLMCall(call) = event {
                    // Simulate the session resolution that precedes deferred export.
                    call.metadata.insert("session_id".into(), session.into());
                }
                store.complete_pending(event).unwrap();
                store.insert_pending(&formal).unwrap();
                store.complete_pending(event).unwrap();
            }
            assert_eq!(count(), 1);
            let (id, call_id, status, input, output, session_id, body, streamed): (
                i64,
                String,
                String,
                i64,
                i64,
                String,
                String,
                bool,
            ) = db
                .query_row(
                    "SELECT id, call_id, status, input_tokens, output_tokens, session_id,
                     output_messages, is_sse FROM genai_events",
                    [],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                        ))
                    },
                )
                .unwrap();
            assert_eq!(id, snapshot_id);
            assert_eq!(call_id, formal.call_id);
            assert_eq!(status, "complete");
            assert_eq!((input, output), (5, 3));
            assert_eq!(session_id, session);
            assert!(body.contains("hello"));
            assert_eq!(streamed, is_sse);
            assert!(!aggregator.has_pending());
            drop(db);
            drop(store);
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
