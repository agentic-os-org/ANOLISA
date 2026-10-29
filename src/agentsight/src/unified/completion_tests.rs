//! Exercise raw queue backpressure and parser-to-SQLite ownership without eBPF privileges.

use super::*;
use crate::unified::capture_runtime_tests::RuntimeFixture;
use crate::unified::response_pending_tests::feed;

fn capture(aggregator: &mut Aggregator, id: &str, text: &str) -> AggregatedResult {
    let body = serde_json::json!({"model":"fixture","messages":[{"role":"user","content":text}],"stream":true}).to_string();
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: api.openai.com\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    assert!(feed(aggregator, 1, request.as_bytes()).is_empty());
    assert!(
        feed(
            aggregator,
            0,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n"
        )
        .is_empty()
    );
    let response = format!(
        "data: {{\"id\":\"{id}\",\"model\":\"fixture\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":\"reply\"}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":12,\"completion_tokens\":1000,\"total_tokens\":1012}}}}\n\ndata: [DONE]\n\n"
    );
    let mut results = feed(aggregator, 0, response.as_bytes());
    assert_eq!(results.len(), 1);
    results.remove(0)
}

#[test]
fn bounded_raw_queue_keeps_rejected_ownership_and_drains_complete_payloads_in_order() {
    let dir = crate::unified::tests::unique_tmp_dir("paced-pipeline");
    let store = GenAISqliteStore::new_with_path(
        &dir.join("genai.db"),
        crate::config::PeriodicStoragePolicy::default(),
    )
    .unwrap();
    let mut aggregator = Aggregator::new();
    let text = "雪\"\n\0".repeat(64 * 1024);
    let first = capture(&mut aggregator, "paced-first", &text);
    let second = capture(&mut aggregator, "paced-second", &text);
    let bytes = first.retained_bytes();
    assert!(bytes >= text.len());
    let mut scheduler = CompletionScheduler::default();
    assert!(scheduler.try_push(first, bytes, 1).is_none());
    let mut rejected = Some(scheduler.try_push(second, bytes, 1).unwrap());
    assert_eq!(scheduler.bytes, bytes);
    assert_eq!(scheduler.len(), 1);
    let mapper = ResponseSessionMapper::new();
    let builder = GenAIBuilder::new();
    for (index, id) in ["paced-first", "paced-second"].iter().enumerate() {
        let raw = scheduler.pop().unwrap();
        assert_eq!(scheduler.bytes, 0);
        let mut analysis = Analyzer::new().analyze_for_delivery(&raw, true);
        drop(raw);
        let (mut output, _) =
            builder.build_for_owned_delivery(&analysis, &mapper, &HashMap::new(), false);
        output.take_raw_bodies(&mut analysis);
        drop(analysis);
        assert_eq!(output.events.len(), 1);
        let expected = serde_json::to_string(&output.events[0]).unwrap();
        super::super::delivery::complete_local_genai(&output, &store, &[]).unwrap();
        let db = rusqlite::Connection::open(dir.join("genai.db")).unwrap();
        let (response_id, json): (String, String) = db
            .query_row(
                "SELECT trace_id,event_json FROM genai_events ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(response_id, *id);
        assert_eq!(json, expected);
        let saved: GenAISemanticEvent = serde_json::from_str(&json).unwrap();
        let GenAISemanticEvent::LLMCall(call) = saved else {
            panic!("expected LLM call")
        };
        use sha2::{Digest, Sha256};
        let conversation = format!(
            "{:x}",
            Sha256::digest(format!("conversation{id}").as_bytes())
        );
        assert_eq!(call.metadata["conversation_id"], conversation[..32]);
        let session = format!("{:x}", Sha256::digest(b"sessionpaced-first"));
        assert_eq!(call.metadata["session_id"], session[..32]);
        assert!(
            call.request
                .raw_body
                .as_deref()
                .unwrap()
                .contains("fixture")
        );
        assert!(
            matches!(&call.request.messages[0].parts[0], crate::genai::semantic::MessagePart::Text { content } if content == &text)
        );
        if index == 0 {
            assert!(
                scheduler
                    .try_push(rejected.take().unwrap(), usize::MAX, 1)
                    .is_none()
            );
        }
    }
    assert_eq!(scheduler.len(), 0);
    assert_eq!(scheduler.bytes, 0);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn runtime_paces_real_delivery_and_reports_queue_accounting() {
    let mut fixture = RuntimeFixture::new(100);
    let mut aggregator = Aggregator::new();
    let result = capture(&mut aggregator, "queued-runtime", "payload");
    let pid = result.pid();
    let bytes = result.retained_bytes();
    let budget = fixture.completions.budget.clone().unwrap();
    let permit = budget.acquire(WorkClass::Maintenance).unwrap();
    fixture.process_aggregated_results(vec![result]);
    assert!(fixture.rows().is_empty());
    let metrics = fixture.runtime_metrics().unwrap();
    assert_eq!(metrics.completion_queue_count, 1);
    assert_eq!(metrics.completion_queue_bytes, bytes as u64);
    assert_eq!(metrics.pending_genai_bytes, bytes as u64);
    fixture.tick();
    assert!(!fixture.drain_one_completion(false));
    drop(permit);
    fixture.flush_completions_for_pid(pid);
    assert_eq!(fixture.rows(), ["queued-runtime"]);
    let metrics = fixture.runtime_metrics().unwrap();
    assert_eq!(metrics.completion_queue_count, 0);
    assert_eq!(metrics.pending_genai_bytes, 0);
    assert!(!fixture.drain_one_completion(false));
}

#[test]
fn runtime_preserves_fifo_under_backpressure_and_delivers_oversized_captures() {
    let mut fixture = RuntimeFixture::new(100);
    fixture.runtime_limits.pending_genai_max_count = 1;
    let budget = fixture.completions.budget.clone().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _permit = budget.acquire(WorkClass::Maintenance).unwrap();
        ready_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(30));
    });
    ready_rx.recv().unwrap();
    let mut aggregator = Aggregator::new();
    let first = capture(&mut aggregator, "fifo-first", "first");
    let second = capture(&mut aggregator, "fifo-second", "second");
    fixture.process_aggregated_results(vec![first, second]);
    fixture.flush_completions();
    worker.join().unwrap();
    assert_eq!(fixture.rows(), ["fifo-first", "fifo-second"]);
    fixture.runtime_limits.pending_genai_max_bytes = 1;
    let oversized = capture(&mut aggregator, "oversized-runtime", &"雪".repeat(4096));
    fixture.process_aggregated_results(vec![oversized]);
    assert_eq!(
        fixture.rows(),
        ["fifo-first", "fifo-second", "oversized-runtime"]
    );
    assert_eq!(fixture.completions.bytes, 0);
}

#[test]
fn runtime_without_pacing_delivers_immediately() {
    let mut fixture = RuntimeFixture::new(0);
    let result = capture(&mut Aggregator::new(), "immediate-runtime", "immediate");
    fixture.process_aggregated_results(vec![result]);
    assert_eq!(fixture.rows(), ["immediate-runtime"]);
    assert_eq!(fixture.completions.len(), 0);
}

#[test]
fn runtime_accounting_failures_preserve_queued_and_oversized_delivery() {
    let mut fixture = RuntimeFixture::new(25);
    fixture
        .completions
        .budget
        .as_ref()
        .unwrap()
        .poison_for_test();
    let mut aggregator = Aggregator::new();
    fixture.process_aggregated_results(vec![capture(
        &mut aggregator,
        "failed-budget-queued",
        "first",
    )]);
    fixture.runtime_limits.pending_genai_max_bytes = 1;
    fixture.process_aggregated_results(vec![capture(
        &mut aggregator,
        "failed-budget-inflight",
        "second",
    )]);
    assert_eq!(
        fixture.rows(),
        ["failed-budget-queued", "failed-budget-inflight"]
    );
    assert_eq!(fixture.completions.len(), 0);
}

struct ObservedSink(Arc<Mutex<Vec<String>>>);
impl GenAIExporter for ObservedSink {
    fn name(&self) -> &str {
        "observed-inactive-sink"
    }
    fn is_active(&self) -> bool {
        false
    }
    fn export(&self, events: &[GenAISemanticEvent]) {
        // Observe invocation, including the inactive sink's own write-gate check.
        for event in events {
            if let GenAISemanticEvent::LLMCall(call) = event {
                self.0
                    .lock()
                    .unwrap()
                    .push(call.metadata["response_id"].clone());
            }
        }
    }
}

#[test]
fn runtime_sqlite_retries_keep_fifo_and_do_not_repeat_sink_delivery() {
    let mut fixture = RuntimeFixture::new(0);
    let store = fixture.genai_sqlite_store.clone().unwrap();
    store.set_test_busy_timeout(Duration::ZERO);
    let observed = Arc::new(Mutex::new(Vec::new()));
    fixture
        .genai_exporters
        .push(Box::new(ObservedSink(Arc::clone(&observed))));
    let lock = rusqlite::Connection::open(fixture.directory.join("genai_events.db")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut aggregator = Aggregator::new();
    fixture.process_aggregated_results(vec![
        capture(&mut aggregator, "retry-first", "first"),
        capture(&mut aggregator, "retry-second", "second"),
    ]);
    assert_eq!(fixture.pending_genai.local_retries.len(), 2);
    assert!(fixture.rows().is_empty());
    let budget = Arc::new(CpuBudget::new(100).unwrap());
    fixture.completions.budget = Some(Arc::clone(&budget));
    let permit = budget.acquire(WorkClass::Maintenance).unwrap();
    fixture.retry_local_genai(false);
    assert_eq!(fixture.pending_genai.local_retries.len(), 2);
    drop(permit);
    fixture.retry_local_genai(true);
    assert_eq!(fixture.pending_genai.local_retries.len(), 2);
    lock.execute_batch("COMMIT").unwrap();
    // Admission is verified above; other tests' process CPU must not delay this FIFO check.
    fixture.completions.budget = None;
    fixture.retry_local_genai(true);
    assert_eq!(fixture.rows(), ["retry-first", "retry-second"]);
    assert_eq!(*observed.lock().unwrap(), ["retry-first", "retry-second"]);
    assert_eq!(fixture.pending_genai.bytes, 0);
    fixture.genai_sqlite_store = None;
    fixture.retry_local_genai(true);
}

#[test]
fn runtime_retry_overflow_waits_for_sqlite_without_dropping_owned_calls() {
    let mut fixture = RuntimeFixture::new(0);
    let store = fixture.genai_sqlite_store.clone().unwrap();
    store.set_test_busy_timeout(Duration::ZERO);
    let observed = Arc::new(Mutex::new(Vec::new()));
    fixture
        .genai_exporters
        .push(Box::new(ObservedSink(Arc::clone(&observed))));
    let lock = rusqlite::Connection::open(fixture.directory.join("genai_events.db")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut aggregator = Aggregator::new();
    fixture.process_aggregated_results(vec![capture(&mut aggregator, "overflow-first", "first")]);
    assert_eq!(fixture.pending_genai.local_retries.len(), 1);
    fixture.runtime_limits.pending_genai_max_count = 0;
    fixture.runtime_limits.pending_genai_max_bytes = 1;
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        lock.execute_batch("COMMIT").unwrap();
    });
    fixture.process_aggregated_results(vec![capture(&mut aggregator, "overflow-second", "second")]);
    release.join().unwrap();
    assert_eq!(fixture.rows(), ["overflow-first", "overflow-second"]);
    assert_eq!(
        *observed.lock().unwrap(),
        ["overflow-first", "overflow-second"]
    );
    assert!(fixture.pending_genai.local_retries.is_empty());
    assert_eq!(fixture.pending_genai.bytes, 0);
}

#[test]
fn runtime_pending_limits_share_the_raw_queue_allowance() {
    let mut fixture = RuntimeFixture::new(0);
    let captured = capture(&mut Aggregator::new(), "deferred-runtime", "deferred");
    let analysis = Analyzer::new().analyze_for_delivery(&captured, true);
    let (output, _) = GenAIBuilder::new().build_for_owned_delivery(
        &analysis,
        &fixture.response_mapper,
        &HashMap::new(),
        false,
    );
    fixture.pending_genai.push(PendingGenAI {
        events: output.events.into(),
        response_id: output.pending_response_id.unwrap_or_default(),
        pid: captured.pid(),
        created_at: Instant::now(),
    });
    let raw = capture(
        &mut Aggregator::new(),
        "shared-raw-runtime",
        "queued raw payload",
    );
    fixture.runtime_limits.pending_genai_max_count = 3;
    fixture.runtime_limits.pending_genai_max_bytes =
        fixture.pending_genai.bytes + raw.retained_bytes() - 1;
    fixture.completions.budget = Some(Arc::new(CpuBudget::new(100).unwrap()));
    fixture.enforce_pending_genai_limits();
    assert_eq!(fixture.pending_genai.len(), 1);
    assert!(fixture.rows().is_empty());
    assert!(fixture.completions.try_push(raw, usize::MAX, 3).is_none());
    fixture.enforce_pending_genai_limits();
    assert_eq!(fixture.pending_genai.len(), 0);
    assert_eq!(fixture.pending_genai.bytes, 0);
    assert_eq!(fixture.rows(), ["deferred-runtime"]);
    assert_eq!(fixture.completions.len(), 1);
    fixture.flush_completions();
    assert_eq!(fixture.rows(), ["deferred-runtime", "shared-raw-runtime"]);
}

#[test]
fn runtime_retry_overflow_times_out_and_responds_to_shutdown() {
    for (stop, queued) in [(false, true), (true, true), (false, false), (true, false)] {
        let mut fixture = RuntimeFixture::new(0);
        let store = fixture.genai_sqlite_store.clone().unwrap();
        store.set_test_busy_timeout(Duration::ZERO);
        let lock = rusqlite::Connection::open(fixture.directory.join("genai_events.db")).unwrap();
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut aggregator = Aggregator::new();
        if queued {
            fixture.process_aggregated_results(vec![capture(&mut aggregator, "kept", "first")]);
        }
        fixture.runtime_limits.pending_genai_max_count = 0;
        fixture.runtime_limits.pending_genai_max_bytes = 1;
        let running = Arc::clone(&fixture.running);
        let stopper = std::thread::spawn(move || {
            if stop {
                std::thread::sleep(Duration::from_millis(30));
                running.store(false, Ordering::SeqCst);
            }
        });
        let start = Instant::now();
        fixture.process_aggregated_results(vec![capture(&mut aggregator, "dropped", "second")]);
        stopper.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(3));
        if !stop {
            assert!(start.elapsed() >= Duration::from_secs(1));
        }
        assert_eq!(
            fixture.pending_genai.local_retries.len(),
            usize::from(queued)
        );
        lock.execute_batch("COMMIT").unwrap();
        fixture.retry_local_genai(true);
        assert_eq!(fixture.rows(), if queued { vec!["kept"] } else { vec![] });
    }
}

#[test]
fn runtime_pid_exit_and_both_shutdown_paths_drain_queued_captures() {
    for mode in ["exit", "run", "shutdown"] {
        let mut fixture = RuntimeFixture::new(100);
        let result = capture(&mut Aggregator::new(), mode, "shutdown payload");
        let pid = result.pid();
        let budget = fixture.completions.budget.clone().unwrap();
        let permit = budget.acquire(WorkClass::Maintenance).unwrap();
        fixture.process_aggregated_results(vec![result]);
        assert_eq!(fixture.completions.len(), 1);
        drop(permit);
        match mode {
            "exit" => fixture.handle_procmon_event(&crate::probes::procmon::Event::Exit {
                pid,
                tid: pid,
                uid: 0,
                timestamp_ns: 10,
                comm: "fixture".into(),
                exit_code: 0,
            }),
            "run" => {
                fixture.running.store(false, Ordering::SeqCst);
                assert_eq!(fixture.run().unwrap(), 0);
            }
            _ => fixture.shutdown(),
        }
        assert_eq!(fixture.rows(), [mode]);
        assert_eq!(fixture.completions.bytes, 0);
    }
}

#[test]
fn queued_result_accounting_covers_protocol_variants_and_shared_sources() {
    use crate::aggregator::{Http2Stream, StreamId};
    use crate::parser::http2::{Http2FrameType, ParsedHttp2Frame};
    let captured = capture(&mut Aggregator::new(), "accounting", "雪");
    let AggregatedResult::SseComplete(mut pair) = captured else {
        panic!("expected SSE")
    };
    let pid = pair.connection_id.pid;
    pair.response.sse_events[0].id = Some("event-id".into());
    pair.response.sse_events[0].event = Some("message".into());
    pair.response.sse_continuation_bytes = Some(vec![0; 256]);
    let request = AggregatedResult::RequestOnly {
        connection_id: pair.connection_id,
        request: pair.request.clone(),
    };
    let response = AggregatedResult::ResponseOnly {
        connection_id: pair.connection_id,
        response: pair.response.clone(),
    };
    let partial_bytes = request.retained_bytes();
    let abandoned = AggregatedResult::Abandoned {
        reason: crate::aggregator::RemoveReason::Shutdown,
        partial: Box::new(request.clone()),
    };
    assert_eq!(
        abandoned.retained_bytes(),
        std::mem::size_of::<AggregatedResult>() + partial_bytes
    );
    let frame = ParsedHttp2Frame {
        frame_type: Http2FrameType::Data,
        flags: 0,
        stream_id: 1,
        payload_offset: 0,
        payload_len: 1,
        source_event: pair.request.source_event.clone(),
    };
    let frames = vec![frame.clone(), frame.clone()];
    let source_bytes = std::mem::size_of_val(frame.source_event.as_ref())
        + frame.source_event.buf.capacity()
        + frame.source_event.comm.capacity();
    let expected = std::mem::size_of::<AggregatedResult>()
        + frames.capacity() * std::mem::size_of::<ParsedHttp2Frame>()
        + source_bytes;
    let raw_frames = AggregatedResult::Http2Frames {
        connection_id: pair.connection_id,
        frames,
    };
    assert_eq!(
        raw_frames.retained_bytes(),
        expected,
        "one shared source is charged once"
    );
    let mut stream = Http2Stream::new(StreamId::new(pair.connection_id, 1), 1);
    stream.request_headers = Some(frame.clone());
    stream.response_headers = Some(frame.clone());
    stream.request_data_frames.push(frame.clone());
    stream.response_data_frames.push(frame);
    stream.decoded_request_headers = Some(vec![(":path".into(), "/long/path".into())]);
    stream.decoded_response_headers = Some(vec![(":status".into(), "200".into())]);
    let decoded = AggregatedResult::Http2StreamComplete(stream);
    assert!(decoded.retained_bytes() > source_bytes);
    let mut process =
        crate::aggregator::AggregatedProcess::new(pid, pid, 0, 0, "fixture".into(), 1);
    process.filename = Some("fixture.bin".into());
    process.args = Some("--fixture".into());
    process.session_id = Some("session".into());
    process.stdout_data = vec![0; 512];
    process.stderr_data = vec![0; 256];
    let process = AggregatedResult::ProcessComplete(process);
    assert!(process.retained_bytes() >= std::mem::size_of::<AggregatedResult>() + 768);
    for result in [
        request,
        response,
        abandoned,
        raw_frames,
        decoded,
        process,
        AggregatedResult::HttpComplete(pair.clone()),
        AggregatedResult::SseComplete(pair),
    ] {
        assert_eq!(result.pid(), pid);
        assert!(result.retained_bytes() > std::mem::size_of::<AggregatedResult>());
    }
}
