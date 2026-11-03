//! Lifecycle and retained-allocation regressions with deterministic maintenance time.

use super::*;
use crate::aggregator::RemoveReason;
use std::collections::HashMap;

fn event(id: u64, rw: i32, bytes: &[u8]) -> Rc<SslEvent> {
    Rc::new(SslEvent {
        source: 0,
        timestamp_ns: 1_000,
        delta_ns: 0,
        pid: std::process::id(),
        tid: 1,
        uid: 0,
        len: bytes.len() as u32,
        rw,
        comm: "fixture".into(),
        buf: bytes.to_vec(),
        is_handshake: false,
        ssl_ptr: id,
    })
}

fn request(id: u64, body: &[u8], length: usize) -> ParsedRequest {
    ParsedRequest {
        method: "POST".into(),
        path: "/v1/chat/completions".into(),
        version: 11,
        headers: HashMap::from([("content-length".into(), length.to_string())]),
        body_offset: 0,
        body_len: body.len(),
        source_event: event(id, 1, body),
        reassembled_body: None,
    }
}

fn sse_headers(id: u64) -> ParsedResponse {
    ParsedResponse {
        version: 11,
        status_code: 200,
        reason: "OK".into(),
        headers: HashMap::from([("content-type".into(), "text/event-stream".into())]),
        body_offset: 0,
        body_len: 0,
        source_event: event(id, 0, b""),
    }
}

fn reason(results: &[AggregatedResult], expected: RemoveReason) {
    assert!(matches!(results, [AggregatedResult::Abandoned { reason, .. }] if *reason == expected));
}

#[test]
fn lru_removal_keeps_only_metadata_and_clears_every_side_cache() {
    let mut http = HttpConnectionAggregator::with_capacity(2);
    let old = ConnectionId::from_ssl_event(&event(1, 1, b""));
    http.process_request(request(1, b"one", 3));
    http.process_request(request(2, b"two", 3));
    http.sse_continuation_buffers.put(old, vec![1; 128]);
    http.sse_read_state.put(old, SseReadState::default());
    http.idle_snapshotted.put(old, ());
    http.request_chunks
        .put(old, super::super::chunk_stream::ChunkStream::default());
    http.last_activity.put(
        old,
        Instant::now() - crate::aggregator::result::ACTIVE_CONNECTION_GRACE,
    );
    http.process_request(request(3, b"new", 3));
    let results = http.take_ready();
    reason(&results, RemoveReason::LruEvicted);
    let AggregatedResult::Abandoned { partial, .. } = &results[0] else {
        unreachable!()
    };
    let AggregatedResult::RequestOnly {
        request,
        connection_id,
    } = partial.as_ref()
    else {
        unreachable!()
    };
    assert_eq!(*connection_id, old);
    assert!(request.body().is_empty());
    assert!(request.source_event.buf.is_empty());
    assert!(!http.connections.contains(&old));
    assert!(!http.last_activity.contains(&old));
    assert!(!http.idle_snapshotted.contains(&old));
    assert!(!http.sse_read_state.contains(&old));
    assert!(!http.sse_continuation_buffers.contains(&old));
    assert!(!http.request_chunks.contains(&old));
    assert_eq!(http.active_connections(), 2);
}

#[test]
fn replacing_a_stream_emits_its_partial_output() {
    let mut http = HttpConnectionAggregator::new();
    http.set_compact_sse(true);
    http.process_request(request(1, b"{}", 2));
    http.process_response(sse_headers(1));
    let source = event(1, 0, b"data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n");
    let weak = Rc::downgrade(&source);
    http.process_raw_body_data(&source);
    drop(source);
    assert!(
        weak.upgrade().is_none(),
        "live stream must release raw TLS captures"
    );
    http.process_request(request(1, b"new", 3));
    let results = http.take_ready();
    reason(&results, RemoveReason::Replaced);
    let AggregatedResult::Abandoned { partial, .. } = &results[0] else {
        unreachable!()
    };
    let AggregatedResult::SseComplete(pair) = partial.as_ref() else {
        unreachable!()
    };
    assert_eq!(
        pair.response.json_body()[0]["choices"][0]["delta"]["content"],
        "partial"
    );
    assert_eq!(pair.request.body(), b"{}");
    assert!(http.has_pending());
}

#[test]
fn soft_snapshot_borrows_once_and_hard_tick_owns_the_partial() {
    let mut http = HttpConnectionAggregator::with_limits(4, 8 * 1024, Duration::ZERO);
    http.hard_timeout = Duration::from_secs(10);
    http.set_compact_sse(true);
    let request = request(1, b"{}", 2);
    let source = Rc::clone(&request.source_event);
    http.process_request(request);
    http.process_response(sse_headers(1));
    let refs = Rc::strong_count(&source);
    http.tick(Instant::now() + Duration::from_secs(1));
    assert_eq!(http.active_connections(), 1);
    assert!(http.snapshot_idle_connections(|_, _| None::<()>).is_empty());
    let snapshots = http.snapshot_idle_connections(|_, request| Some(request.body().as_ptr()));
    assert_eq!(snapshots, vec![source.buf.as_ptr()]);
    assert_eq!(Rc::strong_count(&source), refs);
    assert!(http.snapshot_idle_connections(|_, _| Some(())).is_empty());
    http.tick(Instant::now() + Duration::from_secs(11));
    assert_eq!(http.active_connections(), 0);
    reason(&http.take_ready(), RemoveReason::HardTimeout);
    assert_eq!(http.metrics().connection_cache_bytes, 0);
    assert!(http.last_activity.is_empty());
    assert!(http.sse_streams.is_empty());
}

#[test]
fn byte_budget_evicts_oldest_until_all_retained_allocations_fit() {
    let mut http = HttpConnectionAggregator::with_capacity(8);
    let body = vec![b'x'; 4096];
    http.process_request(request(1, &body, body.len()));
    let per_connection = http.metrics().connection_cache_bytes;
    http.bytes_budget = 2 * per_connection;
    http.process_request(request(2, &body, body.len()));
    http.process_request(request(3, &body, body.len()));
    assert_eq!(http.active_connections(), 2);
    assert_eq!(http.metrics().connection_cache_bytes, 2 * per_connection);
    let results = http.take_ready();
    reason(&results, RemoveReason::OverBudget);
    let AggregatedResult::Abandoned { partial, .. } = &results[0] else {
        unreachable!()
    };
    assert!(
        matches!(partial.as_ref(), AggregatedResult::RequestOnly { connection_id, .. } if connection_id.ssl_ptr == 1)
    );
    http.clear();
    assert_eq!(http.metrics().connection_cache_bytes, 0);
}

#[test]
fn fragmented_body_detaches_the_first_capture_and_reserves_once() {
    let mut http = HttpConnectionAggregator::new();
    let request = request(1, b"abc", 1024);
    let weak = Rc::downgrade(&request.source_event);
    http.process_request(request);
    assert!(weak.upgrade().is_none());
    let id = ConnectionId::from_ssl_event(&event(1, 1, b""));
    let Some(ConnectionState::RequestBodyPending { body_buffer, .. }) = http.connections.peek(&id)
    else {
        unreachable!()
    };
    assert_eq!(body_buffer.capacity(), 1024);
    http.process_raw_body_data(&event(1, 1, &vec![b'x'; 1021]));
    let Some(ConnectionState::RequestPending { request }) = http.connections.peek(&id) else {
        unreachable!()
    };
    assert!(request.source_event.buf.is_empty());
    assert_eq!(request.body().len(), 1024);
    assert_eq!(request.body_len, 1024);
    assert_eq!(request.reassembled_body.as_ref().unwrap().capacity(), 1024);
}

#[test]
fn drain_of_a_partial_body_describes_the_buffer_it_keeps() {
    let mut http = HttpConnectionAggregator::new();
    // A declared 100-byte body that never completes: the drain keeps the six
    // bytes it saw, so `body_len` has to describe those six bytes rather than
    // the zero the split-request handoff left behind.
    http.process_request(request(1, b"prompt", 100));
    http.drain_pending(RemoveReason::PidExited);
    let results = http.take_ready();
    let AggregatedResult::Abandoned { partial, .. } = &results[0] else {
        unreachable!()
    };
    let AggregatedResult::RequestOnly { request, .. } = partial.as_ref() else {
        unreachable!()
    };
    assert_eq!(request.body(), b"prompt");
    assert_eq!(request.body_len, request.body().len());
}

#[test]
fn append_limits_emit_bodyless_metadata_immediately() {
    let mut http = HttpConnectionAggregator::with_limits(4, 1024, Duration::from_secs(60));
    let mut request = request(1, b"400\r\n", 0);
    request.headers = HashMap::from([("transfer-encoding".into(), "chunked".into())]);
    http.process_request(request);
    http.process_raw_body_data(&event(1, 1, &vec![b'x'; 2048]));
    let results = http.take_ready();
    reason(&results, RemoveReason::OverBudget);
    let AggregatedResult::Abandoned { partial, .. } = &results[0] else {
        unreachable!()
    };
    let AggregatedResult::RequestOnly { request, .. } = partial.as_ref() else {
        unreachable!()
    };
    assert!(request.body().is_empty());
    assert!(request.source_event.buf.is_empty());
    assert_eq!(http.active_connections(), 0);
    assert_eq!(http.metrics().connection_cache_bytes, 0);
}

// Resource disposal must clear both body views so downstream builders cannot
// recover discarded payload through a stale length or reassembled buffer.
#[test]
fn over_budget_request_keeps_body_len_matching_the_discarded_body() {
    let mut http = HttpConnectionAggregator::with_limits(4, 1024, Duration::from_secs(60));
    let oversized = vec![b'x'; 2048];
    http.process_request(request(1, &oversized, oversized.len()));
    let results = http.take_ready();
    reason(&results, RemoveReason::OverBudget);
    let AggregatedResult::Abandoned { partial, .. } = &results[0] else {
        unreachable!()
    };
    let AggregatedResult::RequestOnly { request, .. } = partial.as_ref() else {
        unreachable!()
    };
    assert!(request.body().is_empty());
    assert_eq!(request.body_len, request.body().len());
    assert_eq!(http.active_connections(), 0);
}

#[test]
fn response_limit_releases_both_bodies_before_queuing_metadata() {
    let mut http = HttpConnectionAggregator::with_limits(4, 1024, Duration::from_secs(60));
    http.process_request(request(1, b"{}", 2));
    let source = event(
        1,
        0,
        b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\nfirst",
    );
    let ParsedHttpMessage::Response(response) = HttpParser::new().parse(source).unwrap() else {
        unreachable!()
    };
    http.process_response(response);
    http.process_raw_body_data(&event(1, 0, &vec![b'x'; 2048]));
    let results = http.take_ready();
    reason(&results, RemoveReason::OverBudget);
    let AggregatedResult::Abandoned { partial, .. } = &results[0] else {
        unreachable!()
    };
    let AggregatedResult::HttpComplete(pair) = partial.as_ref() else {
        unreachable!()
    };
    assert!(pair.response.parsed.body().is_empty());
    assert!(pair.request.body().is_empty());
    assert!(pair.response.parsed.source_event.buf.is_empty());
    assert!(pair.request.source_event.buf.is_empty());
    assert_eq!(http.metrics().connection_cache_bytes, 0);
}

#[test]
fn thirty_two_streams_keep_one_request_copy_and_reduce_tls_history() {
    let mut http = HttpConnectionAggregator::new();
    http.set_compact_sse(true);
    let body = vec![b'x'; 1024 * 1024];
    for id in 1..=32 {
        http.process_request(request(id, &body[..8], body.len()));
        http.process_raw_body_data(&event(id, 1, &body[8..]));
        http.process_response(sse_headers(id));
    }
    let delta = b"data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" x\"}}]}\n\n";
    for _ in 0..100 {
        for id in 1..=32 {
            http.process_raw_body_data(&event(id, 0, delta));
        }
    }
    assert_eq!(http.active_connections(), 32);
    assert!(http.take_ready().is_empty());
    let metrics = http.metrics();
    assert!(metrics.connection_cache_bytes >= 32 * body.len());
    assert!(metrics.connection_cache_bytes < 34 * 1024 * 1024);
    http.drain_pending(RemoveReason::Shutdown);
    let results = http.take_ready();
    assert_eq!(results.len(), 32);
    assert!(results.iter().all(|result| matches!(
        result,
        AggregatedResult::Abandoned {
            reason: RemoveReason::Shutdown,
            ..
        }
    )));
    assert_eq!(http.metrics().connection_cache_bytes, 0);
}

#[test]
fn abandonment_flushes_the_last_captured_sse_line_without_marking_it_complete() {
    for expected in [
        RemoveReason::HardTimeout,
        RemoveReason::PidExited,
        RemoveReason::Shutdown,
    ] {
        let mut http = HttpConnectionAggregator::new();
        http.set_compact_sse(true);
        http.process_request(request(1, b"{}", 2));
        http.process_response(sse_headers(1));
        let mut tail = (*event(1, 0, b"data: {\"choices\":[{\"delta\":{\"content\":\"last fragment\"}}],\"usage\":{\"completion_tokens\":5}}")).clone();
        tail.timestamp_ns = 9000;
        assert!(http.process_raw_body_data(&tail).is_none());
        match expected {
            RemoveReason::HardTimeout => http.tick(Instant::now() + Duration::from_secs(1000)),
            RemoveReason::PidExited => {
                let removed = http.drain_connections_for_pid(std::process::id());
                http.queue_removed(removed);
            }
            _ => http.drain_pending(expected),
        }
        let results = http.take_ready();
        reason(&results, expected);
        let AggregatedResult::Abandoned { partial, .. } = &results[0] else {
            unreachable!()
        };
        let AggregatedResult::SseComplete(pair) = partial.as_ref() else {
            panic!("partial SSE expected")
        };
        assert_eq!(
            pair.response.json_body()[0]["choices"][0]["delta"]["content"],
            "last fragment"
        );
        assert_eq!(
            pair.response.json_body()[0]["usage"]["completion_tokens"],
            5
        );
        assert_eq!(pair.response.captured_sse_event_count, Some(1));
        assert_eq!(pair.response.end_timestamp_ns(), 9000);
        assert_eq!(http.active_connections(), 0);
    }
}

#[test]
fn budget_abandonment_does_not_replay_consumed_sse_lines() {
    for reducer_overflow in [false, true] {
        let first = if reducer_overflow {
            format!("data: {{\"payload\":\"{}\"}}\n\n", "x".repeat(900))
        } else {
            "data: {\"payload\":\"first\"}\n\n".into()
        };
        let mut bytes = first.clone().into_bytes();
        if !reducer_overflow {
            bytes.extend_from_slice(format!("data: {}\n\n", "x".repeat(2048)).as_bytes());
        }
        let source = event(1, 0, &bytes);
        let mut stream = SseStream::new(&sse_headers(1), 1024).unwrap();
        assert_eq!(
            stream.feed(&bytes, &source).unwrap_err().kind(),
            std::io::ErrorKind::OutOfMemory
        );
        let (events, count, _) = stream.into_events();
        assert_eq!(count, 1);
        assert_eq!(events.len(), 1);
        let expected = SseParser::new().parse(event(1, 0, first.as_bytes()));
        assert_eq!(events[0].data(), expected[0].data());
    }
}

#[test]
fn compact_activity_promotes_the_connection_eviction_order() {
    let mut http = HttpConnectionAggregator::with_capacity(2);
    http.set_compact_sse(true);
    for id in 1..=2 {
        http.process_request(request(id, b"{}", 2));
        http.process_response(sse_headers(id));
    }
    let stale = Instant::now() - crate::aggregator::result::ACTIVE_CONNECTION_GRACE;
    for id in 1..=2 {
        http.last_activity
            .put(ConnectionId::from_ssl_event(&event(id, 0, b"")), stale);
    }
    http.process_raw_body_data(&event(
        1,
        0,
        b"data: {\"choices\":[{\"delta\":{\"content\":\"active\"}}]}\n\n",
    ));
    http.process_request(request(3, b"{}", 2));
    assert!(http.is_sse_active(&ConnectionId::from_ssl_event(&event(1, 0, b""))));
    let results = http.take_ready();
    reason(&results, RemoveReason::LruEvicted);
    assert!(
        matches!(&results[0], AggregatedResult::Abandoned { partial, .. }
        if matches!(partial.as_ref(), AggregatedResult::SseComplete(pair) if pair.connection_id.ssl_ptr == 2))
    );
}

#[test]
fn retired_sse_markers_are_bounded_expire_and_follow_pid_cleanup() {
    let mut http = HttpConnectionAggregator::with_capacity(2);
    http.set_compact_sse(true);
    for id in 1..=3 {
        http.process_response(sse_headers(id));
        http.abandon(
            &ConnectionId::from_ssl_event(&event(id, 0, b"")),
            RemoveReason::OverBudget,
        );
    }
    assert_eq!(http.retired_sse.len(), 2);
    http.tick(Instant::now() + Duration::from_secs(301));
    assert!(http.retired_sse.is_empty());
    http.process_response(sse_headers(4));
    http.abandon(
        &ConnectionId::from_ssl_event(&event(4, 0, b"")),
        RemoveReason::OverBudget,
    );
    assert_eq!(http.retired_sse.len(), 1);
    assert!(
        http.drain_connections_for_pid(std::process::id())
            .is_empty()
    );
    assert!(http.retired_sse.is_empty());
}

#[test]
fn incremental_http_accounting_matches_full_scans_through_stream_lifecycles() {
    for compact in [false, true] {
        let mut http = HttpConnectionAggregator::with_capacity(4);
        http.set_compact_sse(compact);
        for id in 1..=4 {
            http.process_request(request(id, b"{}", 2));
            http.process_response(sse_headers(id));
        }
        for _ in 0..20 {
            for id in 1..=4 {
                let source = event(id, 0, b"id: fixture\nevent: message\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"y\"}}]}\n\n");
                if compact {
                    http.process_raw_body_data(&source);
                } else {
                    let id = ConnectionId::from_ssl_event(&source);
                    for event in SseParser::new().parse(source) {
                        http.process_sse_event(&id, event);
                    }
                }
                let pending = http
                    .connections
                    .iter()
                    .map(|(_, state)| state.buffered_bytes())
                    .sum::<usize>();
                let total = pending
                    + http
                        .retained
                        .values()
                        .map(|retained| retained.metadata_bytes())
                        .sum::<usize>()
                    + http
                        .sse_continuation_buffers
                        .iter()
                        .map(|(_, bytes)| bytes.capacity())
                        .sum::<usize>()
                    + http
                        .sse_streams
                        .iter()
                        .map(|(_, stream)| stream.retained_bytes())
                        .sum::<usize>();
                assert_eq!(http.metrics().pending_connection_bytes, pending);
                assert_eq!(http.metrics().connection_cache_bytes, total);
                for (key, state) in http.connections.iter() {
                    let sse = http.sse_streams.peek(key).map_or_else(
                        || {
                            state.sse_retained_bytes()
                                + http
                                    .sse_continuation_buffers
                                    .peek(key)
                                    .map_or(0, Vec::capacity)
                        },
                        |stream| stream.payload_retained_bytes(),
                    );
                    assert_eq!(http.retained[key].sse_bytes, sse);
                }
            }
        }
        http.drain_pending(RemoveReason::Shutdown);
        assert_eq!(http.metrics().connection_cache_bytes, 0);
        assert!(http.retained.is_empty());
    }
}
