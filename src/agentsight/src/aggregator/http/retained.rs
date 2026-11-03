//! Capacity accounting includes shared capture buffers once per retained state.

use super::{ConnectionId, ConnectionMetrics, ConnectionState, HttpConnectionAggregator};
use crate::parser::http::{ParsedRequest, ParsedResponse};
use crate::probes::sslsniff::SslEvent;
use std::collections::{HashMap, HashSet};
use std::mem::size_of;

#[derive(Debug)]
struct Counter {
    bytes: usize,
    first_sources: [usize; 4],
    first_len: usize,
    sources: Option<HashSet<usize>>,
}

#[derive(Debug, Default)]
/// Per-connection charges and append-only SSE source history.
pub(super) struct RetainedConnection {
    bytes: usize,
    pending_bytes: usize,
    pending: usize,
    /// Payload charge used by the per-stream limit independently of request bytes.
    pub(super) sse_bytes: usize,
    sse: Option<RetainedSse>,
}

#[derive(Debug)]
struct RetainedSse {
    all: Counter,
    events: Counter,
    count: usize,
    first_source: usize,
    first_metadata: usize,
}

fn event_metadata(event: &crate::parser::sse::ParsedSseEvent) -> usize {
    event.id.as_ref().map_or(0, String::capacity)
        + event.event.as_ref().map_or(0, String::capacity)
        + event.parsed_retained_bytes()
}

impl RetainedConnection {
    /// Include persistent source-dedup sets in the shared byte budget.
    pub(super) fn metadata_bytes(&self) -> usize {
        self.sse.as_ref().map_or(0, |sse| {
            sse.all.metadata_bytes() + sse.events.metadata_bytes()
        })
    }

    fn update_state(&mut self, state: &ConnectionState) {
        self.pending = usize::from(!matches!(state, ConnectionState::Idle));
        let ConnectionState::SseActive {
            request,
            response_headers,
            sse_events,
            compressed_buffer,
            zstd_decoder,
            ..
        } = state
        else {
            self.sse = None;
            self.sse_bytes = 0;
            self.pending_bytes = state.buffered_bytes();
            return;
        };
        let first_source = sse_events
            .first()
            .map_or(0, |event| event.source_event() as *const SslEvent as usize);
        // Prefix recovery can prepend one event or repair the first event's metadata.
        // Ordinary reads only append, so old TLS sources never need to be rescanned.
        if self
            .sse
            .as_ref()
            .is_none_or(|sse| sse.count > sse_events.len() || sse.first_source != first_source)
        {
            let mut all = Counter::new(size_of::<ConnectionState>());
            if let Some(request) = request {
                all.request(request);
            }
            all.response(response_headers);
            self.sse = Some(RetainedSse {
                all,
                events: Counter::new(0),
                count: 0,
                first_source,
                first_metadata: 0,
            });
        }
        if let Some(sse) = &mut self.sse {
            if sse.count != 0 {
                let metadata = sse_events.first().map_or(0, event_metadata);
                sse.all.bytes = sse.all.bytes.saturating_sub(sse.first_metadata) + metadata;
                sse.events.bytes = sse.events.bytes.saturating_sub(sse.first_metadata) + metadata;
            }
            for event in &sse_events[sse.count..] {
                sse.all.source(event.source_event());
                sse.all.source(event.capture_event());
                sse.events.source(event.source_event());
                sse.events.source(event.capture_event());
                let metadata = event_metadata(event);
                sse.all.bytes += metadata;
                sse.events.bytes += metadata;
            }
            sse.count = sse_events.len();
            sse.first_metadata = sse_events.first().map_or(0, event_metadata);
            let buffers = sse_events.capacity() * size_of::<crate::parser::sse::ParsedSseEvent>()
                + compressed_buffer.as_ref().map_or(0, Vec::capacity)
                + zstd_decoder
                    .as_ref()
                    .map_or(0, |decoder| decoder.borrow().retained_bytes());
            self.pending_bytes = sse.all.bytes + buffers;
            self.sse_bytes = sse.events.bytes + buffers;
        }
    }
}

impl HttpConnectionAggregator {
    /// Reconcile the touched connection without rescanning earlier SSE events.
    pub(super) fn refresh_retained(&mut self, id: &ConnectionId) {
        let Some(state) = self.connections.peek(id) else {
            return;
        };
        let retained = self.retained.entry(*id).or_default();
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
        retained.update_state(state);
        let continuation = self
            .sse_continuation_buffers
            .peek(id)
            .map_or(0, Vec::capacity);
        retained.sse_bytes += continuation;
        let stream = self.sse_streams.peek(id);
        if let Some(stream) = stream {
            retained.sse_bytes = stream.payload_retained_bytes();
        }
        retained.bytes = retained.pending_bytes
            + continuation
            + retained.metadata_bytes()
            + stream.map_or(0, super::super::sse_stream::SseStream::retained_bytes)
            + self
                .request_chunks
                .peek(id)
                .map_or(0, super::super::chunk_stream::ChunkStream::retained_bytes);
        self.retained_metrics.connection_cache_bytes += retained.bytes;
        self.retained_metrics.pending_connection_bytes += retained.pending_bytes;
        self.retained_metrics.pending_connection_count += retained.pending;
    }

    /// Refund charges when a connection leaves correlation.
    pub(super) fn forget_retained(&mut self, id: &ConnectionId) {
        if let Some(retained) = self.retained.remove(id) {
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

    /// Return incrementally maintained gauges without traversing retained payloads.
    pub(crate) fn metrics(&self) -> ConnectionMetrics {
        ConnectionMetrics {
            eviction_count: self.eviction_count,
            ..self.retained_metrics
        }
    }
}

impl Counter {
    fn metadata_bytes(&self) -> usize {
        self.sources
            .as_ref()
            .map_or(0, |sources| sources.capacity() * size_of::<usize>())
    }

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
            self.source(event.capture_event());
            self.bytes += event_metadata(event);
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
                if let Some(events) = &stream.reduced_sse_events {
                    counter.bytes +=
                        events.capacity() * size_of::<crate::parser::sse::ParsedSseEvent>();
                    for event in events {
                        counter.source(event.source_event());
                        counter.source(event.capture_event());
                        counter.bytes += event_metadata(event);
                    }
                }
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
    #[cfg(test)]
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
            counter.source(event.capture_event());
            counter.bytes += event_metadata(event);
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
                    counter.source(event.capture_event());
                    counter.bytes += event_metadata(event);
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
