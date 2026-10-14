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
            let stream = match SseStream::new(&response, self.max_sse_bytes) {
                Ok(stream) => stream,
                Err(error) => {
                    log::warn!("Cannot initialize HTTP/2 SSE decoder | stream={id:?}: {error}");
                    return None;
                }
            };
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
        for event in events {
            let mut source = event.source_event().metadata_only();
            if let Some(kind) = &event.event {
                source.buf.extend_from_slice(b"event: ");
                source.buf.extend_from_slice(kind.as_bytes());
                source.buf.push(b'\n');
            }
            for line in event.data().split(|byte| *byte == b'\n') {
                source.buf.extend_from_slice(b"data: ");
                source.buf.extend_from_slice(line);
                source.buf.push(b'\n');
            }
            source.buf.push(b'\n');
            source.len = source.buf.len() as u32;
            let len = source.buf.len();
            stream.response_data_frames.push(ParsedHttp2Frame {
                frame_type: Http2FrameType::Data,
                flags: 0,
                stream_id: id.stream_id,
                payload_offset: 0,
                payload_len: len,
                source_event: Rc::new(source),
            });
        }
    }
}
