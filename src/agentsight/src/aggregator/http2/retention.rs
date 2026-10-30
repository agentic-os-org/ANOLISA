//! HTTP/2 shares the byte budget and emits partial streams on forced removal.

use super::*;
use crate::aggregator::{AggregatedResult, RemoveReason};

#[derive(Debug, Default)]
struct RetainedFrames {
    count: usize,
    first_source: usize,
    bytes: usize,
}

fn frame_bytes(frame: &ParsedHttp2Frame) -> usize {
    std::mem::size_of::<crate::probes::sslsniff::SslEvent>()
        + frame.source_event.buf.capacity()
        + frame.source_event.comm.capacity()
}

impl RetainedFrames {
    fn bytes(&mut self, frames: &Vec<ParsedHttp2Frame>) -> usize {
        let first_source = frames
            .first()
            .map_or(0, |frame| std::rc::Rc::as_ptr(&frame.source_event) as usize);
        if frames.len() < self.count || self.first_source != first_source {
            *self = Self::default();
        }
        self.bytes += frames[self.count..].iter().map(frame_bytes).sum::<usize>();
        self.count = frames.len();
        self.first_source = first_source;
        self.bytes + frames.capacity() * std::mem::size_of::<ParsedHttp2Frame>()
    }
}

#[derive(Debug, Default)]
/// Incremental frame charges and auxiliary allocations owned by one stream.
pub(super) struct RetainedStream {
    request: RetainedFrames,
    response: RetainedFrames,
    bytes: usize,
    pending_bytes: usize,
    pending: usize,
}

impl RetainedStream {
    /// Charge newly appended frames and current vector capacities.
    pub(super) fn state_bytes(&mut self, state: &Http2StreamState) -> usize {
        match state {
            Http2StreamState::WaitingRequestData {
                request_headers,
                request_data_frames,
            }
            | Http2StreamState::RequestComplete {
                request_headers,
                request_data_frames,
            } => {
                request_headers.as_ref().map_or(0, frame_bytes)
                    + self.request.bytes(request_data_frames)
            }
            Http2StreamState::ReceivingResponse {
                request_headers,
                request_data_frames,
                response_headers,
                response_data_frames,
            } => {
                request_headers.as_ref().map_or(0, frame_bytes)
                    + self.request.bytes(request_data_frames)
                    + response_headers.as_ref().map_or(0, frame_bytes)
                    + self.response.bytes(response_data_frames)
            }
            Http2StreamState::Complete(stream) => {
                stream.request_headers.as_ref().map_or(0, frame_bytes)
                    + self.request.bytes(&stream.request_data_frames)
                    + stream.response_headers.as_ref().map_or(0, frame_bytes)
                    + self.response.bytes(&stream.response_data_frames)
            }
        }
    }
}

impl Http2StreamState {
    pub(super) fn into_partial(self, id: StreamId) -> Http2Stream {
        let (
            request_headers,
            request_data_frames,
            response_headers,
            response_data_frames,
            request_complete,
        ) = match self {
            Self::WaitingRequestData {
                request_headers,
                request_data_frames,
            } => (
                request_headers,
                request_data_frames,
                None,
                Vec::new(),
                false,
            ),
            Self::RequestComplete {
                request_headers,
                request_data_frames,
            } => (request_headers, request_data_frames, None, Vec::new(), true),
            Self::ReceivingResponse {
                request_headers,
                request_data_frames,
                response_headers,
                response_data_frames,
            } => (
                request_headers,
                request_data_frames,
                response_headers,
                response_data_frames,
                true,
            ),
            Self::Complete(stream) => return stream,
        };
        let start = request_headers
            .as_ref()
            .or(request_data_frames.first())
            .or(response_headers.as_ref())
            .or(response_data_frames.first())
            .map_or(0, |frame| frame.source_event.timestamp_ns);
        let end = response_data_frames
            .last()
            .or(response_headers.as_ref())
            .or(request_data_frames.last())
            .map_or(start, |frame| frame.source_event.timestamp_ns);
        let mut stream = Http2Stream::new(id, start);
        stream.request_headers = request_headers;
        stream.request_data_frames = request_data_frames;
        stream.response_headers = response_headers;
        stream.response_data_frames = response_data_frames;
        stream.request_complete = request_complete;
        stream.end_timestamp_ns = end;
        stream
    }
}

impl Http2StreamAggregator {
    /// Reconcile the touched stream and its side caches as one charge.
    pub(super) fn refresh_retained(&mut self, id: StreamId) {
        let mut retained = self.retained.remove(&id).unwrap_or_default();
        self.retained_metrics.connection_cache_bytes = self
            .retained_metrics
            .connection_cache_bytes
            .saturating_sub(retained.bytes);
        self.retained_metrics.pending_connection_bytes = self
            .retained_metrics
            .pending_connection_bytes
            .saturating_sub(retained.pending_bytes);
        self.retained_metrics.pending_connection_count = self
            .retained_metrics
            .pending_connection_count
            .saturating_sub(retained.pending);
        retained.pending = usize::from(self.streams.contains(&id));
        retained.pending_bytes = self
            .streams
            .peek(&id)
            .map_or(0, |state| retained.state_bytes(state));
        retained.bytes = retained.pending_bytes
            + self
                .continuation_buffers
                .get(&id)
                .map_or(0, |buffer| buffer.data.capacity())
            + self
                .decoded_headers_store
                .get(&id)
                .map_or(0, DecodedHeadersPair::buffered_bytes)
            + self
                .sse_streams
                .peek(&id)
                .map_or(0, super::super::http::SseStream::retained_bytes);
        self.retained_metrics.connection_cache_bytes += retained.bytes;
        self.retained_metrics.pending_connection_bytes += retained.pending_bytes;
        self.retained_metrics.pending_connection_count += retained.pending;
        if retained.bytes != 0 || retained.pending != 0 {
            self.retained.insert(id, retained);
        }
    }

    /// Refund the stream including any retained side caches.
    pub(super) fn forget_retained(&mut self, id: StreamId) {
        if let Some(retained) = self.retained.remove(&id) {
            self.retained_metrics.connection_cache_bytes = self
                .retained_metrics
                .connection_cache_bytes
                .saturating_sub(retained.bytes);
            self.retained_metrics.pending_connection_bytes = self
                .retained_metrics
                .pending_connection_bytes
                .saturating_sub(retained.pending_bytes);
            self.retained_metrics.pending_connection_count = self
                .retained_metrics
                .pending_connection_count
                .saturating_sub(retained.pending);
        }
    }

    /// Reconcile decoder charges after SETTINGS, decode, reset, or removal.
    pub(super) fn refresh_hpack(&mut self, id: ConnectionId) {
        let previous = self.retained_hpack.remove(&id).unwrap_or(0);
        let bytes = self
            .hpack_states
            .peek(&id)
            .map_or(0, HpackConnectionState::table_bytes);
        self.retained_metrics.connection_cache_bytes = self
            .retained_metrics
            .connection_cache_bytes
            .saturating_sub(previous)
            + bytes;
        if bytes != 0 {
            self.retained_hpack.insert(id, bytes);
        }
    }

    /// Refund displaced decoders before the LRU admits another connection.
    pub(super) fn hpack_state(&mut self, id: ConnectionId) -> &mut HpackConnectionState {
        if !self.hpack_states.contains(&id)
            && self.hpack_states.len() == self.hpack_states.cap().get()
        {
            if let Some((old, _)) = self.hpack_states.pop_lru() {
                self.refresh_hpack(old);
            }
        }
        self.hpack_states
            .get_or_insert_mut(id, HpackConnectionState::new)
    }

    /// Return cached gauges without rescanning earlier frames or other streams.
    pub(crate) fn metrics(&self) -> ConnectionMetrics {
        ConnectionMetrics {
            eviction_count: self.eviction_count,
            ..self.retained_metrics
        }
    }

    pub(super) fn process_closed_frame(
        &mut self,
        frame: &ParsedHttp2Frame,
        id: StreamId,
        direction: StreamDirection,
    ) {
        // SSE can finish before compressed epilogues or END_STREAM arrive.
        // Keep only a bounded marker, while decoding trailers to preserve HPACK.
        if frame.is_data() {
            metrics::counter!("agentsight_http2_closed_stream_discarded_bytes_total")
                .increment(strip_data_padding(frame.payload(), frame.flags).len() as u64);
        }
        if frame.is_headers() {
            let fragment = frame.header_block_fragment();
            if frame.has_end_headers() {
                self.decode_header_block(id.connection_id, direction, fragment);
            } else if fragment.len() <= MAX_CONTINUATION_BUFFER {
                self.continuation_buffers.insert(
                    id,
                    ContinuationBuffer {
                        data: fragment.to_vec(),
                        direction,
                    },
                );
            } else {
                self.reset_hpack_decoder(id.connection_id, direction);
            }
            if frame.has_end_stream() {
                self.closed_streams.put(id, true);
            }
        } else if frame.frame_type == Http2FrameType::Continuation {
            self.handle_continuation_frame(frame, id, id.connection_id, direction);
            self.decoded_headers_store.remove(&id);
        } else if frame.has_end_stream() || frame.frame_type == Http2FrameType::RstStream {
            self.closed_streams.pop(&id);
            self.discard_side_state(id);
            return;
        }
        if !self.continuation_buffers.contains_key(&id)
            && self.closed_streams.peek(&id) == Some(&true)
        {
            self.closed_streams.pop(&id);
            self.discard_side_state(id);
        }
    }

    pub(crate) fn abandon_pending(&mut self, reason: RemoveReason) {
        let keys: Vec<_> = self.streams.iter().map(|(id, _)| *id).collect();
        for id in keys {
            self.abandon_stream(id, reason);
        }
    }

    pub(super) fn abandon_stream(&mut self, id: StreamId, reason: RemoveReason) {
        if let Some(state) = self.streams.pop(&id) {
            self.emit_abandoned(id, state, reason);
        } else {
            self.discard_side_state(id);
        }
    }

    pub(super) fn emit_abandoned(
        &mut self,
        id: StreamId,
        state: Http2StreamState,
        reason: RemoveReason,
    ) {
        metrics::counter!("agentsight_http_conn_removed_total", "reason" => reason.as_str(), "state" => "http2_stream").increment(1);
        metrics::counter!("agentsight_http_abandoned_emitted_total", "reason" => reason.as_str())
            .increment(1);
        if reason != RemoveReason::CapacityRejected {
            self.eviction_count = self.eviction_count.saturating_add(1);
        }
        let stream = if reason.metadata_only() {
            let mut stream = state.into_partial(id);
            if let Some(pair) = self.decoded_headers_store.remove(&id) {
                stream.decoded_request_headers = pair.request;
                stream.decoded_response_headers = pair.response;
            }
            // Preserve process attribution even when the header frame was missed.
            if stream.request_headers.is_none() {
                stream.request_headers = stream.request_data_frames.first().cloned();
            }
            for frame in [&mut stream.request_headers, &mut stream.response_headers]
                .into_iter()
                .flatten()
            {
                frame.source_event = std::rc::Rc::new(frame.source_event.metadata_only());
                frame.payload_offset = 0;
                frame.payload_len = 0;
            }
            for headers in [
                &mut stream.decoded_request_headers,
                &mut stream.decoded_response_headers,
            ]
            .into_iter()
            .flatten()
            {
                *headers = headers
                    .iter()
                    .filter(|(name, _)| crate::aggregator::result::metadata_header(name))
                    .take(8)
                    .map(|(name, value)| {
                        (
                            name.clone(),
                            crate::aggregator::result::metadata_text(value),
                        )
                    })
                    .collect();
            }
            stream.request_data_frames = Vec::new();
            stream.response_data_frames = Vec::new();
            stream.reduced_sse_events = None;
            self.discard_side_state(id);
            stream
        } else {
            self.finalize_stream(id, state.into_partial(id))
        };
        if matches!(
            reason,
            RemoveReason::LruEvicted
                | RemoveReason::CapacityRejected
                | RemoveReason::OverBudget
                | RemoveReason::ProtocolError
        ) {
            self.retire_stream(id);
        }
        self.ready.push(AggregatedResult::Abandoned {
            reason,
            partial: Box::new(AggregatedResult::Http2StreamComplete(stream)),
        });
    }

    /// Keep tails and trailers on the bounded closed-stream path after finalization.
    pub(super) fn retire_stream(&mut self, id: StreamId) {
        if let Some((old, _)) = self.closed_streams.push(id, false) {
            self.discard_side_state(old);
        }
    }

    pub(crate) fn take_ready(&mut self) -> Vec<AggregatedResult> {
        std::mem::take(&mut self.ready)
    }

    /// Rank unprotected entries ahead of the hard-byte fallback across HTTP versions.
    pub(crate) fn eviction_priority(&self) -> Option<(bool, Instant)> {
        if let Some(id) = self.eviction_candidate(Instant::now()) {
            return self.last_activity.peek(&id).map(|time| (false, *time));
        }
        self.last_activity.peek_lru().map(|(_, time)| (true, *time))
    }

    /// Protect recent traffic before response headers arrive as well as during SSE.
    pub(super) fn eviction_candidate(&self, now: Instant) -> Option<StreamId> {
        self.streams.iter().rev().find_map(|(id, state)| {
            let protected = !matches!(state, Http2StreamState::Complete(_))
                && self.last_activity.peek(id).is_some_and(|time| {
                    now.saturating_duration_since(*time)
                        < crate::aggregator::result::ACTIVE_CONNECTION_GRACE
                });
            (!protected).then_some(*id)
        })
    }

    pub(crate) fn abandon_oldest(&mut self) -> bool {
        let Some(id) = self
            .eviction_candidate(Instant::now())
            .or_else(|| self.streams.peek_lru().map(|(id, _)| *id))
        else {
            if let Some((id, _)) = self.closed_streams.pop_lru() {
                self.discard_side_state(id);
                return true;
            }
            return if let Some((id, _)) = self.hpack_states.pop_lru() {
                self.refresh_hpack(id);
                true
            } else {
                false
            };
        };
        self.abandon_stream(id, RemoveReason::OverBudget);
        true
    }

    pub(super) fn enforce_bytes_budget(&mut self) {
        while self.bytes_budget != 0 && self.metrics().connection_cache_bytes > self.bytes_budget {
            if !self.abandon_oldest() {
                break;
            }
        }
    }

    pub(crate) fn drain_for_pid(&mut self, pid: u32) {
        let keys: Vec<_> = self
            .streams
            .iter()
            .filter_map(|(id, _)| (id.connection_id.pid == pid).then_some(*id))
            .collect();
        for id in keys {
            self.abandon_stream(id, RemoveReason::PidExited);
        }
        let closed: Vec<_> = self
            .closed_streams
            .iter()
            .filter_map(|(id, _)| (id.connection_id.pid == pid).then_some(*id))
            .collect();
        for id in closed {
            self.closed_streams.pop(&id);
            self.discard_side_state(id);
        }
        let connections: Vec<_> = self
            .hpack_states
            .iter()
            .filter_map(|(id, _)| (id.pid == pid).then_some(*id))
            .collect();
        for id in connections {
            self.hpack_states.pop(&id);
            self.refresh_hpack(id);
        }
    }

    pub(crate) fn drain_dead_pids(&mut self) {
        let pids: std::collections::HashSet<_> = self
            .streams
            .iter()
            .map(|(id, _)| id.connection_id.pid)
            .chain(self.hpack_states.iter().map(|(id, _)| id.pid))
            .collect();
        for pid in pids {
            if !crate::utils::procfs::proc_pid(pid).exists() {
                self.drain_for_pid(pid);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probes::sslsniff::SslEvent;
    use std::rc::Rc;

    fn frame(
        id: u32,
        rw: i32,
        kind: Http2FrameType,
        flags: u8,
        bytes: Vec<u8>,
    ) -> ParsedHttp2Frame {
        let len = bytes.len();
        ParsedHttp2Frame {
            frame_type: kind,
            flags,
            stream_id: id,
            payload_offset: 0,
            payload_len: len,
            source_event: Rc::new(SslEvent {
                source: 0,
                timestamp_ns: 1,
                delta_ns: 0,
                pid: std::process::id(),
                tid: 1,
                uid: 0,
                len: len as u32,
                rw,
                comm: "fixture".into(),
                buf: bytes,
                is_handshake: false,
                ssl_ptr: 1,
            }),
        }
    }

    fn assert_accounting(aggregator: &Http2StreamAggregator) {
        let pending = aggregator
            .streams
            .iter()
            .map(|(_, state)| state.buffered_bytes())
            .sum::<usize>();
        let total = pending
            + aggregator
                .continuation_buffers
                .values()
                .map(|buffer| buffer.data.capacity())
                .sum::<usize>()
            + aggregator
                .decoded_headers_store
                .values()
                .map(DecodedHeadersPair::buffered_bytes)
                .sum::<usize>()
            + aggregator
                .hpack_states
                .iter()
                .map(|(_, state)| state.table_bytes())
                .sum::<usize>()
            + aggregator
                .sse_streams
                .iter()
                .map(|(_, stream)| stream.retained_bytes())
                .sum::<usize>();
        assert_eq!(
            aggregator.metrics().pending_connection_count,
            aggregator.streams.len()
        );
        assert_eq!(aggregator.metrics().pending_connection_bytes, pending);
        assert_eq!(aggregator.metrics().connection_cache_bytes, total);
    }

    #[test]
    fn incremental_http2_accounting_matches_frames_headers_and_pid_drain() {
        let mut aggregator = Http2StreamAggregator::with_capacity(4);
        let mut encoder = hpack::Encoder::new();
        for id in 1..=4 {
            let headers = encoder.encode([
                (b":method".as_slice(), b"POST".as_slice()),
                (b":path".as_slice(), b"/v1/messages".as_slice()),
            ]);
            aggregator.process_frames(vec![frame(id, 1, Http2FrameType::Headers, 4, headers)]);
            assert_accounting(&aggregator);
        }
        for _ in 0..20 {
            aggregator.process_frames(
                (1..=4)
                    .map(|id| frame(id, 1, Http2FrameType::Data, 0, vec![b'x'; 256]))
                    .collect(),
            );
            assert_accounting(&aggregator);
        }
        aggregator.drain_for_pid(std::process::id());
        assert_accounting(&aggregator);
        assert_eq!(aggregator.metrics().connection_cache_bytes, 0);
        assert!(aggregator.retained.is_empty());
        assert!(aggregator.retained_hpack.is_empty());
    }

    #[test]
    fn http2_compact_native_uses_the_request_incremental_mode() {
        for (incremental, expected) in [(false, "aa"), (true, "aaa")] {
            let mut aggregator = Http2StreamAggregator::new();
            aggregator.set_compact_sse(true);
            let mut encoder = hpack::Encoder::new();
            let request = encoder.encode([
                (b":method".as_slice(), b"POST".as_slice()),
                (
                    b":path".as_slice(),
                    b"/api/v1/services/aigc/text-generation/generation".as_slice(),
                ),
            ]);
            aggregator.process_frames(vec![
                frame(1, 1, Http2FrameType::Headers, 4, request),
                frame(
                    1,
                    1,
                    Http2FrameType::Data,
                    1,
                    serde_json::json!({"parameters":{"incremental_output":incremental}})
                        .to_string()
                        .into_bytes(),
                ),
            ]);
            let mut encoder = hpack::Encoder::new();
            let response = encoder.encode([
                (b":status".as_slice(), b"200".as_slice()),
                (b"content-type".as_slice(), b"text/event-stream".as_slice()),
            ]);
            aggregator.process_frames(vec![frame(1, 0, Http2FrameType::Headers, 4, response)]);
            assert!(
                aggregator
                    .process_frames(vec![frame(
                        1,
                        0,
                        Http2FrameType::Data,
                        0,
                        b"data: {\"output\":{\"text\":\"a\",\"finish_reason\":\"null\"}}\n\n"
                            .to_vec()
                    )])
                    .is_empty()
            );
            let completed = aggregator.process_frames(vec![frame(
                1,
                0,
                Http2FrameType::Data,
                0,
                b"data: {\"output\":{\"text\":\"aa\",\"finish_reason\":\"stop\"}}\n\n".to_vec(),
            )]);
            assert_eq!(completed.len(), 1);
            assert_eq!(
                completed[0].response_sse_json_array().unwrap()[0]["output"]["text"],
                expected
            );
            assert_accounting(&aggregator);
        }
    }

    #[test]
    fn over_budget_http2_tails_cannot_restart_a_retired_stream() {
        let mut aggregator = Http2StreamAggregator::with_limits(4, 1024, Duration::from_secs(60));
        aggregator.process_frames(vec![frame(1, 1, Http2FrameType::Data, 0, vec![b'x'; 2048])]);
        let abandoned = aggregator.take_ready();
        assert!(matches!(
            &abandoned[..],
            [AggregatedResult::Abandoned {
                reason: RemoveReason::OverBudget,
                ..
            }]
        ));
        for _ in 0..8 {
            assert!(
                aggregator
                    .process_frames(vec![frame(1, 0, Http2FrameType::Data, 0, b"late".to_vec())])
                    .is_empty()
            );
            assert!(aggregator.take_ready().is_empty());
            assert_eq!(aggregator.active_stream_count(), 0);
            assert_accounting(&aggregator);
        }
        aggregator.process_frames(vec![frame(1, 0, Http2FrameType::Data, 1, Vec::new())]);
        assert!(aggregator.closed_streams.is_empty());
    }
}
