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

    fn aggregated_response(&mut self, response: &crate::aggregator::http::AggregatedResponse) {
        self.response(&response.parsed);
        self.bytes += response.sse_events.capacity()
            * size_of::<crate::parser::sse::ParsedSseEvent>()
            + response
                .sse_continuation_bytes
                .as_ref()
                .map_or(0, Vec::capacity);
        for event in &response.sse_events {
            self.source(event.source_event());
            self.bytes += event.id.as_ref().map_or(0, String::capacity)
                + event.event.as_ref().map_or(0, String::capacity);
        }
    }

    fn frames(&mut self, frames: &Vec<crate::parser::http2::ParsedHttp2Frame>) {
        self.bytes += frames.capacity() * size_of::<crate::parser::http2::ParsedHttp2Frame>();
        for frame in frames {
            self.source(&frame.source_event);
        }
    }
}

impl crate::aggregator::AggregatedResult {
    /// Charge retained allocations without serializing or cloning the completed request.
    pub(crate) fn retained_bytes(&self) -> usize {
        use crate::aggregator::AggregatedResult::*;
        let mut counter = Counter::new(size_of::<Self>());
        match self {
            Abandoned { partial, .. } => counter.bytes += partial.retained_bytes(),
            HttpComplete(pair) | SseComplete(pair) => {
                counter.request(&pair.request);
                counter.aggregated_response(&pair.response);
            }
            RequestOnly { request, .. } => counter.request(request),
            ResponseOnly { response, .. } => counter.aggregated_response(response),
            Http2Frames { frames, .. } => counter.frames(frames),
            Http2StreamComplete(stream) => {
                for frame in stream
                    .request_headers
                    .iter()
                    .chain(stream.response_headers.iter())
                {
                    counter.source(&frame.source_event);
                }
                counter.frames(&stream.request_data_frames);
                counter.frames(&stream.response_data_frames);
                for headers in stream
                    .decoded_request_headers
                    .iter()
                    .chain(stream.decoded_response_headers.iter())
                {
                    counter.bytes += headers.capacity() * size_of::<(String, String)>()
                        + headers
                            .iter()
                            .map(|(key, value)| key.capacity() + value.capacity())
                            .sum::<usize>();
                }
            }
            ProcessComplete(process) => {
                counter.bytes += process.comm.capacity()
                    + process.filename.as_ref().map_or(0, String::capacity)
                    + process.args.as_ref().map_or(0, String::capacity)
                    + process.stdout_data.capacity()
                    + process.stderr_data.capacity()
                    + process.session_id.as_ref().map_or(0, String::capacity);
            }
        }
        counter.bytes
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
