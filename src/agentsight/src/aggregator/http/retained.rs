//! Capacity accounting includes shared capture buffers once per retained state.

use super::ConnectionState;
use crate::parser::http::{ParsedRequest, ParsedResponse};
use crate::probes::sslsniff::SslEvent;
use std::collections::{HashMap, HashSet};
use std::mem::size_of;

struct Counter {
    bytes: usize,
    first_sources: [usize; 4],
    first_len: usize,
    sources: Option<HashSet<usize>>,
}

impl Counter {
    fn new(bytes: usize) -> Self {
        Self {
            bytes,
            first_sources: [0; 4],
            first_len: 0,
            sources: None,
        }
    }

    fn source(&mut self, event: &SslEvent) {
        let address = event as *const SslEvent as usize;
        let inserted = if let Some(sources) = &mut self.sources {
            sources.insert(address)
        } else if self.first_sources[..self.first_len].contains(&address) {
            false
        } else if self.first_len < self.first_sources.len() {
            self.first_sources[self.first_len] = address;
            self.first_len += 1;
            true
        } else {
            // Compact streams retain only request and response metadata; allocate
            // a set only for legacy streams that keep multiple capture sources.
            self.sources = Some(
                self.first_sources
                    .into_iter()
                    .chain(std::iter::once(address))
                    .collect(),
            );
            true
        };
        if inserted {
            self.bytes += size_of::<SslEvent>() + event.buf.capacity() + event.comm.capacity();
        }
    }
    fn headers(&mut self, headers: &HashMap<String, String>) {
        self.bytes += headers.capacity() * size_of::<(String, String)>()
            + headers
                .iter()
                .map(|(key, value)| key.capacity() + value.capacity())
                .sum::<usize>();
    }
    fn request(&mut self, request: &ParsedRequest) {
        self.source(&request.source_event);
        self.headers(&request.headers);
        self.bytes += request.method.capacity()
            + request.path.capacity()
            + request.reassembled_body.as_ref().map_or(0, Vec::capacity);
    }
    fn response(&mut self, response: &ParsedResponse) {
        self.source(&response.source_event);
        self.headers(&response.headers);
    }
}

impl ConnectionState {
    pub(super) fn sse_retained_bytes(&self) -> usize {
        let Self::SseActive {
            sse_events,
            compressed_buffer,
            zstd_decoder,
            ..
        } = self
        else {
            return 0;
        };
        let mut counter =
            Counter::new(sse_events.capacity() * size_of::<crate::parser::sse::ParsedSseEvent>());
        for event in sse_events {
            counter.source(event.source_event());
            counter.bytes += event.id.as_ref().map_or(0, String::capacity)
                + event.event.as_ref().map_or(0, String::capacity);
        }
        counter.bytes
            + compressed_buffer.as_ref().map_or(0, Vec::capacity)
            + zstd_decoder
                .as_ref()
                .map_or(0, |decoder| decoder.borrow().retained_bytes())
    }

    pub(super) fn buffered_bytes(&self) -> usize {
        let mut counter = Counter::new(size_of::<Self>());
        match self {
            Self::Idle => return 0,
            Self::RequestPending { request } => counter.request(request),
            Self::RequestBodyPending {
                request,
                body_buffer,
                ..
            } => {
                counter.request(request);
                counter.bytes += body_buffer.capacity();
            }
            Self::ResponsePending { request, assembly } => {
                if let Some(request) = request {
                    counter.request(request);
                }
                match assembly {
                    super::pending_response::PendingResponse::Headers(source) => {
                        counter.source(source)
                    }
                    super::pending_response::PendingResponse::Body { response, .. } => {
                        counter.response(response)
                    }
                }
            }
            Self::SseActive {
                request,
                response_headers,
                sse_events,
                compressed_buffer,
                zstd_decoder,
                ..
            } => {
                if let Some(request) = request {
                    counter.request(request);
                }
                counter.response(response_headers);
                counter.bytes +=
                    sse_events.capacity() * size_of::<crate::parser::sse::ParsedSseEvent>();
                for event in sse_events {
                    counter.source(event.source_event());
                    counter.bytes += event.id.as_ref().map_or(0, String::capacity)
                        + event.event.as_ref().map_or(0, String::capacity);
                }
                counter.bytes += compressed_buffer.as_ref().map_or(0, Vec::capacity)
                    + zstd_decoder
                        .as_ref()
                        .map_or(0, |decoder| decoder.borrow().retained_bytes());
            }
        }
        counter.bytes
    }
}
