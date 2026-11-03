//! Compact SSE state is selected for semantic-only FFI delivery.

use super::super::sse_stream::SseStream;
use super::{ConnectionId, ConnectionState, HttpConnectionAggregator};
use crate::aggregator::{AggregatedResult, RemoveReason};
use crate::parser::http::ParsedResponse;
use crate::parser::sse::ParsedSseEvent;
use crate::probes::sslsniff::SslEvent;
use std::cell::RefCell;
use std::rc::Rc;

impl HttpConnectionAggregator {
    /// Ignore tails of explicitly abandoned streams until a new exchange begins.
    pub(crate) fn discard_retired_sse(&mut self, event: &SslEvent) -> bool {
        if event.rw != 0 {
            return false;
        }
        let id = ConnectionId::from_ssl_event(event);
        let bytes = &event.buf[..event.buf_size() as usize];
        if self
            .retired_sse
            .peek(&id)
            .is_some_and(|(_, reason)| *reason == RemoveReason::CapacityRejected)
        {
            metrics::counter!("agentsight_http_sse_discarded_bytes_total", "reason" => "capacity_rejected")
                .increment(bytes.len() as u64);
            return true;
        }
        // A fresh HTTP response is a new exchange, even if its request was missed.
        if bytes.starts_with(b"HTTP/1.") {
            return false;
        }
        let Some((_, reason)) = self.retired_sse.get(&id) else {
            return false;
        };
        metrics::counter!("agentsight_http_sse_discarded_bytes_total", "reason" => reason.as_str())
            .increment(bytes.len() as u64);
        true
    }

    pub(crate) fn set_compact_sse(&mut self, enabled: bool) {
        self.compact_sse = enabled;
    }

    pub(super) fn sse_entry_state_for_connection(
        &mut self,
        id: ConnectionId,
        response: &ParsedResponse,
        request: Option<&crate::parser::http::ParsedRequest>,
    ) -> (
        Vec<ParsedSseEvent>,
        Option<Vec<u8>>,
        Option<String>,
        Option<Rc<RefCell<crate::utils::decompress::ZstdStreamDecoder>>>,
    ) {
        if !self.compact_sse {
            return Self::sse_entry_state(response);
        }
        match SseStream::new(response, self.max_sse_retained_bytes.max(1024)) {
            Ok(mut stream) => {
                if let Some(request) = request
                    .filter(|request| crate::parser::llm::is_dashscope_native_path(&request.path))
                {
                    stream.set_native_incremental(request.json_body().and_then(|value| {
                        value
                            .pointer("/parameters/incremental_output")
                            .and_then(serde_json::Value::as_bool)
                    }));
                }
                let error = stream.feed(response.body(), &response.source_event).err();
                let done = error.is_none() && stream.done();
                self.sse_streams.put(id, stream);
                if let Some(error) = error {
                    log::warn!("Initial SSE fragment cannot be decoded | conn={id:?}: {error}");
                    self.sse_errors.put(id, reason(&error));
                }
                let marker = done.then(|| {
                    ParsedSseEvent::new_done_marker(Rc::new(response.source_event.metadata_only()))
                });
                (
                    marker.into_iter().collect(),
                    None,
                    response.content_encoding().map(str::to_owned),
                    None,
                )
            }
            Err(error) => {
                log::warn!("Streaming SSE decoder unavailable | conn={id:?}: {error}");
                Self::sse_entry_state(response)
            }
        }
    }

    pub(crate) fn accepts_compact_sse_bytes(&self, event: &SslEvent) -> bool {
        event.rw == 0
            && self
                .sse_streams
                .contains(&ConnectionId::from_ssl_event(event))
    }

    pub(crate) fn accepts_unmatched_sse(&self, event: &SslEvent) -> bool {
        event.rw == 0
            && !self
                .connections
                .contains(&ConnectionId::from_ssl_event(event))
    }

    pub(crate) fn start_unmatched_sse(&mut self, event: &SslEvent) -> Option<AggregatedResult> {
        let bytes = &event.buf[..event.buf_size() as usize];
        let mut headers =
            std::collections::HashMap::from([("content-type".into(), "text/event-stream".into())]);
        let chunked = bytes
            .windows(2)
            .position(|pair| pair == b"\r\n")
            .and_then(|end| std::str::from_utf8(&bytes[..end]).ok())
            .is_some_and(|line| {
                usize::from_str_radix(line.split(';').next().unwrap_or(""), 16).is_ok()
            });
        if chunked {
            headers.insert("transfer-encoding".into(), "chunked".into());
        }
        self.process_response(ParsedResponse {
            version: 11,
            status_code: 0,
            reason: String::new(),
            headers,
            body_offset: 0,
            body_len: bytes.len(),
            source_event: Rc::new(event.clone()),
        })
    }

    pub(super) fn process_compact_sse_bytes(
        &mut self,
        event: &SslEvent,
    ) -> Option<AggregatedResult> {
        let id = ConnectionId::from_ssl_event(event);
        self.touch(&id);
        let stream = self.sse_streams.get_mut(&id)?;
        let error = stream
            .feed(&event.buf[..event.buf_size() as usize], event)
            .err();
        let done = error.is_none() && stream.done();
        if let Some(error) = error {
            log::warn!("SSE stream finalized early | conn={id:?}: {error}");
            self.abandon(&id, reason(&error));
        } else if done {
            if let Some(ConnectionState::SseActive {
                request,
                response_headers,
                ..
            }) = self.connections.pop(&id)
            {
                let result = self.finish_sse(id, request, response_headers, Vec::new());
                return Some(result);
            }
        }
        self.refresh_retained(&id);
        self.enforce_connection_limits(&id);
        None
    }
}

fn reason(error: &std::io::Error) -> RemoveReason {
    if error.kind() == std::io::ErrorKind::OutOfMemory {
        RemoveReason::OverBudget
    } else {
        RemoveReason::ProtocolError
    }
}
