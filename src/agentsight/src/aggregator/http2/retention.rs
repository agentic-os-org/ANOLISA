//! HTTP/2 shares the byte budget and emits partial streams on forced removal.

use super::*;
use crate::aggregator::{AggregatedResult, RemoveReason};

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
    pub(super) fn process_closed_frame(
        &mut self,
        frame: &ParsedHttp2Frame,
        id: StreamId,
        direction: StreamDirection,
    ) {
        // SSE can finish before compressed epilogues or END_STREAM arrive.
        // Keep only a bounded marker, while decoding trailers to preserve HPACK.
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
        self.eviction_count = self.eviction_count.saturating_add(1);
        let stream = self.finalize_stream(id, state.into_partial(id));
        self.ready.push(AggregatedResult::Abandoned {
            reason,
            partial: Box::new(AggregatedResult::Http2StreamComplete(stream)),
        });
    }

    pub(crate) fn take_ready(&mut self) -> Vec<AggregatedResult> {
        std::mem::take(&mut self.ready)
    }

    pub(crate) fn oldest_activity(&self) -> Option<Instant> {
        self.last_activity.peek_lru().map(|(_, time)| *time)
    }

    pub(crate) fn abandon_oldest(&mut self) -> bool {
        let Some((id, _)) = self.streams.peek_lru() else {
            if let Some((id, _)) = self.closed_streams.pop_lru() {
                self.discard_side_state(id);
                return true;
            }
            return self.hpack_states.pop_lru().is_some();
        };
        let id = *id;
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
