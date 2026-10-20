//! Capacity protection must preserve live SSE without bypassing hard byte limits.

use super::*;
use crate::aggregator::RemoveReason;
use std::collections::HashMap;

fn source(id: u64, rw: i32, body: &[u8]) -> Rc<SslEvent> {
    Rc::new(SslEvent {
        source: 0,
        timestamp_ns: 1,
        delta_ns: 0,
        pid: std::process::id(),
        tid: 1,
        uid: 0,
        len: body.len() as u32,
        rw,
        comm: "fixture".into(),
        buf: body.to_vec(),
        is_handshake: false,
        ssl_ptr: id,
    })
}

fn request(id: u64, body: &[u8]) -> ParsedRequest {
    ParsedRequest {
        method: "POST".into(),
        path: "/v1/chat/completions".into(),
        version: 11,
        headers: HashMap::new(),
        body_offset: 0,
        body_len: body.len(),
        source_event: source(id, 1, body),
        reassembled_body: None,
    }
}

fn activate(http: &mut HttpConnectionAggregator, id: u64) {
    http.process_request(request(id, b"{}"));
    http.process_response(ParsedResponse {
        version: 11,
        status_code: 200,
        reason: "OK".into(),
        headers: HashMap::from([("content-type".into(), "text/event-stream".into())]),
        body_offset: 0,
        body_len: 0,
        source_event: source(id, 0, b""),
    });
}

fn headers(id: u64, sse: bool) -> ParsedResponse {
    ParsedResponse {
        version: 11,
        status_code: if sse { 200 } else { 204 },
        reason: "fixture".into(),
        headers: if sse {
            HashMap::from([("content-type".into(), "text/event-stream".into())])
        } else {
            HashMap::new()
        },
        body_offset: 0,
        body_len: 0,
        source_event: source(id, 0, b""),
    }
}

fn capacity_rejected_fixture(compact: bool) -> HttpConnectionAggregator {
    let mut http = HttpConnectionAggregator::with_capacity(1);
    http.set_compact_sse(compact);
    activate(&mut http, 1);
    http.process_request(request(2, b"rejected"));
    assert!(matches!(
        &http.take_ready()[..],
        [AggregatedResult::Abandoned {
            reason: RemoveReason::CapacityRejected,
            ..
        }]
    ));
    http
}

#[test]
fn capacity_rejected_response_headers_do_not_repeat_the_diagnostic() {
    for compact in [false, true] {
        for sse in [false, true] {
            let mut http = capacity_rejected_fixture(compact);
            let rejected = ConnectionId::from_ssl_event(&source(2, 0, b""));
            assert!(http.process_response(headers(2, sse)).is_none());
            assert!(http.take_ready().is_empty());
            assert!(matches!(
                http.retired_sse.peek(&rejected),
                Some((_, RemoveReason::CapacityRejected))
            ));
            assert!(!http.connections.contains(&rejected));
            assert_eq!(http.active_connections(), 1);
            assert!(http.is_sse_active(&ConnectionId::from_ssl_event(&source(1, 0, b""))));
            assert!(
                http.process_raw_body_data(&source(2, 0, b"data: [DONE]\n\n"))
                    .is_none()
            );
            assert!(http.take_ready().is_empty());
        }
    }
}

#[test]
fn capacity_rejected_response_headers_stay_suppressed_when_capacity_is_freed() {
    for compact in [false, true] {
        for sse in [false, true] {
            let mut http = capacity_rejected_fixture(compact);
            let active = ConnectionId::from_ssl_event(&source(1, 0, b""));
            let rejected = ConnectionId::from_ssl_event(&source(2, 0, b""));
            http.abandon(&active, RemoveReason::Shutdown);
            drop(http.take_ready());
            assert_eq!(http.active_connections(), 0);
            assert!(http.process_response(headers(2, sse)).is_none());
            assert!(http.take_ready().is_empty());
            assert!(http.retired_sse.contains(&rejected));
            assert_eq!(http.active_connections(), 0);
            assert_eq!(http.metrics().connection_cache_bytes, 0);
        }
    }
}

#[test]
fn new_request_clears_capacity_rejection_and_pairs_its_response() {
    for compact in [false, true] {
        let mut http = capacity_rejected_fixture(compact);
        let active = ConnectionId::from_ssl_event(&source(1, 0, b""));
        let returning = ConnectionId::from_ssl_event(&source(2, 1, b""));
        http.abandon(&active, RemoveReason::Shutdown);
        drop(http.take_ready());
        http.process_request(request(2, b"next"));
        assert!(!http.retired_sse.contains(&returning));
        assert!(http.has_pending_request(&returning));
        let Some(AggregatedResult::HttpComplete(pair)) = http.process_response(headers(2, false))
        else {
            panic!("the new exchange did not complete")
        };
        assert_eq!(pair.connection_id, returning);
        assert_eq!(pair.request.body(), b"next");
        assert!(http.take_ready().is_empty());
    }
}

#[test]
fn response_headers_clear_retirement_markers_for_other_reasons() {
    for compact in [false, true] {
        for reason in [
            RemoveReason::LruEvicted,
            RemoveReason::OverBudget,
            RemoveReason::ProtocolError,
        ] {
            let mut http = HttpConnectionAggregator::with_capacity(1);
            http.set_compact_sse(compact);
            let id = ConnectionId::from_ssl_event(&source(1, 0, b""));
            http.retired_sse.put(id, (Instant::now(), reason));
            assert!(matches!(
                http.process_response(headers(1, false)),
                Some(AggregatedResult::ResponseOnly { connection_id, .. }) if connection_id == id
            ));
            assert!(!http.retired_sse.contains(&id));
            assert!(http.take_ready().is_empty());
        }
    }
}

#[test]
fn active_sse_is_spared_even_when_it_is_the_lru_entry() {
    for compact in [false, true] {
        let mut http = HttpConnectionAggregator::with_capacity(2);
        http.set_compact_sse(compact);
        activate(&mut http, 1);
        http.process_request(request(2, b"pending"));
        http.last_activity.put(
            ConnectionId::from_ssl_event(&source(2, 1, b"")),
            Instant::now() - crate::aggregator::result::ACTIVE_CONNECTION_GRACE,
        );
        http.process_request(request(3, b"new"));
        let ready = http.take_ready();
        assert!(
            matches!(&ready[..], [AggregatedResult::Abandoned {reason: RemoveReason::LruEvicted, partial}]
            if matches!(partial.as_ref(), AggregatedResult::RequestOnly { connection_id, request }
                if connection_id.ssl_ptr == 2 && request.body().is_empty()))
        );
        assert!(http.is_sse_active(&ConnectionId::from_ssl_event(&source(1, 0, b""))));
        assert_eq!(http.active_connections(), 2);
    }
}

#[test]
fn grace_expires_at_the_boundary_and_activity_renews_it() {
    let mut http = HttpConnectionAggregator::with_capacity(1);
    http.set_compact_sse(true);
    activate(&mut http, 1);
    let id = ConnectionId::from_ssl_event(&source(1, 0, b""));
    let now = Instant::now();
    let grace = crate::aggregator::result::ACTIVE_CONNECTION_GRACE;
    http.last_activity.put(id, now);
    assert_eq!(
        http.eviction_candidate(now + grace - Duration::from_nanos(1)),
        None
    );
    assert_eq!(http.eviction_candidate(now + grace), Some(id));
    http.last_activity.put(id, now - grace);
    http.process_raw_body_data(&source(1, 0, b": heartbeat\n\n"));
    http.process_request(request(2, b"new"));
    assert!(matches!(
        &http.take_ready()[..],
        [AggregatedResult::Abandoned {
            reason: RemoveReason::CapacityRejected,
            ..
        }]
    ));
    assert!(http.is_sse_active(&id));
}

#[test]
fn hard_bytes_still_remove_protected_sse_and_release_all_payloads() {
    for compact in [false, true] {
        let mut http = HttpConnectionAggregator::with_capacity(1);
        http.set_compact_sse(compact);
        activate(&mut http, 1);
        let capture = source(1, 0, b"data: {\"payload\":\"partial\"}\n\n");
        let weak = Rc::downgrade(&capture);
        if compact {
            http.process_raw_body_data(&capture);
        } else {
            let id = ConnectionId::from_ssl_event(&capture);
            for event in SseParser::new().parse(Rc::clone(&capture)) {
                http.process_sse_event(&id, event);
            }
        }
        drop(capture);
        http.bytes_budget = 1;
        http.enforce_limits();
        let ready = http.take_ready();
        let [
            AggregatedResult::Abandoned {
                reason: RemoveReason::OverBudget,
                partial,
            },
        ] = &ready[..]
        else {
            panic!("expected byte-pressure metadata")
        };
        let AggregatedResult::SseComplete(pair) = partial.as_ref() else {
            panic!("expected SSE metadata")
        };
        assert!(pair.request.body().is_empty());
        assert!(pair.response.sse_events.is_empty());
        assert!(pair.response.sse_continuation_bytes.is_none());
        assert!(weak.upgrade().is_none());
        assert_eq!(http.metrics().connection_cache_bytes, 0);
        assert_eq!(http.active_connections(), 0);
    }
}

#[test]
fn metadata_is_bounded_utf8_and_does_not_keep_authorization_or_body() {
    let mut http = HttpConnectionAggregator::with_capacity(1);
    let mut old = request(1, &vec![b'x'; 1024 * 1024]);
    let weak = Rc::downgrade(&old.source_event);
    old.path = "汉".repeat(1024);
    old.headers = HashMap::from([
        ("x-request-id".into(), "🙂".repeat(1024)),
        ("authorization".into(), "secret".into()),
        ("host".into(), "fixture.test".into()),
    ]);
    http.process_request(old);
    http.last_activity.put(
        ConnectionId::from_ssl_event(&source(1, 1, b"")),
        Instant::now() - crate::aggregator::result::ACTIVE_CONNECTION_GRACE,
    );
    http.process_request(request(2, b"{}"));
    assert!(weak.upgrade().is_none());
    let ready = http.take_ready();
    let [AggregatedResult::Abandoned { partial, .. }] = &ready[..] else {
        panic!("expected one diagnostic")
    };
    let AggregatedResult::RequestOnly { request, .. } = partial.as_ref() else {
        panic!("expected request metadata")
    };
    assert!(request.path.len() <= 2048);
    assert_eq!(request.headers["x-request-id"].len(), 2048);
    assert!(!request.headers.contains_key("authorization"));
    assert_eq!(request.headers["host"], "fixture.test");
    assert!(request.body().is_empty());
}

#[test]
fn recent_request_and_response_waiting_survive_capacity_churn() {
    for compact in [false, true] {
        for phase in 0..3 {
            let mut http = HttpConnectionAggregator::with_capacity(1);
            http.set_compact_sse(compact);
            let mut original = request(1, if phase == 1 { b"{" } else { b"{}" });
            original.headers.insert("content-length".into(), "2".into());
            http.process_request(original);
            if phase == 2 {
                http.process_response(ParsedResponse {
                    version: 11,
                    status_code: 200,
                    reason: "OK".into(),
                    headers: HashMap::from([("content-length".into(), "2".into())]),
                    body_offset: 0,
                    body_len: 1,
                    source_event: source(1, 0, b"{"),
                });
            }
            let id = ConnectionId::from_ssl_event(&source(1, 1, b""));
            let now = Instant::now();
            let grace = crate::aggregator::result::ACTIVE_CONNECTION_GRACE;
            http.last_activity.put(id, now);
            assert_eq!(
                http.eviction_candidate(now + grace - Duration::from_nanos(1)),
                None
            );
            assert_eq!(http.eviction_candidate(now + grace), Some(id));
            http.process_request(request(2, b"churn"));
            assert!(matches!(&http.take_ready()[..],
                [AggregatedResult::Abandoned { reason: RemoveReason::CapacityRejected, partial }]
                if matches!(partial.as_ref(), AggregatedResult::RequestOnly {connection_id, ..} if connection_id.ssl_ptr == 2)));
            assert_eq!(http.active_connections(), 1);
            assert!(http.connections.contains(&id));
            if phase == 2 {
                let result = http.process_raw_body_data(&source(1, 0, b"}")).unwrap();
                let AggregatedResult::HttpComplete(pair) = result else {
                    panic!("response was lost")
                };
                assert_eq!(pair.request.body(), b"{}");
                assert_eq!(pair.response.parsed.body(), b"{}");
            } else {
                if phase == 1 {
                    http.process_raw_body_data(&source(1, 1, b"}"));
                }
                http.process_response(ParsedResponse {
                    version: 11,
                    status_code: 200,
                    reason: "OK".into(),
                    headers: HashMap::from([("content-type".into(), "text/event-stream".into())]),
                    body_offset: 0,
                    body_len: 0,
                    source_event: source(1, 0, b""),
                });
                let result = if compact {
                    http.process_raw_body_data(&source(1, 0, b"data: [DONE]\n\n"))
                } else {
                    http.process_sse_event(
                        &id,
                        SseParser::new()
                            .parse(source(1, 0, b"data: [DONE]\n\n"))
                            .pop()
                            .unwrap(),
                    )
                }
                .unwrap();
                let AggregatedResult::SseComplete(pair) = result else {
                    panic!("request was lost")
                };
                assert_eq!(pair.request.body(), b"{}");
            }
            assert!(http.take_ready().is_empty());
        }
    }
}

#[test]
fn keep_alive_slot_survives_churn_between_completed_response_and_next_request() {
    for compact in [false, true] {
        let mut http = HttpConnectionAggregator::with_capacity(1);
        http.set_compact_sse(compact);
        let id = ConnectionId::from_ssl_event(&source(1, 1, b""));
        for _ in 0..8 {
            activate(&mut http, 1);
            let done = source(1, 0, b"data: [DONE]\n\n");
            let result = if compact {
                http.process_raw_body_data(&done)
            } else {
                http.process_sse_event(&id, SseParser::new().parse(done).pop().unwrap())
            };
            assert!(matches!(result, Some(AggregatedResult::SseComplete(_))));
            drop(result);
            assert!(!http.has_pending());
            assert_eq!(http.metrics().connection_cache_bytes, 0);
            assert_eq!(http.recently_completed.len(), 1);
            assert!(http.last_activity.is_empty());
            assert!(http.sse_streams.is_empty());

            // The newcomer arrives before the next request on the existing TLS connection.
            http.process_request(request(2, b"churn"));
            assert!(matches!(
                &http.take_ready()[..],
                [AggregatedResult::Abandoned {
                    reason: RemoveReason::CapacityRejected,
                    ..
                }]
            ));
            assert_eq!(http.connections.len() + http.recently_completed.len(), 1);
        }
        http.process_request(request(1, b"next"));
        assert!(http.has_pending_request(&id));
        assert!(http.recently_completed.is_empty());
        assert!(http.take_ready().is_empty());
    }
}

#[test]
fn completed_slots_expire_without_activity_and_do_not_retain_capture_buffers() {
    let mut http = HttpConnectionAggregator::with_capacity(1);
    let original = request(1, b"payload");
    let weak = Rc::downgrade(&original.source_event);
    http.process_request(original);
    let result = http.process_response(ParsedResponse {
        version: 11,
        status_code: 204,
        reason: "No Content".into(),
        headers: HashMap::new(),
        body_offset: 0,
        body_len: 0,
        source_event: source(1, 0, b""),
    });
    assert!(matches!(result, Some(AggregatedResult::HttpComplete(_))));
    drop(result);
    assert!(weak.upgrade().is_none());
    let id = ConnectionId::from_ssl_event(&source(1, 1, b""));
    let completed = *http.recently_completed.peek(&id).unwrap();
    // Cleanup can be called both by the HTTP aggregator and the unified dispatcher.
    http.cleanup_completed(&id);
    assert_eq!(http.recently_completed.peek(&id), Some(&completed));
    let grace = crate::aggregator::result::ACTIVE_CONNECTION_GRACE;
    http.tick(completed + grace - Duration::from_nanos(1));
    assert_eq!(http.recently_completed.len(), 1);
    http.tick(completed + grace);
    assert!(http.recently_completed.is_empty());
    http.process_request(request(2, b"new"));
    assert!(http.take_ready().is_empty());
    assert_eq!(http.active_connections(), 1);
}

#[test]
fn completed_reservations_follow_pid_cleanup_and_clear() {
    for dead_pid in [false, true] {
        let mut http = HttpConnectionAggregator::with_capacity(1);
        let mut id = ConnectionId::from_ssl_event(&source(1, 1, b""));
        if dead_pid {
            id.pid = u32::MAX;
        }
        http.recently_completed.put(id, Instant::now());
        if dead_pid {
            assert!(http.drain_dead_pid_connections().is_empty());
        } else {
            assert!(http.drain_connections_for_pid(id.pid).is_empty());
        }
        assert!(http.recently_completed.is_empty());
        http.recently_completed.put(id, Instant::now());
        http.clear();
        assert!(http.recently_completed.is_empty());
    }
}

#[test]
fn admission_expires_completed_slots_without_a_maintenance_tick() {
    let mut http = HttpConnectionAggregator::with_capacity(2);
    let id = ConnectionId::from_ssl_event(&source(1, 1, b""));
    http.recently_completed.put(
        id,
        Instant::now() - crate::aggregator::result::ACTIVE_CONNECTION_GRACE,
    );
    activate(&mut http, 2);
    http.process_request(request(3, b"new"));
    assert!(http.recently_completed.is_empty());
    assert_eq!(http.active_connections(), 2);
    assert!(http.take_ready().is_empty());
}

#[test]
fn keep_alive_reservations_do_not_prevent_hard_byte_reclamation() {
    let mut http = HttpConnectionAggregator::with_capacity(2);
    let id = ConnectionId::from_ssl_event(&source(1, 1, b""));
    http.recently_completed.put(id, Instant::now());
    activate(&mut http, 2);
    http.bytes_budget = 1;
    http.enforce_limits();
    assert!(matches!(
        &http.take_ready()[..],
        [AggregatedResult::Abandoned {
            reason: RemoveReason::OverBudget,
            ..
        }]
    ));
    assert!(!http.has_pending());
    assert_eq!(http.metrics().connection_cache_bytes, 0);
    assert_eq!(http.recently_completed.len(), 1);
}
