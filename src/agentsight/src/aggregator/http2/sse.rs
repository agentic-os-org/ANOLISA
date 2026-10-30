//! HTTP/2 DATA payloads use the same bounded SSE pipeline as HTTP/1.

use super::*;
use crate::aggregator::RemoveReason;
use crate::aggregator::http::SseStream;
use std::rc::Rc;

impl Http2StreamAggregator {
    pub(crate) fn configure_retention(&mut self, limits: &crate::config::RuntimeLimits) {
        self.compact_sse = !limits.retain_raw_sse;
        self.max_sse_bytes = limits.max_sse_retained_bytes.max(1024);
        self.bytes_budget = limits.connection_max_bytes;
    }

    pub(crate) fn set_compact_sse(&mut self, enabled: bool) {
        self.compact_sse = enabled;
    }

    pub(super) fn process_sse_frame(
        &mut self,
        id: StreamId,
        frame: &ParsedHttp2Frame,
    ) -> Option<Option<Http2Stream>> {
        if !self.compact_sse || frame.source_event.rw != 0 || !frame.is_data() {
            return None;
        }
        if !self.sse_streams.contains(&id) {
            let headers = self.decoded_headers_store.get(&id)?.response.as_ref()?;
            if !headers.iter().any(|(key, value)| {
                key.eq_ignore_ascii_case("content-type")
                    && value.to_ascii_lowercase().contains("text/event-stream")
            }) {
                return None;
            }
            let response = crate::parser::http::ParsedResponse {
                version: 2,
                status_code: 200,
                reason: String::new(),
                headers: headers
                    .iter()
                    .filter(|(key, _)| {
                        !matches!(key.as_str(), "content-length" | "transfer-encoding")
                    })
                    .cloned()
                    .collect(),
                body_offset: 0,
                body_len: 0,
                source_event: Rc::new(frame.source_event.metadata_only()),
            };
            let mut stream = match SseStream::new(&response, self.max_sse_bytes) {
                Ok(stream) => stream,
                Err(error) => {
                    log::warn!("Cannot initialize HTTP/2 SSE decoder | stream={id:?}: {error}");
                    return None;
                }
            };
            stream.set_native_incremental(self.native_incremental(id));
            self.sse_streams.put(id, stream);
        }
        let stream = self.sse_streams.get_mut(&id)?;
        let error = stream
            .feed(
                strip_data_padding(frame.payload(), frame.flags),
                &frame.source_event,
            )
            .and_then(|_| {
                if frame.has_end_stream() {
                    stream.finish(&frame.source_event)
                } else {
                    Ok(())
                }
            })
            .err();
        let done = stream.done() || frame.has_end_stream();
        let over_budget = stream.payload_retained_bytes() > self.max_sse_bytes;
        if let Some(error) = error {
            log::warn!("HTTP/2 SSE stream finalized early | stream={id:?}: {error}");
            let reason = if error.kind() == std::io::ErrorKind::OutOfMemory {
                RemoveReason::OverBudget
            } else {
                RemoveReason::ProtocolError
            };
            self.abandon_stream(id, reason);
            return Some(None);
        }
        if over_budget {
            self.abandon_stream(id, RemoveReason::OverBudget);
            return Some(None);
        }
        if done {
            let state = self.streams.pop(&id)?;
            let mut stream = state.into_partial(id);
            stream.response_complete = true;
            stream.end_timestamp_ns = frame.source_event.timestamp_ns;
            let stream = self.finalize_completed(id, stream);
            if frame.has_end_stream() {
                self.closed_streams.pop(&id);
            }
            return Some(Some(stream));
        }
        self.streams.promote(&id);
        self.last_activity.put(id, Instant::now());
        Some(None)
    }

    pub(super) fn finish_sse(&mut self, id: StreamId, stream: &mut Http2Stream) {
        let Some(sse) = self.sse_streams.pop(&id) else {
            return;
        };
        let (events, count, timestamp) = sse.into_events();
        stream.end_timestamp_ns = timestamp;
        stream.captured_sse_event_count = Some(count);
        stream.response_body_decoded = true;
        stream.reduced_sse_events = Some(events);
    }

    fn native_incremental(&self, id: StreamId) -> Option<bool> {
        let headers = self.decoded_headers_store.get(&id)?.request.as_ref()?;
        let path = headers.iter().find(|(name, _)| name == ":path")?.1.as_str();
        if !crate::parser::llm::is_dashscope_native_path(path) {
            return None;
        }
        let frames = match self.streams.peek(&id)? {
            Http2StreamState::WaitingRequestData {
                request_data_frames,
                ..
            }
            | Http2StreamState::RequestComplete {
                request_data_frames,
                ..
            }
            | Http2StreamState::ReceivingResponse {
                request_data_frames,
                ..
            } => request_data_frames,
            Http2StreamState::Complete(stream) => &stream.request_data_frames,
        };
        let bytes: Vec<_> = frames
            .iter()
            .flat_map(|frame| {
                strip_data_padding(frame.payload(), frame.flags)
                    .iter()
                    .copied()
            })
            .collect();
        let encoding = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-encoding"))
            .map(|(_, value)| value.as_str());
        let body = crate::utils::decompress::decompress_body_to_string(&bytes, encoding)?;
        serde_json::from_str::<serde_json::Value>(&body)
            .ok()?
            .pointer("/parameters/incremental_output")?
            .as_bool()
    }
}

impl Http2Stream {
    /// Materialize compact SSE only when a consumer requests body bytes.
    pub(super) fn encode_sse_events(events: &[crate::parser::sse::ParsedSseEvent]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for event in events {
            if let Some(kind) = &event.event {
                bytes.extend_from_slice(b"event: ");
                bytes.extend_from_slice(kind.as_bytes());
                bytes.push(b'\n');
            }
            for line in event.data().split(|byte| *byte == b'\n') {
                bytes.extend_from_slice(b"data: ");
                bytes.extend_from_slice(line);
                bytes.push(b'\n');
            }
            bytes.push(b'\n');
        }
        bytes
    }
}
