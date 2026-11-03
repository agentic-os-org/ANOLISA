//! Protect active HTTP/2 SSE while preserving HPACK state and hard memory bounds.

use super::*;
use crate::aggregator::RemoveReason;
use crate::probes::sslsniff::SslEvent;
use std::rc::Rc;

fn frame(id: u32, rw: i32, kind: Http2FrameType, flags: u8, bytes: Vec<u8>) -> ParsedHttp2Frame {
    ParsedHttp2Frame {
        frame_type: kind,
        flags,
        stream_id: id,
        payload_offset: 0,
        payload_len: bytes.len(),
        source_event: Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1,
            delta_ns: 0,
            pid: std::process::id(),
            tid: 1,
            uid: 0,
            len: bytes.len() as u32,
            rw,
            comm: "fixture".into(),
            buf: bytes,
            is_handshake: false,
            ssl_ptr: 1,
        }),
    }
}

fn headers(id: u32, rw: i32) -> ParsedHttp2Frame {
    let mut encoder = hpack::Encoder::new();
    let values = if rw == 1 {
        vec![
            (b":method".as_slice(), b"POST".as_slice()),
            (b":path".as_slice(), b"/v1/messages".as_slice()),
        ]
    } else {
        vec![
            (b":status".as_slice(), b"200".as_slice()),
            (b"content-type".as_slice(), b"text/event-stream".as_slice()),
        ]
    };
    frame(
        id,
        rw,
        Http2FrameType::Headers,
        if rw == 1 { 5 } else { 4 },
        encoder.encode(values),
    )
}

fn activate(aggregator: &mut Http2StreamAggregator, id: u32) {
    assert!(
        aggregator
            .process_frames(vec![headers(id, 1), headers(id, 0)])
            .is_empty()
    );
}

#[test]
fn capacity_skips_active_sse_for_a_pending_request() {
    for compact in [false, true] {
        let mut aggregator = Http2StreamAggregator::with_capacity(2);
        aggregator.set_compact_sse(compact);
        activate(&mut aggregator, 1);
        aggregator.process_frames(vec![headers(3, 1)]);
        let pending = *aggregator
            .streams
            .iter()
            .find(|(id, _)| id.stream_id == 3)
            .unwrap()
            .0;
        aggregator.last_activity.put(
            pending,
            Instant::now() - crate::aggregator::result::ACTIVE_CONNECTION_GRACE,
        );
        aggregator.process_frames(vec![headers(5, 1)]);
        let ready = aggregator.take_ready();
        assert!(
            matches!(&ready[..], [AggregatedResult::Abandoned { reason: RemoveReason::LruEvicted, partial }]
            if matches!(partial.as_ref(), AggregatedResult::Http2StreamComplete(stream) if stream.stream_id.stream_id == 3))
        );
        assert!(aggregator.streams.iter().any(|(id, _)| id.stream_id == 1));
        assert_eq!(aggregator.active_stream_count(), 2);
    }
}

#[test]
fn all_active_streams_reject_admission_and_ignore_tail_without_losing_existing_streams() {
    for compact in [false, true] {
        let mut aggregator = Http2StreamAggregator::with_capacity(2);
        aggregator.set_compact_sse(compact);
        activate(&mut aggregator, 1);
        activate(&mut aggregator, 3);
        aggregator.process_frames(vec![headers(5, 1)]);
        let ready = aggregator.take_ready();
        assert!(
            matches!(&ready[..], [AggregatedResult::Abandoned { reason: RemoveReason::CapacityRejected, partial }]
            if matches!(partial.as_ref(), AggregatedResult::Http2StreamComplete(stream)
                if stream.stream_id.stream_id == 5 && stream.request_body().is_empty()))
        );
        for _ in 0..4 {
            aggregator.process_frames(vec![
                headers(5, 0),
                frame(5, 0, Http2FrameType::Data, 0, b"tail".to_vec()),
            ]);
            assert!(aggregator.take_ready().is_empty());
            assert_eq!(aggregator.active_stream_count(), 2);
        }
        let completed = aggregator.process_frames(vec![frame(
            1,
            0,
            Http2FrameType::Data,
            1,
            b"data: [DONE]\n\n".to_vec(),
        )]);
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].stream_id.stream_id, 1);
        assert!(aggregator.take_ready().is_empty());
    }
}

#[test]
fn grace_boundary_and_hard_byte_reclamation_work_in_both_retention_modes() {
    for compact in [false, true] {
        let mut aggregator = Http2StreamAggregator::with_capacity(1);
        aggregator.set_compact_sse(compact);
        activate(&mut aggregator, 1);
        let id = *aggregator.streams.iter().next().unwrap().0;
        let now = Instant::now();
        aggregator.last_activity.put(id, now);
        let grace = crate::aggregator::result::ACTIVE_CONNECTION_GRACE;
        assert_eq!(
            aggregator.eviction_candidate(now + grace - Duration::from_nanos(1)),
            None
        );
        assert_eq!(aggregator.eviction_candidate(now + grace), Some(id));
        let capture = frame(
            1,
            0,
            Http2FrameType::Data,
            0,
            b"data: {\"payload\":\"partial\"}\n\n".to_vec(),
        );
        let weak = Rc::downgrade(&capture.source_event);
        aggregator.process_frames(vec![capture]);
        assert_eq!(aggregator.eviction_candidate(Instant::now()), None);
        aggregator.bytes_budget = 1;
        aggregator.enforce_bytes_budget();
        let ready = aggregator.take_ready();
        let [
            AggregatedResult::Abandoned {
                reason: RemoveReason::OverBudget,
                partial,
            },
        ] = &ready[..]
        else {
            panic!("expected metadata")
        };
        let AggregatedResult::Http2StreamComplete(stream) = partial.as_ref() else {
            panic!("expected stream metadata")
        };
        assert_eq!(stream.path(), "/v1/messages");
        assert_eq!(stream.status_code(), 200);
        assert_eq!(stream.comm(), "fixture");
        assert!(stream.request_data_frames.is_empty());
        assert!(stream.response_data_frames.is_empty());
        assert!(stream.reduced_sse_events.is_none());
        assert!(
            stream
                .request_headers
                .as_ref()
                .unwrap()
                .source_event
                .buf
                .is_empty()
        );
        assert!(weak.upgrade().is_none());
        assert_eq!(aggregator.active_stream_count(), 0);
        assert_eq!(aggregator.metrics().connection_cache_bytes, 0);
    }
}

#[test]
fn recent_request_and_response_waiting_survive_capacity_churn() {
    for compact in [false, true] {
        for phase in 0..3 {
            let mut aggregator = Http2StreamAggregator::with_capacity(1);
            aggregator.set_compact_sse(compact);
            let mut request = headers(1, 1);
            request.flags = 4;
            aggregator.process_frames(vec![request]);
            if phase > 0 {
                aggregator.process_frames(vec![frame(
                    1,
                    1,
                    Http2FrameType::Data,
                    1,
                    b"{}".to_vec(),
                )]);
            }
            if phase == 2 {
                aggregator.process_frames(vec![headers(1, 0)]);
            }
            let id = *aggregator.streams.iter().next().unwrap().0;
            let now = Instant::now();
            let grace = crate::aggregator::result::ACTIVE_CONNECTION_GRACE;
            aggregator.last_activity.put(id, now);
            assert_eq!(
                aggregator.eviction_candidate(now + grace - Duration::from_nanos(1)),
                None
            );
            assert_eq!(aggregator.eviction_candidate(now + grace), Some(id));
            aggregator.process_frames(vec![headers(3, 1)]);
            assert!(matches!(&aggregator.take_ready()[..],
                [AggregatedResult::Abandoned { reason: RemoveReason::CapacityRejected, partial }]
                if matches!(partial.as_ref(), AggregatedResult::Http2StreamComplete(stream) if stream.stream_id.stream_id == 3)));
            assert!(aggregator.streams.contains(&id));
            if phase == 0 {
                aggregator.process_frames(vec![frame(
                    1,
                    1,
                    Http2FrameType::Data,
                    1,
                    b"{}".to_vec(),
                )]);
            }
            if phase < 2 {
                aggregator.process_frames(vec![headers(1, 0)]);
            }
            let complete = aggregator.process_frames(vec![frame(
                1,
                0,
                Http2FrameType::Data,
                1,
                b"data: [DONE]\n\n".to_vec(),
            )]);
            assert_eq!(complete.len(), 1);
            assert_eq!(complete[0].stream_id, id);
            assert_eq!(complete[0].request_body(), b"{}");
            assert!(aggregator.take_ready().is_empty());
        }
    }
}
