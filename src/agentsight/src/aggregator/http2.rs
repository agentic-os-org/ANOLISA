//! HTTP/2 Stream Aggregator - correlates HTTP/2 request/response frames by stream ID
//!
//! This module implements aggregation logic for HTTP/2 frames, grouping frames
//! by their stream_id and correlating request (client->server) with response (server->client)
//! to form complete HTTP/2 request/response pairs.

use crate::aggregator::http::{ConnectionId, ConnectionMetrics, event_has_meaningful_output};
use crate::aggregator::result::AggregatedResult;
use crate::chrome_trace::{ChromeTraceEvent, ToChromeTraceEvent, ns_to_us};
use crate::config::DEFAULT_CONNECTION_CAPACITY;
use crate::parser::http2::{Http2FrameType, ParsedHttp2Frame};
use crate::parser::sse::SSEParser;
use hpack::Decoder;
use lru::LruCache;
use std::collections::HashMap;
use std::num::NonZeroUsize;

const MAX_CONTINUATION_BUFFER: usize = 65536;

/// Per-connection HPACK decoder state (one decoder per direction)
struct HpackConnectionState {
    req_decoder: Decoder<'static>,
    resp_decoder: Decoder<'static>,
}

impl HpackConnectionState {
    fn new() -> Self {
        HpackConnectionState {
            req_decoder: Decoder::new(),
            resp_decoder: Decoder::new(),
        }
    }
}

impl std::fmt::Debug for HpackConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HpackConnectionState")
            .finish_non_exhaustive()
    }
}

/// Buffer for reassembling CONTINUATION frames
#[derive(Debug, Clone)]
struct ContinuationBuffer {
    data: Vec<u8>,
    direction: StreamDirection,
}

/// Strip PADDED and PRIORITY framing from a HEADERS frame payload,
/// returning the raw header block fragment.
fn strip_headers_framing(payload: &[u8], flags: u8) -> &[u8] {
    let mut offset = 0;
    let mut end = payload.len();

    // PADDED flag (0x08): first byte is pad_length, last pad_length bytes are padding
    if flags & 0x08 != 0 {
        if payload.is_empty() {
            return &[];
        }
        let pad_length = payload[0] as usize;
        offset += 1;
        if end > pad_length {
            end -= pad_length;
        } else {
            return &[];
        }
    }

    // PRIORITY flag (0x20): 5 bytes (4-byte stream dependency + 1 byte weight)
    if flags & 0x20 != 0 {
        offset += 5;
    }

    if offset >= end {
        return &[];
    }

    &payload[offset..end]
}

/// Stream identifier within an HTTP/2 connection
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub struct StreamId {
    pub connection_id: ConnectionId,
    pub stream_id: u32,
}

impl StreamId {
    /// Create a new StreamId from connection and stream
    pub fn new(connection_id: ConnectionId, stream_id: u32) -> Self {
        StreamId {
            connection_id,
            stream_id,
        }
    }
}

/// Direction of the frame (request or response)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamDirection {
    /// Client -> Server (request direction, rw=1 for SSL_write)
    Request,
    /// Server -> Client (response direction, rw=0 for SSL_read)
    Response,
}

impl StreamDirection {
    /// Determine direction from SslEvent rw field
    pub fn from_rw(rw: i32) -> Self {
        if rw == 1 {
            StreamDirection::Request
        } else {
            StreamDirection::Response
        }
    }
}

/// State of an HTTP/2 stream during aggregation
#[derive(Debug, Clone)]
pub enum Http2StreamState {
    /// Waiting for request data (HEADERS or DATA frames)
    WaitingRequestData {
        request_headers: Option<ParsedHttp2Frame>,
        request_data_frames: Vec<ParsedHttp2Frame>,
    },
    /// Request complete, waiting for response
    RequestComplete {
        request_headers: Option<ParsedHttp2Frame>,
        request_data_frames: Vec<ParsedHttp2Frame>,
    },
    /// Receiving response data
    ReceivingResponse {
        request_headers: Option<ParsedHttp2Frame>,
        request_data_frames: Vec<ParsedHttp2Frame>,
        response_headers: Option<ParsedHttp2Frame>,
        response_data_frames: Vec<ParsedHttp2Frame>,
    },
    /// Stream complete (both request and response have END_STREAM)
    Complete(Http2Stream),
}

impl Http2StreamState {
    pub fn state_name(&self) -> &str {
        match self {
            Http2StreamState::WaitingRequestData { .. } => "WaitingRequestData",
            Http2StreamState::RequestComplete { .. } => "RequestComplete",
            Http2StreamState::ReceivingResponse { .. } => "ReceivingResponse",
            Http2StreamState::Complete(_) => "Complete",
        }
    }

    /// Estimate frame payload bytes retained for stream correlation.
    fn buffered_bytes(&self) -> usize {
        fn frame_bytes(frame: &ParsedHttp2Frame) -> usize {
            frame.payload_len
        }

        fn frames_bytes(frames: &[ParsedHttp2Frame]) -> usize {
            frames
                .iter()
                .map(frame_bytes)
                .fold(0usize, usize::saturating_add)
        }

        fn stream_bytes(stream: &Http2Stream) -> usize {
            stream
                .request_headers
                .as_ref()
                .map_or(0, frame_bytes)
                .saturating_add(frames_bytes(&stream.request_data_frames))
                .saturating_add(stream.response_headers.as_ref().map_or(0, frame_bytes))
                .saturating_add(frames_bytes(&stream.response_data_frames))
        }

        match self {
            Self::WaitingRequestData {
                request_headers,
                request_data_frames,
            }
            | Self::RequestComplete {
                request_headers,
                request_data_frames,
            } => request_headers
                .as_ref()
                .map_or(0, frame_bytes)
                .saturating_add(frames_bytes(request_data_frames)),
            Self::ReceivingResponse {
                request_headers,
                request_data_frames,
                response_headers,
                response_data_frames,
            } => request_headers
                .as_ref()
                .map_or(0, frame_bytes)
                .saturating_add(frames_bytes(request_data_frames))
                .saturating_add(response_headers.as_ref().map_or(0, frame_bytes))
                .saturating_add(frames_bytes(response_data_frames)),
            Self::Complete(stream) => stream_bytes(stream),
        }
    }
}

/// Whether a response DATA payload carries the SSE terminator that ends the
/// body, letting the stream be closed without waiting for END_STREAM.
///
/// A stream normally closes on the END_STREAM flag, but that flag can never
/// arrive: a client which recognises `[DONE]` as the end of the answer may exit
/// before reading the server's final, empty DATA frame. The stream then sits in
/// `ReceivingResponse` forever and no token usage is ever extracted from it —
/// the HTTP/1.1 path already avoids this by synthesising a done marker from the
/// chunked terminator (see `parser::unified`), and this is the HTTP/2 equivalent.
///
/// Only the OpenAI-style literal terminator counts, deliberately:
///
/// - It is unambiguous, and by protocol no body bytes follow it, so closing here
///   yields the same aggregated frames a later END_STREAM would have produced.
/// - Anthropic (`message_stop`) and the DashScope native protocol have their own
///   terminators, but those streams close on END_STREAM today. Recognising them
///   here would change when an already-working stream completes, for no gain.
fn response_sse_stream_ended(payload: &[u8]) -> bool {
    // Both spacings occur in the wild; `data:[DONE]` is legal SSE.
    const TERMINATORS: [&[u8]; 4] = [
        b"data: [DONE]",
        b"data:[DONE]",
        b"data: [END]",
        b"data:[END]",
    ];
    // Match only at SSE field boundaries: the terminator must sit at the very
    // start of the payload or be preceded by a newline. A bare substring search
    // would false-positive on model output that happens to contain the literal
    // text (e.g. a JSON delta whose `content` is `"data: [DONE]"`).
    TERMINATORS.iter().any(|t| {
        payload
            .windows(t.len())
            .enumerate()
            .any(|(i, w)| w == *t && (i == 0 || payload[i - 1] == b'\n'))
    })
}

/// A complete or partial HTTP/2 stream
#[derive(Debug, Clone)]
pub struct Http2Stream {
    /// Stream identifier
    pub stream_id: StreamId,
    /// Request headers frame (HEADERS with END_HEADERS)
    pub request_headers: Option<ParsedHttp2Frame>,
    /// Request data frames (DATA frames in request direction)
    pub request_data_frames: Vec<ParsedHttp2Frame>,
    /// Response headers frame
    pub response_headers: Option<ParsedHttp2Frame>,
    /// Response data frames (DATA frames in response direction)
    pub response_data_frames: Vec<ParsedHttp2Frame>,
    /// Whether the request has END_STREAM
    pub request_complete: bool,
    /// Whether the response has END_STREAM
    pub response_complete: bool,
    /// Timestamp of the first frame
    pub start_timestamp_ns: u64,
    /// Timestamp of the last frame
    pub end_timestamp_ns: u64,
    /// Decoded request headers from stateful HPACK (name, value) pairs
    pub decoded_request_headers: Option<Vec<(String, String)>>,
    /// Decoded response headers from stateful HPACK (name, value) pairs
    pub decoded_response_headers: Option<Vec<(String, String)>>,
}

impl Http2Stream {
    /// Create a new empty stream
    pub fn new(stream_id: StreamId, timestamp_ns: u64) -> Self {
        Http2Stream {
            stream_id,
            request_headers: None,
            request_data_frames: Vec::new(),
            response_headers: None,
            response_data_frames: Vec::new(),
            request_complete: false,
            response_complete: false,
            start_timestamp_ns: timestamp_ns,
            end_timestamp_ns: timestamp_ns,
            decoded_request_headers: None,
            decoded_response_headers: None,
        }
    }

    /// Check if the stream is complete (both request and response have END_STREAM)
    pub fn is_complete(&self) -> bool {
        self.request_complete && self.response_complete
    }

    /// Add a frame to the stream
    /// Returns true if the stream becomes complete after adding this frame
    pub fn add_frame(&mut self, frame: &ParsedHttp2Frame, direction: StreamDirection) -> bool {
        self.end_timestamp_ns = self.end_timestamp_ns.max(frame.source_event.timestamp_ns);

        match direction {
            StreamDirection::Request => {
                if frame.is_headers() {
                    self.request_headers = Some(frame.clone());
                    if frame.has_end_stream() {
                        self.request_complete = true;
                    }
                } else if frame.is_data() {
                    self.request_data_frames.push(frame.clone());
                    if frame.has_end_stream() {
                        self.request_complete = true;
                    }
                }
            }
            StreamDirection::Response => {
                if frame.is_headers() {
                    self.response_headers = Some(frame.clone());
                    if frame.has_end_stream() {
                        self.response_complete = true;
                    }
                } else if frame.is_data() {
                    self.response_data_frames.push(frame.clone());
                    if frame.has_end_stream() {
                        self.response_complete = true;
                    }
                }
            }
        }

        self.is_complete()
    }

    /// Concatenate all request DATA frames into a single buffer.
    /// HEADERS payload is HPACK-encoded metadata, not body data.
    pub fn request_body(&self) -> Vec<u8> {
        let mut result = Vec::new();
        for frame in &self.request_data_frames {
            result.extend_from_slice(frame.payload());
        }
        result
    }

    /// Concatenate all response DATA frames into a single buffer.
    /// HEADERS payload is HPACK-encoded metadata, not body data.
    pub fn response_body(&self) -> Vec<u8> {
        let mut result = Vec::new();
        for frame in &self.response_data_frames {
            result.extend_from_slice(frame.payload());
        }
        result
    }

    /// Content-Encoding header from response headers (e.g. "gzip", "deflate")
    pub fn content_encoding(&self) -> Option<String> {
        self.response_headers.as_ref().and_then(|h| {
            let headers = h.decode_headers_stateless();
            headers
                .iter()
                .find(|(name, _)| name == "content-encoding" || name == "Content-Encoding")
                .and_then(|(_, value)| value.clone())
        })
    }

    /// Content-Encoding header from request headers
    pub fn request_content_encoding(&self) -> Option<String> {
        self.request_headers.as_ref().and_then(|h| {
            let headers = h.decode_headers_stateless();
            headers
                .iter()
                .find(|(name, _)| name == "content-encoding" || name == "Content-Encoding")
                .and_then(|(_, value)| value.clone())
        })
    }

    /// Get request body as decompressed string (concatenates all data frames)
    pub fn request_body_str(&self) -> Option<String> {
        let body = self.request_body();
        if body.is_empty() {
            None
        } else {
            crate::utils::decompress::decompress_body_to_string(
                &body,
                self.request_content_encoding().as_deref(),
            )
        }
    }

    /// Get response body as decompressed string (concatenates all data frames)
    pub fn response_body_str(&self) -> Option<String> {
        let body = self.response_body();
        if body.is_empty() {
            None
        } else {
            crate::utils::decompress::decompress_body_to_string(
                &body,
                self.content_encoding().as_deref(),
            )
        }
    }

    /// Return the capture timestamp of the first observable meaningful SSE output.
    ///
    /// HTTP/2 keeps response DATA frames (and their source-event timestamps), so
    /// parse the uncompressed stream incrementally and attribute each completed
    /// SSE event to the DATA frame that made it observable. Compressed response
    /// bodies cannot be mapped back to plaintext event boundaries safely.
    pub fn first_output_timestamp_ns(&self) -> Option<u64> {
        if self
            .content_encoding()
            .is_some_and(|encoding| !encoding.eq_ignore_ascii_case("identity"))
        {
            return None;
        }

        let mut body = Vec::new();
        let mut parsed_event_count = 0usize;
        // Re-parsing the accumulated body is O(frames^2) worst-case, but meaningful output
        // normally arrives early and returns; an algorithm redesign is out of scope here.
        for frame in &self.response_data_frames {
            body.extend_from_slice(frame.payload());
            let Ok(body_str) = std::str::from_utf8(&body) else {
                continue;
            };
            let parsed = SSEParser::parse_stream(body_str);

            for event in parsed.events.iter().skip(parsed_event_count) {
                let value = serde_json::from_str::<serde_json::Value>(&event.data).ok();
                if event_has_meaningful_output(value.as_ref()) {
                    return Some(frame.source_event.timestamp_ns);
                }
            }
            parsed_event_count = parsed.events.len();
        }
        None
    }

    /// Try to parse request body as JSON (concatenates all data frames first)
    pub fn request_json_body(&self) -> Option<serde_json::Value> {
        self.request_body_str()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    /// Try to parse response body as JSON (concatenates all data frames first)
    pub fn response_json_body(&self) -> Option<serde_json::Value> {
        self.response_body_str()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    /// Parse response body as SSE events and return JSON array of event data
    ///
    /// This method parses the response body as SSE (Server-Sent Events) stream
    /// and returns a JSON array containing each event's data field.
    /// If the body is not valid SSE format, returns None.
    pub fn response_sse_json_array(&self) -> Option<serde_json::Value> {
        let body_str = self.response_body_str()?;

        // Use legacy SSEParser to parse the stream (returns owned data)
        let sse_events = SSEParser::parse_stream(&body_str);

        if sse_events.events.is_empty() {
            return None;
        }

        // Extract JSON data from each event
        let json_array: Vec<serde_json::Value> = sse_events
            .events
            .iter()
            .filter_map(|event| {
                // Skip [DONE] marker
                if event.data.trim() == "[DONE]" {
                    return None;
                }
                // Try to parse event data as JSON
                serde_json::from_str::<serde_json::Value>(&event.data).ok()
            })
            .collect();

        if json_array.is_empty() {
            None
        } else {
            Some(serde_json::Value::Array(json_array))
        }
    }
    /// Count complete SSE events in the response body.
    ///
    /// The count follows the HTTP/1 aggregator's event count semantics and
    /// includes non-JSON events such as the [DONE] marker.
    pub fn response_sse_event_count(&self) -> usize {
        self.response_body_str()
            .map(|body| SSEParser::parse_stream(&body).events.len())
            .unwrap_or(0)
    }

    /// Check if response content-type indicates SSE stream
    pub fn is_response_sse(&self) -> bool {
        if let Some(headers) = self.decoded_response_headers.as_ref() {
            if let Some((_, value)) = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            {
                return value.contains("text/event-stream");
            }
        }
        self.response_headers
            .as_ref()
            .map(|h| {
                let headers = h.decode_headers_stateless();
                headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                    .and_then(|(_, value)| value.clone())
                    .map(|ct| ct.contains("text/event-stream"))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// Extract HTTP method from request headers (e.g., "GET", "POST")
    /// Prefers stateful decoded headers, falls back to stateless.
    pub fn method(&self) -> String {
        if let Some(ref hdrs) = self.decoded_request_headers {
            if let Some((_, v)) = hdrs.iter().find(|(n, _)| n == ":method") {
                return v.clone();
            }
        }
        self.request_headers
            .as_ref()
            .map(|h| {
                let headers = h.decode_headers_stateless();
                headers
                    .iter()
                    .find(|(name, _)| name == ":method")
                    .and_then(|(_, value)| value.clone())
                    .unwrap_or_else(|| "POST".to_string())
            })
            .unwrap_or_else(|| "POST".to_string())
    }

    /// Extract path from request headers (e.g., "/v1/chat/completions")
    /// Prefers stateful decoded headers, falls back to stateless.
    pub fn path(&self) -> String {
        if let Some(ref hdrs) = self.decoded_request_headers {
            if let Some((_, v)) = hdrs.iter().find(|(n, _)| n == ":path") {
                return v.clone();
            }
        }
        self.request_headers
            .as_ref()
            .map(|h| {
                let headers = h.decode_headers_stateless();
                headers
                    .iter()
                    .find(|(name, _)| name == ":path")
                    .and_then(|(_, value)| value.clone())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    /// Extract status code from response headers
    /// Prefers stateful decoded headers, falls back to stateless.
    pub fn status_code(&self) -> u16 {
        if let Some(ref hdrs) = self.decoded_response_headers {
            if let Some((_, v)) = hdrs.iter().find(|(n, _)| n == ":status") {
                return v.parse().unwrap_or(0);
            }
        }
        self.response_headers
            .as_ref()
            .map(|h| {
                let headers = h.decode_headers_stateless();
                headers
                    .iter()
                    .find(|(name, _)| name == ":status")
                    .and_then(|(_, value)| value.clone())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    /// Get request headers as JSON string
    pub fn request_headers_json(&self) -> String {
        if let Some(ref headers) = self.request_headers {
            let decoded = headers
                .decode_headers_stateless()
                .into_iter()
                .filter_map(|(name, value)| value.map(|v| (name, v)))
                .collect::<std::collections::HashMap<String, String>>();
            serde_json::to_string(&decoded).unwrap_or_default()
        } else {
            String::new()
        }
    }

    /// Get response headers as JSON string
    pub fn response_headers_json(&self) -> String {
        if let Some(ref headers) = self.response_headers {
            let decoded = headers
                .decode_headers_stateless()
                .into_iter()
                .filter_map(|(name, value)| value.map(|v| (name, v)))
                .collect::<std::collections::HashMap<String, String>>();
            serde_json::to_string(&decoded).unwrap_or_default()
        } else {
            String::new()
        }
    }

    /// Get process command name from source event
    pub fn comm(&self) -> String {
        self.request_headers
            .as_ref()
            .map(|h| h.source_event.comm_str())
            .or_else(|| {
                self.request_data_frames
                    .first()
                    .map(|f| f.source_event.comm_str())
            })
            .or_else(|| {
                self.response_headers
                    .as_ref()
                    .map(|h| h.source_event.comm_str())
            })
            .or_else(|| {
                self.response_data_frames
                    .first()
                    .map(|f| f.source_event.comm_str())
            })
            .unwrap_or_default()
    }

    /// Get process ID from source event
    pub fn pid(&self) -> u32 {
        self.request_headers
            .as_ref()
            .map(|h| h.source_event.pid)
            .or_else(|| self.request_data_frames.first().map(|f| f.source_event.pid))
            .or_else(|| self.response_headers.as_ref().map(|h| h.source_event.pid))
            .or_else(|| {
                self.response_data_frames
                    .first()
                    .map(|f| f.source_event.pid)
            })
            .unwrap_or(0)
    }
}

/// Decoded headers pair for a stream (request + response)
#[derive(Debug, Clone, Default)]
struct DecodedHeadersPair {
    request: Option<Vec<(String, String)>>,
    response: Option<Vec<(String, String)>>,
}

impl DecodedHeadersPair {
    fn buffered_bytes(&self) -> usize {
        fn headers_bytes(headers: &[(String, String)]) -> usize {
            headers.iter().fold(0usize, |total, (name, value)| {
                total.saturating_add(name.len()).saturating_add(value.len())
            })
        }

        self.request
            .as_deref()
            .map_or(0, headers_bytes)
            .saturating_add(self.response.as_deref().map_or(0, headers_bytes))
    }
}

/// HTTP/2 Stream Aggregator
///
/// Aggregates HTTP/2 frames by stream_id within a connection,
/// correlating request and response frames to form complete streams.
/// Maintains per-connection HPACK decoder state for stateful header decoding.
#[derive(Debug)]
pub struct Http2StreamAggregator {
    /// Active streams being aggregated (key: StreamId)
    streams: LruCache<StreamId, Http2StreamState>,
    /// Completed streams waiting to be retrieved
    completed_streams: Vec<Http2Stream>,
    /// Per-connection HPACK decoder state
    hpack_states: LruCache<ConnectionId, HpackConnectionState>,
    /// Buffers for CONTINUATION frame reassembly (key: StreamId)
    continuation_buffers: HashMap<StreamId, ContinuationBuffer>,
    /// Decoded headers waiting to be attached to streams on completion
    decoded_headers_store: HashMap<StreamId, DecodedHeadersPair>,
    /// Cumulative active-stream evictions from the bounded LRU.
    eviction_count: u64,
}

impl Default for Http2StreamAggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl Http2StreamAggregator {
    /// Create a new aggregator with default capacity
    pub fn new() -> Self {
        Http2StreamAggregator {
            streams: LruCache::new(NonZeroUsize::new(DEFAULT_CONNECTION_CAPACITY * 4).unwrap()),
            completed_streams: Vec::new(),
            hpack_states: LruCache::new(NonZeroUsize::new(DEFAULT_CONNECTION_CAPACITY).unwrap()),
            continuation_buffers: HashMap::new(),
            decoded_headers_store: HashMap::new(),
            eviction_count: 0,
        }
    }

    /// Create a new aggregator with custom capacity
    pub fn with_capacity(capacity: usize) -> Self {
        Http2StreamAggregator {
            streams: LruCache::new(NonZeroUsize::new(capacity).unwrap()),
            completed_streams: Vec::new(),
            hpack_states: LruCache::new(NonZeroUsize::new(capacity).unwrap()),
            continuation_buffers: HashMap::new(),
            decoded_headers_store: HashMap::new(),
            eviction_count: 0,
        }
    }

    /// Process a batch of HTTP/2 frames
    ///
    /// Returns completed streams that have both request and response with END_STREAM.
    /// Handles SETTINGS (dynamic table size), HEADERS (with PADDED/PRIORITY stripping),
    /// and CONTINUATION reassembly with stateful HPACK decoding.
    pub fn process_frames(&mut self, frames: Vec<ParsedHttp2Frame>) -> Vec<Http2Stream> {
        let mut completed = Vec::new();

        for frame in frames {
            let connection_id = ConnectionId::from_ssl_event(&frame.source_event);
            let direction = StreamDirection::from_rw(frame.source_event.rw);

            // Handle connection-level frames (stream_id == 0)
            if frame.stream_id == 0 {
                if frame.is_settings() {
                    self.handle_settings_frame(&frame, connection_id, direction);
                }
                continue;
            }

            let stream_id = StreamId::new(connection_id, frame.stream_id);

            // Handle CONTINUATION frames: buffer until END_HEADERS
            if frame.frame_type == Http2FrameType::Continuation {
                self.handle_continuation_frame(&frame, stream_id, connection_id, direction);
                continue;
            }

            // Handle HEADERS frames: strip framing, possibly buffer for CONTINUATION
            if frame.is_headers() {
                let decoded = if frame.has_end_headers() {
                    let fragment = strip_headers_framing(frame.payload(), frame.flags);
                    self.decode_header_block(connection_id, direction, fragment)
                } else {
                    // No END_HEADERS — start buffering for CONTINUATION
                    let fragment = strip_headers_framing(frame.payload(), frame.flags);
                    if fragment.len() <= MAX_CONTINUATION_BUFFER {
                        self.continuation_buffers.insert(
                            stream_id,
                            ContinuationBuffer {
                                data: fragment.to_vec(),
                                direction,
                            },
                        );
                    }
                    None
                };

                self.store_decoded_headers(stream_id, direction, decoded);

                // Get or create stream state, process frame
                let state = self.streams.pop(&stream_id).unwrap_or_else(|| {
                    Http2StreamState::WaitingRequestData {
                        request_headers: None,
                        request_data_frames: Vec::new(),
                    }
                });

                let state = self.process_frame_in_state(state, frame, direction, &stream_id);

                match state {
                    Http2StreamState::Complete(stream) => {
                        completed.push(self.finalize_stream(stream_id, stream));
                    }
                    _ => {
                        self.insert_stream_state(stream_id, state);
                    }
                }
                continue;
            }

            // DATA and other frames: normal processing
            let state = self.streams.pop(&stream_id).unwrap_or_else(|| {
                Http2StreamState::WaitingRequestData {
                    request_headers: None,
                    request_data_frames: Vec::new(),
                }
            });

            let state = self.process_frame_in_state(state, frame, direction, &stream_id);

            match state {
                Http2StreamState::Complete(stream) => {
                    completed.push(self.finalize_stream(stream_id, stream));
                }
                _ => {
                    self.insert_stream_state(stream_id, state);
                }
            }
        }

        completed
    }

    /// Handle SETTINGS frame: update HPACK decoder dynamic table size
    fn handle_settings_frame(
        &mut self,
        frame: &ParsedHttp2Frame,
        conn_id: ConnectionId,
        direction: StreamDirection,
    ) {
        // ACK frames have no payload
        if frame.flags & 0x01 != 0 {
            return;
        }

        let payload = frame.payload();
        // SETTINGS payload is a list of 6-byte entries: (2-byte id, 4-byte value)
        let mut pos = 0;
        while pos + 6 <= payload.len() {
            let id = ((payload[pos] as u16) << 8) | payload[pos + 1] as u16;
            let value = ((payload[pos + 2] as u32) << 24)
                | ((payload[pos + 3] as u32) << 16)
                | ((payload[pos + 4] as u32) << 8)
                | payload[pos + 5] as u32;
            pos += 6;

            // SETTINGS_HEADER_TABLE_SIZE (0x01)
            // Per RFC 7540 §6.5.2: SETTINGS from peer X constrains the OTHER
            // direction's encoder, so we resize the decoder for the opposite direction.
            if id == 0x01 {
                let state = self
                    .hpack_states
                    .get_or_insert_mut(conn_id, HpackConnectionState::new);
                match direction {
                    StreamDirection::Request => {
                        state.resp_decoder.set_max_table_size(value as usize)
                    }
                    StreamDirection::Response => {
                        state.req_decoder.set_max_table_size(value as usize)
                    }
                }
                log::debug!(
                    "HPACK table size update: conn={conn_id:?} dir={direction:?} size={value}"
                );
            }
        }
    }

    /// Handle CONTINUATION frame: append to buffer, decode on END_HEADERS
    fn handle_continuation_frame(
        &mut self,
        frame: &ParsedHttp2Frame,
        stream_id: StreamId,
        conn_id: ConnectionId,
        _direction: StreamDirection,
    ) {
        let payload = frame.payload();

        if let Some(buffer) = self.continuation_buffers.get_mut(&stream_id) {
            if buffer.data.len() + payload.len() <= MAX_CONTINUATION_BUFFER {
                buffer.data.extend_from_slice(payload);
            } else {
                log::warn!("CONTINUATION buffer overflow for stream {stream_id:?}, dropping");
                self.continuation_buffers.remove(&stream_id);
                return;
            }

            if frame.has_end_headers() {
                let buffer = self.continuation_buffers.remove(&stream_id).unwrap();
                let decoded = self.decode_header_block(conn_id, buffer.direction, &buffer.data);
                self.store_decoded_headers(stream_id, buffer.direction, decoded);
            }
        }
        // If no buffer exists, this CONTINUATION is orphaned — ignore
    }

    /// Decode a header block fragment using the stateful HPACK decoder.
    /// On error, resets the decoder for that direction and returns None.
    fn decode_header_block(
        &mut self,
        conn_id: ConnectionId,
        direction: StreamDirection,
        fragment: &[u8],
    ) -> Option<Vec<(String, String)>> {
        if fragment.is_empty() {
            return Some(Vec::new());
        }

        let state = self
            .hpack_states
            .get_or_insert_mut(conn_id, HpackConnectionState::new);
        let decoder = match direction {
            StreamDirection::Request => &mut state.req_decoder,
            StreamDirection::Response => &mut state.resp_decoder,
        };

        match decoder.decode(fragment) {
            Ok(headers) => {
                let result: Vec<(String, String)> = headers
                    .into_iter()
                    .map(|(name, value)| {
                        (
                            String::from_utf8_lossy(&name).into_owned(),
                            String::from_utf8_lossy(&value).into_owned(),
                        )
                    })
                    .collect();
                Some(result)
            }
            Err(e) => {
                log::warn!(
                    "HPACK decode error for conn={conn_id:?} dir={direction:?}: {e:?}, resetting decoder"
                );
                // Reset decoder for this direction
                let state = self
                    .hpack_states
                    .get_or_insert_mut(conn_id, HpackConnectionState::new);
                match direction {
                    StreamDirection::Request => state.req_decoder = Decoder::new(),
                    StreamDirection::Response => state.resp_decoder = Decoder::new(),
                }
                None
            }
        }
    }

    /// Insert stream state back into LRU, cleaning up side-maps on eviction.
    fn insert_stream_state(&mut self, stream_id: StreamId, state: Http2StreamState) {
        if let Some((evicted_id, _)) = self.streams.push(stream_id, state) {
            self.continuation_buffers.remove(&evicted_id);
            self.decoded_headers_store.remove(&evicted_id);
            self.eviction_count = self.eviction_count.saturating_add(1);
        }
    }

    /// Store decoded headers for a stream. They'll be attached when the stream completes.
    fn store_decoded_headers(
        &mut self,
        stream_id: StreamId,
        direction: StreamDirection,
        decoded: Option<Vec<(String, String)>>,
    ) {
        if let Some(hdrs) = decoded {
            let pair = self.decoded_headers_store.entry(stream_id).or_default();
            match direction {
                StreamDirection::Request => pair.request = Some(hdrs),
                StreamDirection::Response => pair.response = Some(hdrs),
            }
        }
    }

    /// Finalize a completed stream by attaching any decoded headers from the store.
    fn finalize_stream(&mut self, stream_id: StreamId, mut stream: Http2Stream) -> Http2Stream {
        if let Some(pair) = self.decoded_headers_store.remove(&stream_id) {
            stream.decoded_request_headers = pair.request;
            stream.decoded_response_headers = pair.response;
        }
        // Defensive cleanup: a malformed or aborted stream could leave a stale
        // continuation buffer behind; remove it when the stream completes.
        self.continuation_buffers.remove(&stream_id);
        stream
    }

    /// Process a single frame within the context of a stream state
    fn process_frame_in_state(
        &self,
        state: Http2StreamState,
        frame: ParsedHttp2Frame,
        direction: StreamDirection,
        stream_id: &StreamId,
    ) -> Http2StreamState {
        log::debug!(
            "Processing http/2 frame in state: {}, stream_id: {:?}",
            state.state_name(),
            stream_id
        );
        match state {
            Http2StreamState::WaitingRequestData {
                mut request_headers,
                mut request_data_frames,
            } => {
                if direction == StreamDirection::Request {
                    if frame.is_headers() {
                        request_headers = Some(frame.clone());
                        if frame.has_end_stream() {
                            // Request is complete (no body)
                            return Http2StreamState::RequestComplete {
                                request_headers,
                                request_data_frames,
                            };
                        }
                    } else if frame.is_data() {
                        request_data_frames.push(frame.clone());
                        if frame.has_end_stream() {
                            // Request is complete
                            return Http2StreamState::RequestComplete {
                                request_headers,
                                request_data_frames,
                            };
                        }
                    }
                    // Continue waiting for more request data
                    Http2StreamState::WaitingRequestData {
                        request_headers,
                        request_data_frames,
                    }
                } else {
                    // Unexpected response before request complete, stay in waiting state
                    Http2StreamState::WaitingRequestData {
                        request_headers,
                        request_data_frames,
                    }
                }
            }

            Http2StreamState::RequestComplete {
                request_headers,
                request_data_frames,
            } => {
                if direction == StreamDirection::Response {
                    let mut response_headers = None;
                    let mut response_data_frames = Vec::new();

                    if frame.is_headers() {
                        response_headers = Some(frame.clone());
                        if frame.has_end_stream() {
                            // Response is complete (no body)
                            let mut stream = Http2Stream::new(
                                *stream_id,
                                request_headers
                                    .as_ref()
                                    .map(|h| h.source_event.timestamp_ns)
                                    .unwrap_or(frame.source_event.timestamp_ns),
                            );
                            stream.request_headers = request_headers;
                            stream.request_data_frames = request_data_frames;
                            stream.request_complete = true;
                            stream.response_headers = response_headers;
                            stream.response_complete = true;
                            stream.end_timestamp_ns = frame.source_event.timestamp_ns;
                            return Http2StreamState::Complete(stream);
                        }
                    } else if frame.is_data() {
                        let sse_ended = response_sse_stream_ended(frame.payload());
                        response_data_frames.push(frame.clone());
                        if frame.has_end_stream() || sse_ended {
                            // Response is complete
                            let mut stream = Http2Stream::new(
                                *stream_id,
                                request_headers
                                    .as_ref()
                                    .map(|h| h.source_event.timestamp_ns)
                                    .unwrap_or(frame.source_event.timestamp_ns),
                            );
                            stream.request_headers = request_headers;
                            stream.request_data_frames = request_data_frames;
                            stream.request_complete = true;
                            stream.response_headers = response_headers;
                            stream.response_data_frames = response_data_frames;
                            stream.response_complete = true;
                            stream.end_timestamp_ns = frame.source_event.timestamp_ns;
                            return Http2StreamState::Complete(stream);
                        }
                    }

                    // Continue receiving response data
                    Http2StreamState::ReceivingResponse {
                        request_headers,
                        request_data_frames,
                        response_headers,
                        response_data_frames,
                    }
                } else {
                    // Stay in request complete state
                    Http2StreamState::RequestComplete {
                        request_headers,
                        request_data_frames,
                    }
                }
            }

            Http2StreamState::ReceivingResponse {
                request_headers,
                request_data_frames,
                mut response_headers,
                mut response_data_frames,
            } => {
                if direction == StreamDirection::Response {
                    if frame.is_headers() {
                        response_headers = Some(frame.clone());
                        if frame.has_end_stream() {
                            // Response is complete
                            let mut stream = Http2Stream::new(
                                *stream_id,
                                request_headers
                                    .as_ref()
                                    .map(|h| h.source_event.timestamp_ns)
                                    .unwrap_or(frame.source_event.timestamp_ns),
                            );
                            stream.request_headers = request_headers;
                            stream.request_data_frames = request_data_frames;
                            stream.request_complete = true;
                            stream.response_headers = response_headers;
                            stream.response_data_frames = response_data_frames;
                            stream.response_complete = true;
                            stream.end_timestamp_ns = frame.source_event.timestamp_ns;
                            return Http2StreamState::Complete(stream);
                        }
                    } else if frame.is_data() {
                        let sse_ended = response_sse_stream_ended(frame.payload());
                        response_data_frames.push(frame.clone());
                        if frame.has_end_stream() || sse_ended {
                            // Response is complete
                            let mut stream = Http2Stream::new(
                                *stream_id,
                                request_headers
                                    .as_ref()
                                    .map(|h| h.source_event.timestamp_ns)
                                    .unwrap_or(frame.source_event.timestamp_ns),
                            );
                            stream.request_headers = request_headers;
                            stream.request_data_frames = request_data_frames;
                            stream.request_complete = true;
                            stream.response_headers = response_headers;
                            stream.response_data_frames = response_data_frames;
                            stream.response_complete = true;
                            stream.end_timestamp_ns = frame.source_event.timestamp_ns;
                            return Http2StreamState::Complete(stream);
                        }
                    }
                }
                // Continue receiving response data
                Http2StreamState::ReceivingResponse {
                    request_headers,
                    request_data_frames,
                    response_headers,
                    response_data_frames,
                }
            }

            Http2StreamState::Complete(stream) => {
                // Stream already complete, shouldn't receive more frames
                Http2StreamState::Complete(stream)
            }
        }
    }

    /// Check if there are any pending streams
    pub fn has_pending(&self) -> bool {
        !self.streams.is_empty()
    }

    /// Get count of active streams
    pub fn active_stream_count(&self) -> usize {
        self.streams.len()
    }

    /// Return HTTP/2 stream-correlation gauges and cumulative LRU evictions.
    pub(crate) fn metrics(&self) -> ConnectionMetrics {
        let pending_connection_bytes = self
            .streams
            .iter()
            .map(|(_, state)| state.buffered_bytes())
            .fold(0usize, usize::saturating_add);
        let continuation_bytes = self
            .continuation_buffers
            .values()
            .map(|buffer| buffer.data.len())
            .fold(0usize, usize::saturating_add);
        let decoded_header_bytes = self
            .decoded_headers_store
            .values()
            .map(DecodedHeadersPair::buffered_bytes)
            .fold(0usize, usize::saturating_add);

        ConnectionMetrics {
            connection_cache_bytes: pending_connection_bytes
                .saturating_add(continuation_bytes)
                .saturating_add(decoded_header_bytes),
            pending_connection_count: self.streams.len(),
            pending_connection_bytes,
            eviction_count: self.eviction_count,
        }
    }

    /// Clear all streams
    pub fn clear(&mut self) {
        self.streams.clear();
        self.completed_streams.clear();
        self.hpack_states.clear();
        self.continuation_buffers.clear();
        self.decoded_headers_store.clear();
    }

    /// Drain all pending streams and return them as completed
    /// Useful for shutdown or forced completion
    pub fn drain_pending(&mut self) -> Vec<Http2Stream> {
        let mut result = Vec::new();

        // Move all streams from LRU cache
        while let Some((stream_id, state)) = self.streams.pop_lru() {
            if let Some(stream) = self.stream_from_state(state, stream_id) {
                result.push(self.finalize_stream(stream_id, stream));
            }
        }

        result
    }

    /// Convert a stream state to a Http2Stream if possible
    fn stream_from_state(
        &self,
        state: Http2StreamState,
        stream_id: StreamId,
    ) -> Option<Http2Stream> {
        match state {
            Http2StreamState::Complete(stream) => Some(stream),
            Http2StreamState::RequestComplete {
                request_headers,
                request_data_frames,
            } => {
                let timestamp_ns = request_headers
                    .as_ref()
                    .map(|h| h.source_event.timestamp_ns)
                    .unwrap_or_else(|| {
                        request_data_frames
                            .first()
                            .map(|f| f.source_event.timestamp_ns)
                            .unwrap_or(0)
                    });
                let mut stream = Http2Stream::new(stream_id, timestamp_ns);
                stream.request_headers = request_headers;
                stream.request_data_frames = request_data_frames;
                stream.request_complete = true;
                Some(stream)
            }
            Http2StreamState::ReceivingResponse {
                request_headers,
                request_data_frames,
                response_headers,
                response_data_frames,
            } => {
                let timestamp_ns = request_headers
                    .as_ref()
                    .map(|h| h.source_event.timestamp_ns)
                    .unwrap_or_else(|| {
                        request_data_frames
                            .first()
                            .map(|f| f.source_event.timestamp_ns)
                            .unwrap_or(0)
                    });
                let mut stream = Http2Stream::new(stream_id, timestamp_ns);
                stream.request_headers = request_headers;
                stream.request_data_frames = request_data_frames;
                stream.request_complete = true;
                stream.response_headers = response_headers;
                stream.response_data_frames = response_data_frames;
                Some(stream)
            }
            Http2StreamState::WaitingRequestData { .. } => None,
        }
    }
}

/// Convert Http2Stream to AggregatedResult
impl From<Http2Stream> for AggregatedResult {
    fn from(stream: Http2Stream) -> Self {
        AggregatedResult::Http2StreamComplete(stream)
    }
}

impl ToChromeTraceEvent for Http2Stream {
    fn to_chrome_trace_events(&self) -> Vec<ChromeTraceEvent> {
        let mut events = Vec::new();
        let ts_us = ns_to_us(self.start_timestamp_ns);
        let dur_us = ns_to_us(
            self.end_timestamp_ns
                .saturating_sub(self.start_timestamp_ns),
        );
        const MIN_DUR_US: u64 = 1_000;
        let actual_dur = dur_us.max(MIN_DUR_US);

        // Create a single complete event representing the entire stream
        let stream_event = ChromeTraceEvent::complete(
            format!("HTTP/2 stream={}", self.stream_id.stream_id),
            "http2.stream",
            self.stream_id.connection_id.pid,
            0, // tid not available at stream level
            ts_us,
            actual_dur,
        );

        events.push(stream_event);

        // Add events for individual frames
        if let Some(ref headers) = self.request_headers {
            events.extend(headers.to_chrome_trace_events());
        }
        for frame in &self.request_data_frames {
            events.extend(frame.to_chrome_trace_events());
        }
        if let Some(ref headers) = self.response_headers {
            events.extend(headers.to_chrome_trace_events());
        }
        for frame in &self.response_data_frames {
            events.extend(frame.to_chrome_trace_events());
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probes::sslsniff::SslEvent;
    use hpack::Encoder;
    use std::rc::Rc;

    fn create_test_event(pid: u32, ssl_ptr: u64, rw: i32, timestamp_ns: u64) -> Rc<SslEvent> {
        Rc::new(SslEvent {
            source: 0,
            timestamp_ns,
            delta_ns: 0,
            pid,
            tid: 1,
            uid: 0,
            len: 0,
            rw,
            comm: "test".to_string(),
            buf: Vec::new(),
            is_handshake: false,
            ssl_ptr,
        })
    }

    fn create_test_frame(
        stream_id: u32,
        frame_type: u8,
        flags: u8,
        payload: Vec<u8>,
        event: Rc<SslEvent>,
    ) -> ParsedHttp2Frame {
        let payload_offset = 9; // Skip frame header
        let payload_len = payload.len();

        // Create a new event with the payload in buf
        let mut buf = Vec::with_capacity(9 + payload_len);
        // Frame header
        buf.push(((payload_len >> 16) & 0xFF) as u8);
        buf.push(((payload_len >> 8) & 0xFF) as u8);
        buf.push((payload_len & 0xFF) as u8);
        buf.push(frame_type);
        buf.push(flags);
        buf.push(((stream_id >> 24) & 0x7F) as u8);
        buf.push(((stream_id >> 16) & 0xFF) as u8);
        buf.push(((stream_id >> 8) & 0xFF) as u8);
        buf.push((stream_id & 0xFF) as u8);
        // Payload
        buf.extend_from_slice(&payload);

        let event_with_buf = Rc::new(SslEvent {
            source: event.source,
            timestamp_ns: event.timestamp_ns,
            delta_ns: event.delta_ns,
            pid: event.pid,
            tid: event.tid,
            uid: event.uid,
            len: buf.len() as u32,
            rw: event.rw,
            comm: event.comm.clone(),
            buf,
            is_handshake: event.is_handshake,
            ssl_ptr: event.ssl_ptr,
        });

        ParsedHttp2Frame {
            frame_type: Http2FrameType::from_u8(frame_type),
            flags,
            stream_id,
            payload_offset,
            payload_len,
            source_event: event_with_buf,
        }
    }

    #[test]
    fn test_stream_direction_from_rw() {
        assert_eq!(StreamDirection::from_rw(1), StreamDirection::Request);
        assert_eq!(StreamDirection::from_rw(0), StreamDirection::Response);
    }

    #[test]
    fn first_output_timestamp_uses_meaningful_data_frame_timestamp() {
        let mut stream =
            Http2Stream::new(StreamId::new(ConnectionId { pid: 1, ssl_ptr: 1 }, 1), 100);
        let events = [
            (100, br#"data: {"type":"response.created"}"#.to_vec()),
            (
                200,
                br#"data: {"type":"response.output_text.delta","delta":""}"#.to_vec(),
            ),
            (
                300,
                br#"data: {"type":"response.output_text.delta","delta":"hello"}"#.to_vec(),
            ),
            (
                400,
                br#"data: {"type":"response.output_text.done","text":"hello"}"#.to_vec(),
            ),
        ];
        for (timestamp_ns, mut payload) in events {
            payload.extend_from_slice(&[10, 10]);
            stream.response_data_frames.push(create_test_frame(
                1,
                0,
                0,
                payload,
                create_test_event(1234, 0x1000, 0, timestamp_ns),
            ));
        }

        assert_eq!(stream.first_output_timestamp_ns(), Some(300));
    }

    #[test]
    fn first_output_timestamp_handles_meaningful_event_split_across_data_frames() {
        let mut stream =
            Http2Stream::new(StreamId::new(ConnectionId { pid: 1, ssl_ptr: 1 }, 1), 100);
        let first_payload = b"data: {\"type\":\"response.created\"}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hel".to_vec();
        let second_payload = b"lo\"}\n\n".to_vec();

        // The first DATA frame has a complete metadata event, but the
        // meaningful event is still incomplete and must not be counted.
        let first_body = std::str::from_utf8(&first_payload).unwrap();
        let first_parse = SSEParser::parse_stream(first_body);
        assert_eq!(first_parse.events.len(), 1);
        assert!(first_parse.events.iter().all(|event| {
            let value = serde_json::from_str::<serde_json::Value>(&event.data).ok();
            !event_has_meaningful_output(value.as_ref())
        }));

        stream.response_data_frames.push(create_test_frame(
            1,
            0,
            0,
            first_payload,
            create_test_event(1234, 0x1000, 0, 200),
        ));
        stream.response_data_frames.push(create_test_frame(
            1,
            0,
            0,
            second_payload,
            create_test_event(1234, 0x1000, 0, 400),
        ));

        // Re-parsing the accumulated body after frame 2 must attribute the
        // first complete meaningful event to frame 2, not the metadata frame.
        assert_eq!(stream.first_output_timestamp_ns(), Some(400));
    }

    #[test]
    fn first_output_timestamp_is_none_without_meaningful_data_frame() {
        let mut stream =
            Http2Stream::new(StreamId::new(ConnectionId { pid: 1, ssl_ptr: 1 }, 1), 100);
        let mut payload = br#"data: {"type":"response.output_text.done","text":"final"}"#.to_vec();
        payload.extend_from_slice(&[10, 10]);
        stream.response_data_frames.push(create_test_frame(
            1,
            0,
            0,
            payload,
            create_test_event(1234, 0x1000, 0, 500),
        ));

        assert_eq!(stream.first_output_timestamp_ns(), None);
    }

    // ─── SSE terminator completion (HTTP/2 counterpart of the chunked
    // synthetic done marker in parser::unified) ─────────────────────────────

    /// Drive one request/response exchange and return the completed streams.
    ///
    /// `resp_data` are response DATA payloads with their END_STREAM flag, applied
    /// in order, so a test can withhold END_STREAM the way a client that exits on
    /// `[DONE]` does.
    fn run_exchange(resp_data: &[(&[u8], bool)]) -> Vec<Http2Stream> {
        let mut aggregator = Http2StreamAggregator::new();

        // Request: HEADERS with END_STREAM, so the stream reaches RequestComplete.
        let completed = aggregator.process_frames(vec![create_test_frame(
            1,
            1,
            0x05,
            b":method: POST\n:path: /v1/chat/completions".to_vec(),
            create_test_event(1234, 0x1000, 1, 1000),
        )]);
        assert!(completed.is_empty(), "response has not started yet");

        // Response HEADERS without END_STREAM: a body follows.
        let mut completed = aggregator.process_frames(vec![create_test_frame(
            1,
            1,
            0x04,
            b":status: 200".to_vec(),
            create_test_event(1234, 0x1000, 0, 2000),
        )]);

        for (i, (payload, end_stream)) in resp_data.iter().enumerate() {
            let flags = if *end_stream { 0x01 } else { 0x00 };
            completed.extend(aggregator.process_frames(vec![create_test_frame(
                1,
                0,
                flags,
                payload.to_vec(),
                create_test_event(1234, 0x1000, 0, 3000 + i as u64),
            )]));
        }
        completed
    }

    #[test]
    fn sse_done_completes_stream_without_end_stream() {
        // The regression this exists for: the client stops reading at `[DONE]`, so
        // END_STREAM never arrives and the stream used to stall forever.
        let completed = run_exchange(&[
            (
                b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                false,
            ),
            (b"data: [DONE]\n\n", false),
        ]);
        assert_eq!(completed.len(), 1, "[DONE] must close the stream");
        let stream = &completed[0];
        assert!(stream.response_complete);
        assert_eq!(
            stream.response_data_frames.len(),
            2,
            "the terminator frame is part of the body, not dropped"
        );
    }

    #[test]
    fn end_stream_still_completes_without_sse_terminator() {
        // Guards the pre-existing path: a plain JSON response has no `[DONE]` and
        // must still complete on END_STREAM alone, at exactly that frame.
        let completed = run_exchange(&[
            (b"{\"usage\":{\"prompt_tokens\":7", false),
            (b",\"completion_tokens\":3}}", true),
        ]);
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].response_data_frames.len(), 2);
    }

    #[test]
    fn body_without_terminator_stays_open() {
        // Neither END_STREAM nor `[DONE]`: the stream must keep collecting rather
        // than be closed on a guess.
        let completed = run_exchange(&[
            (b"data: {\"delta\":\"a\"}\n\n", false),
            (b"data: {\"delta\":\"b\"}\n\n", false),
        ]);
        assert!(completed.is_empty());
    }

    #[test]
    fn anthropic_terminator_is_left_to_end_stream() {
        // Anthropic streams close on END_STREAM today. `message_stop` is
        // deliberately not treated as a terminator here, so their completion point
        // is unchanged: the first two frames must not complete the stream.
        let completed = run_exchange(&[
            (
                b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                false,
            ),
            (b"data: {\"type\":\"message_stop\"}\n\n", false),
            (b"", true),
        ]);
        assert_eq!(
            completed.len(),
            1,
            "completes on END_STREAM, not on message_stop"
        );
        assert_eq!(
            completed[0].response_data_frames.len(),
            3,
            "all three frames were collected before END_STREAM closed it"
        );
    }

    #[test]
    fn sse_terminator_detection_boundaries() {
        assert!(response_sse_stream_ended(b"data: [DONE]\n\n"));
        assert!(response_sse_stream_ended(b"data:[DONE]\n\n"));
        assert!(response_sse_stream_ended(b"data: [END]\n\n"));
        // Embedded in a multi-event frame, not just at the end.
        assert!(response_sse_stream_ended(
            b"data: {\"a\":1}\n\ndata: [DONE]\n\n"
        ));
        // A payload merely mentioning the token is not a terminator.
        assert!(!response_sse_stream_ended(
            b"data: {\"text\":\"the [DONE] marker\"}\n\n"
        ));
        assert!(!response_sse_stream_ended(b"data: {\"delta\":\"x\"}\n\n"));
        assert!(!response_sse_stream_ended(b""));
        // Model output whose content is the literal terminator text must NOT
        // close the stream — the `data: [DONE]` sits mid-JSON, not at a line
        // start. This is the false-positive the line-boundary check prevents.
        assert!(!response_sse_stream_ended(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"data: [DONE]\"}}]}\n\n"
        ));
    }

    #[test]
    fn test_aggregator_process_request_response() {
        let mut aggregator = Http2StreamAggregator::new();
        let _conn_id = ConnectionId {
            pid: 1234,
            ssl_ptr: 0x1000,
        };

        // Create request HEADERS frame (rw=1, write) with END_STREAM (no body)
        let req_event = create_test_event(1234, 0x1000, 1, 1000);
        let req_headers = create_test_frame(
            1,    // stream_id
            1,    // HEADERS
            0x05, // END_HEADERS | END_STREAM - request has no body
            b":method: POST\n:path: /api/test".to_vec(),
            req_event,
        );

        // Process request
        let completed = aggregator.process_frames(vec![req_headers]);
        assert!(completed.is_empty()); // Request complete but waiting for response
        assert_eq!(aggregator.active_stream_count(), 1);

        // Create response HEADERS frame (rw=0, read)
        let resp_event = create_test_event(1234, 0x1000, 0, 2000);
        let resp_headers = create_test_frame(
            1,    // stream_id
            1,    // HEADERS
            0x05, // END_HEADERS | END_STREAM
            b":status: 200".to_vec(),
            resp_event,
        );

        // Process response
        let completed = aggregator.process_frames(vec![resp_headers]);
        assert_eq!(completed.len(), 1);

        let stream = &completed[0];
        assert_eq!(stream.stream_id.stream_id, 1);
        assert!(stream.request_complete);
        assert!(stream.response_complete);
        assert!(stream.is_complete());
    }

    #[test]
    fn test_aggregator_with_data_frames() {
        let mut aggregator = Http2StreamAggregator::new();

        // Request HEADERS (no END_STREAM, expecting body) - rw=1 for request
        let req_event = create_test_event(1234, 0x1000, 1, 1000);
        let req_headers = create_test_frame(1, 1, 0x04, vec![], req_event.clone());

        // Request DATA with END_STREAM
        let req_data = create_test_frame(1, 0, 0x01, b"{\"key\":\"value\"}".to_vec(), req_event);

        // Process request
        let completed = aggregator.process_frames(vec![req_headers, req_data]);
        assert!(completed.is_empty()); // Still waiting for response

        // Response HEADERS with END_STREAM (no body) - rw=0 for response
        let resp_event = create_test_event(1234, 0x1000, 0, 2000);
        let resp_headers = create_test_frame(1, 1, 0x05, b":status: 200".to_vec(), resp_event);

        let completed = aggregator.process_frames(vec![resp_headers]);
        assert_eq!(completed.len(), 1);

        let stream = &completed[0];
        assert_eq!(stream.request_data_frames.len(), 1);
        assert_eq!(stream.response_data_frames.len(), 0);
    }

    #[test]
    fn test_metrics_include_http2_stream_payload_and_lru_evictions() {
        let mut aggregator = Http2StreamAggregator::with_capacity(1);
        let first = create_test_frame(
            1,
            0,
            0,
            b"one".to_vec(),
            create_test_event(1234, 0x1000, 1, 1000),
        );
        aggregator.process_frames(vec![first]);

        assert_eq!(
            aggregator.metrics(),
            ConnectionMetrics {
                connection_cache_bytes: 3,
                pending_connection_count: 1,
                pending_connection_bytes: 3,
                eviction_count: 0,
            }
        );

        let second = create_test_frame(
            3,
            0,
            0,
            b"second".to_vec(),
            create_test_event(1234, 0x1000, 1, 2000),
        );
        aggregator.process_frames(vec![second]);

        assert_eq!(
            aggregator.metrics(),
            ConnectionMetrics {
                connection_cache_bytes: 6,
                pending_connection_count: 1,
                pending_connection_bytes: 6,
                eviction_count: 1,
            }
        );
    }

    #[test]
    fn test_metrics_include_continuation_buffer_and_cleanup_evicted_stream() {
        let mut aggregator = Http2StreamAggregator::with_capacity(1);
        let first = create_test_frame(
            1,
            1,
            0,
            b"first".to_vec(),
            create_test_event(1234, 0x1000, 1, 1000),
        );
        aggregator.process_frames(vec![first]);

        let metrics = aggregator.metrics();
        assert_eq!(metrics.pending_connection_bytes, 5);
        assert_eq!(metrics.connection_cache_bytes, 10);

        let second = create_test_frame(
            3,
            1,
            0,
            b"next".to_vec(),
            create_test_event(1234, 0x1000, 1, 2000),
        );
        aggregator.process_frames(vec![second]);

        let metrics = aggregator.metrics();
        assert_eq!(metrics.pending_connection_bytes, 4);
        assert_eq!(metrics.connection_cache_bytes, 8);
        assert_eq!(metrics.eviction_count, 1);
        assert!(
            !aggregator.continuation_buffers.contains_key(&StreamId::new(
                ConnectionId {
                    pid: 1234,
                    ssl_ptr: 0x1000,
                },
                1,
            ))
        );
    }

    // --- HPACK stateful decode tests ---

    #[test]
    fn test_strip_headers_framing_bare() {
        let payload = b"\x82\x86\x84";
        assert_eq!(strip_headers_framing(payload, 0x00), payload.as_slice());
    }

    #[test]
    fn test_strip_headers_framing_padded() {
        // PADDED flag = 0x08: first byte = pad_length, last N bytes = padding
        let mut payload = vec![3]; // pad_length = 3
        payload.extend_from_slice(b"\x82\x86\x84"); // header block fragment
        payload.extend_from_slice(&[0, 0, 0]); // 3 bytes of padding
        let result = strip_headers_framing(&payload, 0x08);
        assert_eq!(result, b"\x82\x86\x84");
    }

    #[test]
    fn test_strip_headers_framing_priority() {
        // PRIORITY flag = 0x20: 5 bytes (4-byte dependency + 1 byte weight)
        let mut payload = vec![0x80, 0x00, 0x00, 0x01, 0x10]; // priority data
        payload.extend_from_slice(b"\x82\x86"); // header block fragment
        let result = strip_headers_framing(&payload, 0x20);
        assert_eq!(result, b"\x82\x86");
    }

    #[test]
    fn test_strip_headers_framing_padded_and_priority() {
        // Both PADDED (0x08) and PRIORITY (0x20) = 0x28
        let mut payload = vec![2]; // pad_length = 2
        payload.extend_from_slice(&[0x80, 0x00, 0x00, 0x01, 0x10]); // priority
        payload.extend_from_slice(b"\x82"); // header block fragment
        payload.extend_from_slice(&[0, 0]); // 2 bytes padding
        let result = strip_headers_framing(&payload, 0x28);
        assert_eq!(result, b"\x82");
    }

    #[test]
    fn test_strip_headers_framing_empty_after_strip() {
        // Only padding, no actual content
        let payload = vec![5, 0, 0, 0, 0, 0]; // pad_length=5, then 5 bytes padding
        let result = strip_headers_framing(&payload, 0x08);
        assert_eq!(result, &[] as &[u8]);
    }

    #[test]
    fn test_stateful_hpack_decode_static_table() {
        // Use hpack::Encoder to produce valid HPACK blocks
        let mut encoder = Encoder::new();
        let headers = [
            (b":method".to_vec(), b"POST".to_vec()),
            (b":path".to_vec(), b"/v1/chat/completions".to_vec()),
            (b":scheme".to_vec(), b"https".to_vec()),
        ];
        let encoded = encoder.encode(headers.iter().map(|(n, v)| (&n[..], &v[..])));

        let mut aggregator = Http2StreamAggregator::new();
        let conn_id = ConnectionId {
            pid: 100,
            ssl_ptr: 0x2000,
        };
        let decoded = aggregator.decode_header_block(conn_id, StreamDirection::Request, &encoded);

        assert!(decoded.is_some());
        let hdrs = decoded.unwrap();
        assert_eq!(hdrs.iter().find(|(n, _)| n == ":method").unwrap().1, "POST");
        assert_eq!(
            hdrs.iter().find(|(n, _)| n == ":path").unwrap().1,
            "/v1/chat/completions"
        );
        assert_eq!(
            hdrs.iter().find(|(n, _)| n == ":scheme").unwrap().1,
            "https"
        );
    }

    #[test]
    fn test_stateful_hpack_decode_dynamic_table() {
        // Verify that the second request using dynamic table refs decodes correctly
        let mut encoder = Encoder::new();
        let conn_id = ConnectionId {
            pid: 200,
            ssl_ptr: 0x3000,
        };
        let mut aggregator = Http2StreamAggregator::new();

        // First request: headers get added to dynamic table
        let headers1 = [
            (b":method".to_vec(), b"POST".to_vec()),
            (b":path".to_vec(), b"/v1/chat/completions".to_vec()),
            (b"authorization".to_vec(), b"Bearer sk-test123".to_vec()),
        ];
        let encoded1 = encoder.encode(headers1.iter().map(|(n, v)| (&n[..], &v[..])));
        let decoded1 = aggregator.decode_header_block(conn_id, StreamDirection::Request, &encoded1);
        assert!(decoded1.is_some());

        // Second request: encoder reuses dynamic table entries (shorter encoding)
        let headers2 = [
            (b":method".to_vec(), b"POST".to_vec()),
            (b":path".to_vec(), b"/v1/chat/completions".to_vec()),
            (b"authorization".to_vec(), b"Bearer sk-test123".to_vec()),
        ];
        let encoded2 = encoder.encode(headers2.iter().map(|(n, v)| (&n[..], &v[..])));
        // Second encoding should be shorter due to dynamic table
        assert!(encoded2.len() <= encoded1.len());

        let decoded2 = aggregator.decode_header_block(conn_id, StreamDirection::Request, &encoded2);
        assert!(decoded2.is_some());
        let hdrs = decoded2.unwrap();
        assert_eq!(
            hdrs.iter().find(|(n, _)| n == ":path").unwrap().1,
            "/v1/chat/completions"
        );
        assert_eq!(
            hdrs.iter().find(|(n, _)| n == "authorization").unwrap().1,
            "Bearer sk-test123"
        );
    }

    #[test]
    fn test_stateful_hpack_error_recovery() {
        let mut aggregator = Http2StreamAggregator::new();
        let conn_id = ConnectionId {
            pid: 300,
            ssl_ptr: 0x4000,
        };

        // Feed corrupt data — should fail and reset decoder
        let corrupt = vec![0xFF, 0xFF, 0xFF, 0xFF];
        let decoded = aggregator.decode_header_block(conn_id, StreamDirection::Request, &corrupt);
        assert!(decoded.is_none());

        // After reset, valid HPACK should decode fine
        let mut encoder = Encoder::new();
        let headers = [
            (b":method".to_vec(), b"GET".to_vec()),
            (b":path".to_vec(), b"/health".to_vec()),
        ];
        let encoded = encoder.encode(headers.iter().map(|(n, v)| (&n[..], &v[..])));
        let decoded = aggregator.decode_header_block(conn_id, StreamDirection::Request, &encoded);
        assert!(decoded.is_some());
        let hdrs = decoded.unwrap();
        assert_eq!(hdrs.iter().find(|(n, _)| n == ":method").unwrap().1, "GET");
    }

    #[test]
    fn test_continuation_reassembly() {
        let mut aggregator = Http2StreamAggregator::new();
        let mut encoder = Encoder::new();

        let headers = [
            (b":method".to_vec(), b"POST".to_vec()),
            (b":path".to_vec(), b"/v1/chat/completions".to_vec()),
            (b":scheme".to_vec(), b"https".to_vec()),
            (b"content-type".to_vec(), b"application/json".to_vec()),
        ];
        let encoded = encoder.encode(headers.iter().map(|(n, v)| (&n[..], &v[..])));

        // Split encoded block into two parts
        let mid = encoded.len() / 2;
        let part1 = &encoded[..mid];
        let part2 = &encoded[mid..];

        // HEADERS frame without END_HEADERS (flags=0x00), with END_STREAM (0x01)
        let req_event = create_test_event(400, 0x5000, 1, 1000);
        let headers_frame = create_test_frame(1, 1, 0x01, part1.to_vec(), req_event.clone());

        // CONTINUATION frame with END_HEADERS (flags=0x04)
        let cont_frame = create_test_frame(1, 9, 0x04, part2.to_vec(), req_event);

        // Process: HEADERS then CONTINUATION
        let completed = aggregator.process_frames(vec![headers_frame, cont_frame]);
        assert!(completed.is_empty()); // Still waiting for response

        // Send response to complete the stream
        let mut resp_encoder = Encoder::new();
        let resp_headers = [(b":status".to_vec(), b"200".to_vec())];
        let resp_encoded = resp_encoder.encode(resp_headers.iter().map(|(n, v)| (&n[..], &v[..])));
        let resp_event = create_test_event(400, 0x5000, 0, 2000);
        let resp_frame = create_test_frame(1, 1, 0x05, resp_encoded, resp_event);

        let completed = aggregator.process_frames(vec![resp_frame]);
        assert_eq!(completed.len(), 1);

        let stream = &completed[0];
        // Decoded request headers should be available
        assert!(stream.decoded_request_headers.is_some());
        let req_hdrs = stream.decoded_request_headers.as_ref().unwrap();
        assert_eq!(
            req_hdrs.iter().find(|(n, _)| n == ":method").unwrap().1,
            "POST"
        );
        assert_eq!(
            req_hdrs.iter().find(|(n, _)| n == ":path").unwrap().1,
            "/v1/chat/completions"
        );
        assert_eq!(
            req_hdrs
                .iter()
                .find(|(n, _)| n == "content-type")
                .unwrap()
                .1,
            "application/json"
        );
    }

    #[test]
    fn test_settings_table_size_update() {
        let mut aggregator = Http2StreamAggregator::new();
        let conn_id = ConnectionId {
            pid: 500,
            ssl_ptr: 0x6000,
        };

        // SETTINGS frame with HEADER_TABLE_SIZE = 0 (disable dynamic table)
        // Format: 2-byte id (0x0001) + 4-byte value (0x00000000)
        let settings_payload = vec![0x00, 0x01, 0x00, 0x00, 0x00, 0x00];
        let settings_event = create_test_event(500, 0x6000, 0, 1000); // from server
        let settings_frame = create_test_frame(0, 4, 0x00, settings_payload, settings_event);

        aggregator.process_frames(vec![settings_frame]);

        // The decoder should now have table_size=0
        // Encode with literal-only (since table_size=0 the encoder won't add to dynamic table)
        let mut encoder = Encoder::new();
        let headers = [(b":status".to_vec(), b"200".to_vec())];
        let encoded = encoder.encode(headers.iter().map(|(n, v)| (&n[..], &v[..])));
        let decoded = aggregator.decode_header_block(conn_id, StreamDirection::Response, &encoded);
        assert!(decoded.is_some());
        assert_eq!(
            decoded
                .unwrap()
                .iter()
                .find(|(n, _)| n == ":status")
                .unwrap()
                .1,
            "200"
        );
    }

    #[test]
    fn test_full_hpack_request_response_with_decoded_headers() {
        let mut aggregator = Http2StreamAggregator::new();
        let mut req_encoder = Encoder::new();
        let mut resp_encoder = Encoder::new();

        // Encode request headers
        let req_headers = [
            (b":method".to_vec(), b"POST".to_vec()),
            (b":path".to_vec(), b"/v1/chat/completions".to_vec()),
            (b":scheme".to_vec(), b"https".to_vec()),
        ];
        let req_encoded = req_encoder.encode(req_headers.iter().map(|(n, v)| (&n[..], &v[..])));

        // Request HEADERS with END_HEADERS (0x04), no END_STREAM (body follows)
        let req_event = create_test_event(600, 0x7000, 1, 1000);
        let req_hdr_frame = create_test_frame(3, 1, 0x04, req_encoded, req_event.clone());

        // Request DATA with END_STREAM
        let req_data_frame = create_test_frame(
            3,
            0,
            0x01,
            b"{\"model\":\"qwen\",\"messages\":[]}".to_vec(),
            req_event,
        );

        aggregator.process_frames(vec![req_hdr_frame, req_data_frame]);

        // Encode response headers
        let resp_headers = [
            (b":status".to_vec(), b"200".to_vec()),
            (b"content-type".to_vec(), b"application/json".to_vec()),
        ];
        let resp_encoded = resp_encoder.encode(resp_headers.iter().map(|(n, v)| (&n[..], &v[..])));

        // Response HEADERS with END_HEADERS (0x04), no END_STREAM
        let resp_event = create_test_event(600, 0x7000, 0, 2000);
        let resp_hdr_frame = create_test_frame(3, 1, 0x04, resp_encoded, resp_event.clone());

        // Response DATA with END_STREAM
        let resp_data_frame = create_test_frame(
            3,
            0,
            0x01,
            b"{\"id\":\"chatcmpl-1\",\"choices\":[]}".to_vec(),
            resp_event,
        );

        let completed = aggregator.process_frames(vec![resp_hdr_frame, resp_data_frame]);
        assert_eq!(completed.len(), 1);

        let stream = &completed[0];
        // Verify decoded headers are attached
        assert!(stream.decoded_request_headers.is_some());
        assert!(stream.decoded_response_headers.is_some());

        // path()/method()/status_code() should use decoded headers
        assert_eq!(stream.path(), "/v1/chat/completions");
        assert_eq!(stream.method(), "POST");
        assert_eq!(stream.status_code(), 200);
    }

    #[test]
    fn test_independent_req_resp_decoders() {
        // Request and response decoders are independent per connection
        let mut aggregator = Http2StreamAggregator::new();
        let conn_id = ConnectionId {
            pid: 700,
            ssl_ptr: 0x8000,
        };

        let mut req_encoder = Encoder::new();
        let mut resp_encoder = Encoder::new();

        // Request direction decode
        let req_h = [(b":method".to_vec(), b"GET".to_vec())];
        let req_enc = req_encoder.encode(req_h.iter().map(|(n, v)| (&n[..], &v[..])));
        let req_dec = aggregator.decode_header_block(conn_id, StreamDirection::Request, &req_enc);
        assert!(req_dec.is_some());

        // Response direction decode (independent table)
        let resp_h = [(b":status".to_vec(), b"404".to_vec())];
        let resp_enc = resp_encoder.encode(resp_h.iter().map(|(n, v)| (&n[..], &v[..])));
        let resp_dec =
            aggregator.decode_header_block(conn_id, StreamDirection::Response, &resp_enc);
        assert!(resp_dec.is_some());
        assert_eq!(
            resp_dec.unwrap()[0],
            (":status".to_string(), "404".to_string())
        );
    }
}
