//! HTTP Connection Aggregator - correlates HTTP requests with responses
//
//! This module implements the HTTP Aggregator specification for correlating
//! parsed HTTP requests and responses into complete request/response pairs.

use super::super::result::AggregatedResult;
use super::pair::HttpPair;
#[path = "pending_response.rs"]
mod pending_response;
use super::response::AggregatedResponse;
use crate::config::DEFAULT_CONNECTION_CAPACITY;
use crate::parser::http::{HttpParser, ParsedHttpMessage, ParsedRequest, ParsedResponse};
use crate::parser::sse::{ParsedSseEvent, SseParser};
use crate::probes::sslsniff::SslEvent;
use crate::utils::decompress::ZstdStreamDecoder;
use lru::LruCache;
use pending_response::PendingResponse;
use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Hard cap on a *compressed* SSE buffer awaiting completion. A malicious or
/// buggy stream that never sends a chunk terminator would otherwise grow this
/// buffer unboundedly — the decompression output cap in `utils::decompress`
/// does not help here because decoding only runs once the stream completes.
/// 8 MiB of compressed SSE is far beyond any real stream; on overflow the
/// stream is finalized best-effort so memory stays bounded.
const MAX_COMPRESSED_SSE_BUFFER: usize = 8 * 1024 * 1024;

/// Connection identifier - uniquely identifies an SSL connection
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub struct ConnectionId {
    pub pid: u32,
    pub ssl_ptr: u64,
}

impl ConnectionId {
    /// Create from SslEvent
    pub fn from_ssl_event(event: &SslEvent) -> Self {
        ConnectionId {
            pid: event.pid,
            ssl_ptr: event.ssl_ptr,
        }
    }
}

/// Connection state machine
///
/// `pub(crate)`: this is aggregation-internal state (review F3) — no
/// crate-external consumer exists, and keeping it crate-private lets fields
/// evolve (e.g. the incremental zstd decoder) without breaking the lib API.
#[derive(Debug, Clone)]
pub(crate) enum ConnectionState {
    /// Idle - waiting for request
    ///
    /// Never constructed by the live state machine (states are removed, not
    /// reset, on completion) but still matched by drain/snapshot paths and
    /// constructed by tests; kept so the machine stays explicit about it.
    #[allow(dead_code)]
    Idle,
    /// Request pending - waiting for response
    RequestPending { request: ParsedRequest },
    /// Request body pending - body not yet complete, waiting for more data or response
    RequestBodyPending {
        request: ParsedRequest,
        expected_body_len: Option<usize>,
        body_buffer: Vec<u8>,
    },
    /// HTTP response headers or body awaiting further SSL reads.
    ResponsePending {
        request: Option<ParsedRequest>,
        assembly: PendingResponse,
    },
    /// SSE active - response headers received, body streaming
    SseActive {
        request: Option<ParsedRequest>,
        response_headers: ParsedResponse,
        sse_events: Vec<ParsedSseEvent>,
        /// Raw (chunk-framed, still content-encoded) body bytes, buffered for a
        /// *compressed* SSE stream and decoded once the stream completes.
        /// `None` means an uncompressed stream parsed live via `process_sse_event`
        /// (unchanged legacy behaviour).
        compressed_buffer: Option<Vec<u8>>,
        /// Response `Content-Encoding`, used to decode `compressed_buffer`.
        content_encoding: Option<String>,
        /// Incremental zstd decoder fed as compressed fragments arrive, so a
        /// stream whose frame never closes (HTTP/2 framing, missing chunked
        /// terminator) still decodes from the blocks flushed so far (#2267).
        /// `Rc<RefCell<..>>` because this enum must stay `Clone` for the
        /// snapshot/drain paths while the underlying zstd context is not
        /// clonable — clones deliberately share the one live decoder, which
        /// is also what lets a drain snapshot see the latest decoded bytes.
        zstd_decoder: Option<Rc<RefCell<ZstdStreamDecoder>>>,
    },
}

impl ConnectionState {
    /// Estimate payload bytes retained specifically for request/response
    /// correlation. Container overhead is excluded, so the value remains a
    /// stable diagnostic rather than allocator-specific heap accounting.
    fn buffered_bytes(&self) -> usize {
        fn request_bytes(request: &ParsedRequest) -> usize {
            request
                .reassembled_body
                .as_ref()
                .map_or(request.body_len, Vec::len)
        }

        match self {
            Self::Idle => 0,
            Self::RequestPending { request } => request_bytes(request),
            Self::RequestBodyPending { body_buffer, .. } => body_buffer.len(),
            Self::ResponsePending { request, assembly } => request
                .as_ref()
                .map_or(0, request_bytes)
                .saturating_add(assembly.buffered_bytes()),
            Self::SseActive {
                request,
                response_headers,
                sse_events,
                compressed_buffer,
                ..
            } => {
                request.as_ref().map_or(0, request_bytes)
                    + response_headers.body_len
                    + sse_events
                        .iter()
                        .map(|event| event.data().len())
                        .sum::<usize>()
                    + compressed_buffer.as_ref().map_or(0, Vec::len)
            }
        }
    }
}

/// Snapshot of memory and lifecycle state retained by HTTP correlation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ConnectionMetrics {
    /// Estimated payload bytes retained across all connection caches.
    pub connection_cache_bytes: usize,
    /// In-flight non-idle connections awaiting correlation or completion.
    pub pending_connection_count: usize,
    /// Estimated payload bytes retained by in-flight connections.
    pub pending_connection_bytes: usize,
    /// Cumulative LRU, TTL, and oversized-body evictions.
    pub eviction_count: u64,
}

impl ConnectionMetrics {
    /// Combine independently retained HTTP/1 and HTTP/2 correlation state.
    pub(crate) fn saturating_add(self, other: Self) -> Self {
        Self {
            connection_cache_bytes: self
                .connection_cache_bytes
                .saturating_add(other.connection_cache_bytes),
            pending_connection_count: self
                .pending_connection_count
                .saturating_add(other.pending_connection_count),
            pending_connection_bytes: self
                .pending_connection_bytes
                .saturating_add(other.pending_connection_bytes),
            eviction_count: self.eviction_count.saturating_add(other.eviction_count),
        }
    }
}

/// HTTP Connection Aggregator
#[derive(Debug)]
pub struct HttpConnectionAggregator {
    connections: LruCache<ConnectionId, ConnectionState>,
    /// Raw bytes received as RawData while the connection is in SseActive
    /// state. Some providers (e.g. OpenAI Responses API via dashscope) emit
    /// a final `response.completed` SSE event whose data payload spans
    /// multiple TLS records: the first chunk parses as a SseEvent (with
    /// truncated data), and subsequent chunks have no `data:` prefix so
    /// they arrive as RawData. Buffering them lets us reconstruct the
    /// original event for token-usage extraction when the stream ends.
    sse_continuation_buffers: LruCache<ConnectionId, Vec<u8>>,
    /// Last `source_event` pointer appended into the continuation buffer per
    /// connection. Used to dedup when a single SSL_read produces multiple
    /// ParsedSseEvents that share the same source SslEvent buffer.
    last_appended_src_ptr: LruCache<ConnectionId, usize>,
    /// Last activity timestamp per connection, used for idle timeout eviction.
    last_activity: LruCache<ConnectionId, Instant>,
    /// Connections already snapshotted after idle timeout.
    idle_snapshotted: LruCache<ConnectionId, ()>,
    /// Maximum bytes buffered per connection (request body or compressed SSE).
    max_body_bytes: usize,
    /// Idle timeout before a connection state is forcibly dropped.
    idle_timeout: Duration,
    /// Cumulative automatic eviction count for diagnostics.
    eviction_count: u64,
}

/// Returns true if oversized-SSE-event continuation buffering should run for
/// this SSE stream. Currently only the OpenAI Responses API
/// (`/v1/responses`, dashscope `/compatible-mode/v1/responses`) emits a
/// final `response.completed` event whose data field routinely spans
/// multiple TLS records, so we restrict the extra buffering to that path.
///
/// Matching is intentionally precise: `ends_with("/responses")` catches
/// exact path endings (covers both `/v1/responses` and
/// `/compatible-mode/v1/responses`), while `contains("/responses?")`
/// catches query-string variants. We must NOT use a broad `contains`
/// because sub-paths like `/v1/responses/{id}/items` would be false
/// positives.
fn needs_sse_continuation_buffer(request: Option<&ParsedRequest>) -> bool {
    let Some(req) = request else {
        return false;
    };
    let path = req.path.as_str();
    path.ends_with("/responses") || path.contains("/responses?")
}

/// Default per-connection body buffer cap (8 MiB).
const DEFAULT_MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Default idle timeout for connection states (60 s).
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

impl Default for HttpConnectionAggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpConnectionAggregator {
    /// Create a new aggregator with default capacity and limits.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CONNECTION_CAPACITY)
    }

    /// Create a new aggregator with custom capacity and default limits.
    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_limits(capacity, DEFAULT_MAX_BODY_BYTES, DEFAULT_IDLE_TIMEOUT)
    }

    /// Create a new aggregator with explicit capacity and memory/time limits.
    pub fn with_limits(capacity: usize, max_body_bytes: usize, idle_timeout: Duration) -> Self {
        // A zero capacity has no meaningful LRU; clamp to one so a
        // misconfigured caller gets maximum eviction instead of a panic —
        // the same graceful handling max_body_bytes receives below.
        let cap = NonZeroUsize::new(capacity.max(1)).unwrap_or(NonZeroUsize::MIN);
        HttpConnectionAggregator {
            connections: LruCache::new(cap),
            sse_continuation_buffers: LruCache::new(cap),
            last_appended_src_ptr: LruCache::new(cap),
            last_activity: LruCache::new(cap),
            idle_snapshotted: LruCache::new(cap),
            max_body_bytes: max_body_bytes.max(1024),
            idle_timeout,
            eviction_count: 0,
        }
    }

    /// Current per-connection body buffer cap.
    pub fn max_body_bytes(&self) -> usize {
        self.max_body_bytes
    }

    /// Insert connection state, logging if an unrelated entry is evicted by LRU.
    /// Also records activity timestamp for idle eviction.
    fn insert(&mut self, key: ConnectionId, state: ConnectionState) {
        self.touch(&key);
        if let Some((evicted_key, evicted_state)) = self.connections.push(key, state) {
            if evicted_key != key {
                self.eviction_count = self.eviction_count.saturating_add(1);
                self.sse_continuation_buffers.pop(&evicted_key);
                self.last_appended_src_ptr.pop(&evicted_key);
                // The side caches are keyed by connection as well. Their own LRU
                // would eventually drop these entries, but until then they hold
                // up to 1 MiB of continuation buffer per evicted connection.
                log::warn!(
                    "[HttpAggregator] LRU evicted conn={:?} state={} | capacity={}",
                    evicted_key,
                    match evicted_state {
                        ConnectionState::Idle => "Idle",
                        ConnectionState::RequestPending { .. } => "RequestPending",
                        ConnectionState::ResponsePending { .. } => "ResponsePending",
                        ConnectionState::RequestBodyPending { .. } => "RequestBodyPending",
                        ConnectionState::SseActive { .. } => "SseActive",
                    },
                    self.connections.cap(),
                );
            }
        }
    }

    /// Evict discardable connections that have been idle for longer than
    /// `self.idle_timeout` or whose buffered body exceeds `self.max_body_bytes`.
    ///
    /// In-flight calls retain request evidence for interruption snapshots and
    /// can still complete if more response bytes arrive after the idle timeout.
    pub fn evict_idle_and_oversized(&mut self) {
        let now = Instant::now();
        let timeout = self.idle_timeout;
        let max_bytes = self.max_body_bytes;

        // Collect keys to evict (cannot mutate while iterating).
        let to_evict: Vec<ConnectionId> = self
            .last_activity
            .iter()
            .filter_map(|(k, ts)| {
                if now.duration_since(*ts) > timeout {
                    Some(*k)
                } else {
                    None
                }
            })
            .collect();

        let mut evicted_idle = 0usize;
        for key in &to_evict {
            if matches!(
                self.connections.peek(key),
                Some(
                    ConnectionState::Idle | ConnectionState::ResponsePending { request: None, .. }
                )
            ) {
                if matches!(
                    self.connections.peek(key),
                    Some(ConnectionState::ResponsePending { .. })
                ) {
                    log::warn!(
                        "[HttpAggregator] discarded incomplete idle response | conn={key:?}"
                    );
                }
                self.connections.pop(key);
                self.sse_continuation_buffers.pop(key);
                self.last_appended_src_ptr.pop(key);
                self.last_activity.pop(key);
                self.idle_snapshotted.pop(key);
                evicted_idle += 1;
            }
        }
        if evicted_idle > 0 {
            self.eviction_count = self.eviction_count.saturating_add(evicted_idle as u64);
            log::info!(
                "[HttpAggregator] evicted {} idle connections (timeout={}s)",
                evicted_idle,
                timeout.as_secs()
            );
        }

        // Also evict oversized body buffers.
        let oversized: Vec<ConnectionId> = self
            .connections
            .iter()
            .filter_map(|(k, state)| {
                let body_len = match state {
                    ConnectionState::RequestBodyPending { body_buffer, .. } => body_buffer.len(),
                    _ => 0,
                };
                if body_len > max_bytes { Some(*k) } else { None }
            })
            .collect();

        for key in &oversized {
            self.connections.pop(key);
            self.sse_continuation_buffers.pop(key);
            self.last_appended_src_ptr.pop(key);
            self.last_activity.pop(key);
            self.idle_snapshotted.pop(key);
        }
        if !oversized.is_empty() {
            self.eviction_count = self.eviction_count.saturating_add(oversized.len() as u64);
            log::warn!(
                "[HttpAggregator] evicted {} oversized connections (max_body={}B)",
                oversized.len(),
                max_bytes
            );
        }
    }

    /// Snapshot non-idle connections that exceeded `idle_timeout`.
    ///
    /// These states can represent manually interrupted or abandoned LLM
    /// streams. Returning cloned states lets the caller persist a pending GenAI
    /// row while keeping the live connection available if the stream resumes.
    pub(crate) fn snapshot_idle_connections(&mut self) -> Vec<(ConnectionId, ConnectionState)> {
        let now = Instant::now();
        let timeout = self.idle_timeout;
        let keys: Vec<ConnectionId> = self
            .last_activity
            .iter()
            .filter_map(|(k, ts)| {
                if now.duration_since(*ts) > timeout {
                    Some(*k)
                } else {
                    None
                }
            })
            .collect();

        let mut result = Vec::new();
        for key in keys {
            if self.idle_snapshotted.peek(&key).is_some() {
                continue;
            }
            if let Some(state) = self.connections.peek(&key) {
                match state.clone() {
                    ConnectionState::Idle => {}
                    state => {
                        self.idle_snapshotted.push(key, ());
                        result.push((key, state));
                    }
                }
            }
        }

        if !result.is_empty() {
            log::info!(
                "[HttpAggregator] snapshotted {} idle in-flight connection(s) (timeout={}s)",
                result.len(),
                timeout.as_secs()
            );
        }

        result
    }

    /// Backward-compatible alias for idle snapshots.
    ///
    /// No in-crate caller today; kept (dead_code-allowed) because narrowing
    /// visibility for review F3 must not silently drop API surface that the
    /// alias was published for.
    #[allow(dead_code)]
    pub(crate) fn drain_idle_connections(&mut self) -> Vec<(ConnectionId, ConnectionState)> {
        self.snapshot_idle_connections()
    }

    /// Record activity timestamp for a connection.
    fn touch(&mut self, key: &ConnectionId) {
        self.last_activity.push(*key, Instant::now());
    }

    /// Parse initial SSE body bytes from the first HTTP response packet.
    ///
    /// When HTTP response headers and the first SSE `data:` chunk arrive in the
    /// same SSL_read buffer, the parser only emits `ParsedResponse`. Downstream
    /// SSE analysis consumes `sse_events`, so we must convert the response body
    /// into initial `ParsedSseEvent`s before entering `SseActive`.
    fn initial_sse_events(response: &ParsedResponse) -> Vec<ParsedSseEvent> {
        let body = response.body();
        if body.is_empty() {
            return Vec::new();
        }

        let synthetic_event = std::rc::Rc::new(SslEvent {
            source: response.source_event.source,
            timestamp_ns: response.source_event.timestamp_ns,
            delta_ns: response.source_event.delta_ns,
            pid: response.source_event.pid,
            tid: response.source_event.tid,
            uid: response.source_event.uid,
            len: body.len() as u32,
            rw: response.source_event.rw,
            comm: response.source_event.comm.clone(),
            buf: body.to_vec(),
            is_handshake: response.source_event.is_handshake,
            ssl_ptr: response.source_event.ssl_ptr,
        });

        SseParser::new().parse(synthetic_event)
    }

    /// Decide the initial SSE state from response headers.
    ///
    /// Returns `(initial_sse_events, compressed_buffer, content_encoding,
    /// zstd_decoder)`. `compressed_buffer == Some(..)` marks a *compressed*
    /// stream whose body must be buffered and decoded at completion; `None`
    /// keeps the legacy live-parse path for uncompressed streams. Compression
    /// is detected from the `Content-Encoding` header, with a magic-byte
    /// fallback (gzip `1f 8b`, zstd `28 b5 2f fd`) for cases where the header
    /// is absent. `zstd_decoder == Some(..)` additionally enables incremental
    /// decoding for non-chunked zstd streams (#2267).
    fn sse_entry_state(
        response: &ParsedResponse,
    ) -> (
        Vec<ParsedSseEvent>,
        Option<Vec<u8>>,
        Option<String>,
        Option<Rc<RefCell<ZstdStreamDecoder>>>,
    ) {
        let enc = response.content_encoding().map(|e| e.trim().to_lowercase());
        let initial = response.body();
        // Magic-byte sniffing exists for HPACK-stateless HTTP/2 where the
        // header is unresolvable — it must not override an explicit non-zstd
        // Content-Encoding whose payload happens to start with the zstd magic
        // (review F2: that would route gzip/br bodies into the zstd decoder).
        let header_says_other = matches!(
            enc.as_deref(),
            Some("gzip") | Some("x-gzip") | Some("deflate") | Some("br")
        );
        let is_zstd = enc.as_deref() == Some("zstd")
            || (!header_says_other
                && initial.len() >= 4
                && initial[0] == 0x28
                && initial[1] == 0xb5
                && initial[2] == 0x2f
                && initial[3] == 0xfd);
        let compressed = matches!(
            enc.as_deref(),
            Some("gzip") | Some("x-gzip") | Some("deflate") | Some("zstd") | Some("br")
        ) || (initial.len() >= 2 && initial[0] == 0x1f && initial[1] == 0x8b)
            || is_zstd;

        if compressed {
            // Without chunked framing the raw bytes are a bare zstd stream, so
            // fragments can be decoded incrementally as they arrive. Deferred
            // whole-buffer decoding would need the frame epilogue, which a
            // gateway flushing per event only emits at stream end — or never,
            // when the stream is finalized by a drain path (#2267). The
            // chunk-framed path keeps the legacy decode-at-terminator flow
            // because its bytes are interleaved with chunk-size framing.
            let zstd_decoder = if is_zstd && !Self::is_chunked_response(response) {
                let mut decoder = ZstdStreamDecoder::new();
                decoder.feed(initial);
                Some(Rc::new(RefCell::new(decoder)))
            } else {
                None
            };
            (Vec::new(), Some(initial.to_vec()), enc, zstd_decoder)
        } else {
            (Self::initial_sse_events(response), None, enc, None)
        }
    }

    /// Finish a compressed SSE stream: de-frame chunked transfer-encoding,
    /// decompress the buffered body, parse it into SSE events, and build the
    /// aggregated result (mirrors the live `SseComplete` construction).
    /// Decode a buffered *compressed* SSE body into parsed events. Shared by the
    /// live completion path (`finish_compressed_sse`) and the drain path
    /// (`drain_and_persist_dead_connections`) so both decode identically. `src`
    /// supplies provenance (pid/tid/uid/comm/timestamps) for the synthetic event
    /// handed to the parser.
    pub(crate) fn decode_compressed_sse(
        raw_buffer: &[u8],
        content_encoding: Option<&str>,
        is_chunked: bool,
        zstd_decoder: Option<&RefCell<ZstdStreamDecoder>>,
        src: &SslEvent,
    ) -> Vec<ParsedSseEvent> {
        let body = if is_chunked {
            crate::utils::decompress::dechunk_body(raw_buffer)
        } else {
            raw_buffer.to_vec()
        };
        // Prefer the incremental decoder output: it recovers every block the
        // gateway flushed even when the zstd frame never closed, which
        // whole-buffer decompression cannot (#2267). Empty output means the
        // decoder never produced anything (init failure or poisoned before
        // the first block), so fall back to whole-buffer decoding, which
        // still handles the closed-frame case.
        let decoder_output = zstd_decoder.map(|d| d.borrow_mut().decoded_output());
        let decompressed = match decoder_output {
            Some(out) if !out.is_empty() => out,
            _ => crate::utils::decompress::decompress_body(&body, content_encoding),
        };
        let synthetic = std::rc::Rc::new(SslEvent {
            source: src.source,
            timestamp_ns: src.timestamp_ns,
            delta_ns: src.delta_ns,
            pid: src.pid,
            tid: src.tid,
            uid: src.uid,
            len: decompressed.len() as u32,
            rw: src.rw,
            comm: src.comm.clone(),
            buf: decompressed,
            is_handshake: src.is_handshake,
            ssl_ptr: src.ssl_ptr,
        });
        SseParser::new().parse(synthetic)
    }

    /// Whether a response declares chunked transfer-encoding.
    pub(crate) fn is_chunked_response(response_headers: &ParsedResponse) -> bool {
        response_headers
            .headers
            .get("transfer-encoding")
            .map(|v| v.to_lowercase().contains("chunked"))
            .unwrap_or(false)
    }

    fn finish_compressed_sse(
        connection_id: ConnectionId,
        request: Option<ParsedRequest>,
        response_headers: ParsedResponse,
        content_encoding: Option<String>,
        raw_buffer: Vec<u8>,
        zstd_decoder: Option<Rc<RefCell<ZstdStreamDecoder>>>,
    ) -> AggregatedResult {
        let is_chunked = Self::is_chunked_response(&response_headers);
        let sse_events = Self::decode_compressed_sse(
            &raw_buffer,
            content_encoding.as_deref(),
            is_chunked,
            zstd_decoder.as_deref(),
            &response_headers.source_event,
        );
        log::debug!(
            "[HttpAggregator] Decoded compressed SSE | conn={connection_id:?} | encoding={content_encoding:?} | events={}",
            sse_events.len(),
        );

        let mut response = AggregatedResponse::from_parsed(response_headers);
        response.set_sse_events(sse_events);

        if let Some(req) = request {
            let parsed = response.parsed.clone();
            let mut pair = HttpPair::from_parsed(connection_id, req, parsed);
            pair.response = response;
            AggregatedResult::SseComplete(pair)
        } else {
            AggregatedResult::ResponseOnly {
                connection_id,
                response,
            }
        }
    }

    /// Process HTTP Request (from HTTP Parser)
    pub fn process_request(&mut self, request: ParsedRequest) {
        let connection_id = ConnectionId::from_ssl_event(&request.source_event);
        self.idle_snapshotted.pop(&connection_id);

        if matches!(
            self.connections.peek(&connection_id),
            Some(ConnectionState::ResponsePending { .. })
        ) {
            log::warn!(
                "[HttpAggregator] new request replaced incomplete response | conn={connection_id:?}"
            );
        }

        // Check if body is complete by comparing with Content-Length
        let content_length: Option<usize> = request
            .headers
            .get("content-length")
            .and_then(|v| v.parse().ok());

        let body_complete = match content_length {
            Some(cl) => request.body_len >= cl,
            None => {
                // No Content-Length: check for Transfer-Encoding: chunked
                // RFC 7230 §3.3.1: the coding names are case-insensitive and
                // comma-separated, so compare tokens. `contains("chunked")`
                // missed `Chunked` and declared the body complete on the first
                // read, dropping every later body read.
                let is_chunked = request.headers.get("transfer-encoding").is_some_and(|v| {
                    v.split(',')
                        .any(|token| token.trim().eq_ignore_ascii_case("chunked"))
                });
                if is_chunked {
                    // Detect completion by walking the chunk framing, not by
                    // scanning for the terminator bytes: the 5-byte pattern can
                    // occur inside chunk *data* (a prompt quoting a chunked
                    // example), which ended the body mid-stream. Same helper the
                    // compressed-response path uses (e6c787258).
                    crate::utils::decompress::chunked_stream_complete(request.body())
                } else {
                    true // No Content-Length and not chunked → body is complete
                }
            }
        };

        if body_complete {
            log::trace!(
                "[HttpAggregator] State transition: -> RequestPending | conn={:?} | method={} | path={}",
                connection_id,
                request.method,
                request.path,
            );
            self.insert(connection_id, ConnectionState::RequestPending { request });
        } else {
            log::debug!(
                "[HttpAggregator] State transition: -> RequestBodyPending | conn={:?} | method={} | path={} | body_len={} | content_length={:?}",
                connection_id,
                request.method,
                request.path,
                request.body_len,
                content_length,
            );
            let initial_body = request.body().to_vec();
            self.insert(
                connection_id,
                ConnectionState::RequestBodyPending {
                    request,
                    expected_body_len: content_length,
                    body_buffer: initial_body,
                },
            );
        }
    }

    /// Process HTTP Response (from HTTP Parser)
    /// Returns completed HttpPair or SSE started signal
    pub fn process_response(&mut self, mut response: ParsedResponse) -> Option<AggregatedResult> {
        let connection_id = ConnectionId::from_ssl_event(&response.source_event);

        while (100..200).contains(&response.status_code) && response.status_code != 101 {
            if response.body().is_empty() {
                return None;
            }
            let mut event = (*response.source_event).clone();
            event.buf = response.body().to_vec();
            event.len = event.buf.len() as u32;
            match HttpParser::new().parse(Rc::new(event.clone())) {
                Ok(ParsedHttpMessage::Response(next)) => response = next,
                _ => return self.process_response_bytes(&event),
            }
        }
        let state = self.connections.pop(&connection_id)?;

        match state {
            ConnectionState::RequestBodyPending {
                request,
                expected_body_len,
                mut body_buffer,
            } => {
                // Response arrived → request must be complete (server replies only after full request)
                log::debug!(
                    "[HttpAggregator] State transition: RequestBodyPending -> Complete (response-driven) | conn={:?} | buffered={}",
                    connection_id,
                    body_buffer.len(),
                );
                if let Some(cl) = expected_body_len {
                    body_buffer.truncate(cl);
                }
                let mut completed_request = request;
                completed_request.reassembled_body = Some(body_buffer);

                if response.is_sse() {
                    let mut response_headers = response;
                    let (sse_events, compressed_buffer, content_encoding, zstd_decoder) =
                        Self::sse_entry_state(&response_headers);
                    response_headers.body_len = 0;
                    if let Some(buf) = &compressed_buffer {
                        if crate::utils::decompress::chunked_stream_complete(buf) {
                            return Some(Self::finish_compressed_sse(
                                connection_id,
                                Some(completed_request),
                                response_headers,
                                content_encoding,
                                buf.clone(),
                                zstd_decoder,
                            ));
                        }
                    }
                    self.insert(
                        connection_id,
                        ConnectionState::SseActive {
                            request: Some(completed_request),
                            response_headers,
                            sse_events,
                            compressed_buffer,
                            content_encoding,
                            zstd_decoder,
                        },
                    );
                    None
                } else {
                    self.start_response(connection_id, Some(completed_request), response)
                }
            }
            ConnectionState::RequestPending { request } => {
                if response.is_sse() {
                    log::trace!(
                        "[HttpAggregator] State transition: RequestPending -> SseActive | conn={:?} | status={}",
                        connection_id,
                        response.status_code,
                    );
                    let mut response_headers = response;
                    let (sse_events, compressed_buffer, content_encoding, zstd_decoder) =
                        Self::sse_entry_state(&response_headers);
                    response_headers.body_len = 0;
                    // Transition to SSE active state, wait for SSE events
                    if let Some(buf) = &compressed_buffer {
                        if crate::utils::decompress::chunked_stream_complete(buf) {
                            return Some(Self::finish_compressed_sse(
                                connection_id,
                                Some(request),
                                response_headers,
                                content_encoding,
                                buf.clone(),
                                zstd_decoder,
                            ));
                        }
                    }
                    self.insert(
                        connection_id,
                        ConnectionState::SseActive {
                            request: Some(request),
                            response_headers,
                            sse_events,
                            compressed_buffer,
                            content_encoding,
                            zstd_decoder,
                        },
                    );

                    // Don't return HttpPair yet, wait for SSE events to complete
                    None
                } else {
                    log::trace!(
                        "[HttpAggregator] State transition: RequestPending -> Complete | conn={:?} | status={}",
                        connection_id,
                        response.status_code,
                    );
                    self.start_response(connection_id, Some(request), response)
                }
            }
            ConnectionState::Idle => {
                if response.is_sse() {
                    // SSE response without prior request - still need to wait for SSE events
                    log::trace!(
                        "[HttpAggregator] State transition: Idle -> SseActive (no request) | conn={:?} | status={}",
                        connection_id,
                        response.status_code
                    );
                    let mut response_headers = response;
                    let (sse_events, compressed_buffer, content_encoding, zstd_decoder) =
                        Self::sse_entry_state(&response_headers);
                    response_headers.body_len = 0;
                    if let Some(buf) = &compressed_buffer {
                        if crate::utils::decompress::chunked_stream_complete(buf) {
                            return Some(Self::finish_compressed_sse(
                                connection_id,
                                None,
                                response_headers,
                                content_encoding,
                                buf.clone(),
                                zstd_decoder,
                            ));
                        }
                    }
                    self.insert(
                        connection_id,
                        ConnectionState::SseActive {
                            request: None,
                            response_headers,
                            sse_events,
                            compressed_buffer,
                            content_encoding,
                            zstd_decoder,
                        },
                    );
                    None
                } else {
                    log::trace!(
                        "[HttpAggregator] State transition: Idle -> ResponseOnly | conn={:?} | status={}",
                        connection_id,
                        response.status_code
                    );
                    self.start_response(connection_id, None, response)
                }
            }
            ConnectionState::SseActive { .. } | ConnectionState::ResponsePending { .. } => {
                log::trace!(
                    "[HttpAggregator] State transition: SseActive (unexpected response) | conn={connection_id:?}"
                );
                // Response on SSE connection - shouldn't happen normally
                // Restore state and return None
                self.insert(connection_id, state);
                None
            }
        }
    }

    /// Process raw body data (continuation bytes for an in-progress request)
    pub fn process_raw_body_data(&mut self, ssl_event: &SslEvent) -> Option<AggregatedResult> {
        let connection_id = ConnectionId::from_ssl_event(ssl_event);
        if self.accepts_response_bytes(ssl_event) || self.starts_response(ssl_event) {
            return self.process_response_bytes(ssl_event);
        }
        let state = self.connections.pop(&connection_id)?;

        match state {
            ConnectionState::RequestBodyPending {
                request,
                expected_body_len,
                mut body_buffer,
            } => {
                // Append new data to buffer
                let data = &ssl_event.buf[..ssl_event.buf_size() as usize];
                body_buffer.extend_from_slice(data);

                // Check if body is now complete
                let complete = match expected_body_len {
                    Some(cl) => body_buffer.len() >= cl,
                    None => {
                        // chunked: same framed check as process_request
                        crate::utils::decompress::chunked_stream_complete(&body_buffer)
                    }
                };

                if complete {
                    log::debug!(
                        "[HttpAggregator] State transition: RequestBodyPending -> RequestPending (body complete) | conn={:?} | total_body={}",
                        connection_id,
                        body_buffer.len(),
                    );
                    if let Some(cl) = expected_body_len {
                        body_buffer.truncate(cl);
                    }
                    let mut completed_request = request;
                    completed_request.reassembled_body = Some(body_buffer);
                    self.insert(
                        connection_id,
                        ConnectionState::RequestPending {
                            request: completed_request,
                        },
                    );
                } else {
                    log::trace!(
                        "[HttpAggregator] RequestBodyPending: buffered more data | conn={:?} | total={}",
                        connection_id,
                        body_buffer.len(),
                    );
                    self.insert(
                        connection_id,
                        ConnectionState::RequestBodyPending {
                            request,
                            expected_body_len,
                            body_buffer,
                        },
                    );
                }
                None
            }
            ConnectionState::SseActive {
                request,
                response_headers,
                sse_events,
                compressed_buffer: Some(mut buf),
                content_encoding,
                zstd_decoder,
            } => {
                // Compressed SSE body bytes: the parser forwards them as RawData
                // because they don't parse as SSE text. Buffer until the chunked
                // terminator, then de-frame + decompress + parse.
                let data = &ssl_event.buf[..ssl_event.buf_size() as usize];
                // Also decode incrementally so a zstd frame that never closes
                // (HTTP/2, missing terminator) still yields its events (#2267).
                // The raw buffer is kept in parallel: it feeds the chunked
                // completion check and the whole-buffer fallback.
                if let Some(decoder) = &zstd_decoder {
                    decoder.borrow_mut().feed(data);
                }
                buf.extend_from_slice(data);
                if buf.len() > MAX_COMPRESSED_SSE_BUFFER {
                    log::warn!(
                        "[HttpAggregator] compressed SSE buffer exceeded {MAX_COMPRESSED_SSE_BUFFER} bytes, finalizing best-effort | conn={connection_id:?}"
                    );
                    return Some(Self::finish_compressed_sse(
                        connection_id,
                        request,
                        response_headers,
                        content_encoding,
                        buf,
                        zstd_decoder,
                    ));
                }
                if crate::utils::decompress::chunked_stream_complete(&buf) {
                    Some(Self::finish_compressed_sse(
                        connection_id,
                        request,
                        response_headers,
                        content_encoding,
                        buf,
                        zstd_decoder,
                    ))
                } else {
                    self.insert(
                        connection_id,
                        ConnectionState::SseActive {
                            request,
                            response_headers,
                            sse_events,
                            compressed_buffer: Some(buf),
                            content_encoding,
                            zstd_decoder,
                        },
                    );
                    None
                }
            }
            other => {
                // Not in RequestBodyPending / compressed-SSE state. If we are
                // in an uncompressed SseActive stream targeting the OpenAI
                // Responses API, buffer the bytes as a continuation of the
                // last SSE event so we can recover token-usage from
                // oversized events (e.g. `response.completed`) that span
                // multiple TLS records. Other providers fit usage in a
                // single small event, so skip the extra copy.
                if let ConnectionState::SseActive {
                    request,
                    compressed_buffer: None,
                    ..
                } = &other
                {
                    if needs_sse_continuation_buffer(request.as_ref()) {
                        const MAX_CONTINUATION_BYTES: usize = 1 << 20; // 1 MiB cap
                        let data = &ssl_event.buf[..ssl_event.buf_size() as usize];
                        let buf = self
                            .sse_continuation_buffers
                            .get_or_insert_mut(connection_id, Vec::new);
                        let remaining = MAX_CONTINUATION_BYTES.saturating_sub(buf.len());
                        let take = data.len().min(remaining);
                        if take > 0 {
                            buf.extend_from_slice(&data[..take]);
                        }
                    }
                }
                self.insert(connection_id, other);
                None
            }
        }
    }

    /// Process SSE Event (from SSE Parser)
    /// Only valid when connection is in SseActive state
    pub fn process_sse_event(
        &mut self,
        connection_id: &ConnectionId,
        sse_event: ParsedSseEvent,
    ) -> Option<AggregatedResult> {
        let state = self.connections.pop(connection_id)?;

        match state {
            ConnectionState::SseActive {
                request,
                response_headers,
                mut sse_events,
                compressed_buffer,
                content_encoding,
                zstd_decoder,
            } => {
                // Compressed streams are decoded at completion, not live. A done
                // marker (e.g. a standalone "0\r\n\r\n" chunk terminator surfaced
                // as a DONE event) finalizes the buffered stream.
                if let Some(buf) = compressed_buffer {
                    if sse_event.is_done() {
                        return Some(Self::finish_compressed_sse(
                            *connection_id,
                            request,
                            response_headers,
                            content_encoding,
                            buf,
                            zstd_decoder,
                        ));
                    }
                    self.insert(
                        *connection_id,
                        ConnectionState::SseActive {
                            request,
                            response_headers,
                            sse_events,
                            compressed_buffer: Some(buf),
                            content_encoding,
                            zstd_decoder,
                        },
                    );
                    return None;
                }
                // Check if stream is done before processing
                let is_done = sse_event.is_done();

                log::trace!(
                    "[HttpAggregator] SSE event in SseActive | conn={connection_id:?} | is_done={is_done} | event={:?} | data_len={}",
                    sse_event.event,
                    sse_event.data_len(),
                );

                // Append the underlying SSL chunk bytes to the continuation
                // buffer so that oversized events (whose first chunk arrives
                // here with truncated data) can still be reconstructed by
                // downstream extractors. Only enable for the OpenAI
                // Responses API — other providers emit usage in single
                // small events. Dedup by source_event pointer so a single
                // SSL_read producing multiple SSE events contributes only
                // once — EXCEPT for the done event, which we always append
                // because its source chunk carries usage data that must not
                // be lost to dedup.
                if needs_sse_continuation_buffer(request.as_ref()) {
                    const MAX_CONTINUATION_BYTES: usize = 1 << 20; // 1 MiB cap
                    let src = sse_event.source_event();
                    let src_ptr = src as *const _ as usize;
                    let src_buf_len = src.buf_size() as usize;
                    let last_ptr = self.last_appended_src_ptr.get(connection_id).copied();
                    let should_append = is_done
                        || (last_ptr != Some(src_ptr)
                            && src_buf_len > 0
                            && src_buf_len <= src.buf.len());
                    if should_append && src_buf_len > 0 && src_buf_len <= src.buf.len() {
                        let buf = self
                            .sse_continuation_buffers
                            .get_or_insert_mut(*connection_id, Vec::new);
                        let remaining = MAX_CONTINUATION_BYTES.saturating_sub(buf.len());
                        let take = src_buf_len.min(remaining);
                        if take > 0 {
                            buf.extend_from_slice(&src.buf[..take]);
                        }
                        self.last_appended_src_ptr.put(*connection_id, src_ptr);
                    }
                }

                // Add SSE event to the list
                sse_events.push(sse_event);

                if is_done {
                    log::trace!(
                        "[HttpAggregator] State transition: SseActive -> Complete | conn={connection_id:?}",
                    );

                    // Build aggregated response with SSE events
                    let mut response = AggregatedResponse::from_parsed(response_headers);
                    response.set_sse_events(sse_events);
                    response.sse_continuation_bytes =
                        self.sse_continuation_buffers.pop(connection_id);
                    self.last_appended_src_ptr.pop(connection_id);

                    // Return appropriate result based on whether request exists
                    if let Some(req) = request {
                        let parsed = response.parsed.clone();
                        let mut pair = HttpPair::from_parsed(*connection_id, req, parsed);
                        pair.response = response;
                        Some(AggregatedResult::SseComplete(pair))
                    } else {
                        Some(AggregatedResult::ResponseOnly {
                            connection_id: *connection_id,
                            response,
                        })
                    }
                } else {
                    // Continue SSE active state (uncompressed: parsed live)
                    self.insert(
                        *connection_id,
                        ConnectionState::SseActive {
                            request,
                            response_headers,
                            sse_events,
                            compressed_buffer: None,
                            content_encoding,
                            zstd_decoder: None,
                        },
                    );

                    None
                }
            }
            _ => {
                log::trace!(
                    "[HttpAggregator] SSE event in unexpected state | conn={connection_id:?}"
                );
                // Not in SSE active state, restore state
                self.insert(*connection_id, state);
                None
            }
        }
    }

    /// Get active connection count
    pub fn active_connections(&self) -> usize {
        self.connections.len()
    }

    /// Return current connection-cache gauges and cumulative evictions.
    pub(crate) fn metrics(&self) -> ConnectionMetrics {
        let pending_connection_count = self
            .connections
            .iter()
            .filter(|(_, state)| !matches!(state, ConnectionState::Idle))
            .count();
        let pending_connection_bytes: usize = self
            .connections
            .iter()
            .filter_map(|(_, state)| {
                (!matches!(state, ConnectionState::Idle)).then_some(state.buffered_bytes())
            })
            .sum();
        let continuation_bytes = self
            .sse_continuation_buffers
            .iter()
            .map(|(_, bytes)| bytes.len())
            .sum::<usize>();
        ConnectionMetrics {
            connection_cache_bytes: pending_connection_bytes.saturating_add(continuation_bytes),
            pending_connection_count,
            pending_connection_bytes,
            eviction_count: self.eviction_count,
        }
    }

    /// Check if connection has pending request
    pub fn has_pending_request(&self, connection_id: &ConnectionId) -> bool {
        matches!(
            self.connections.peek(connection_id),
            Some(ConnectionState::RequestPending { .. })
        )
    }

    /// Check if connection is SSE active
    pub fn is_sse_active(&self, connection_id: &ConnectionId) -> bool {
        matches!(
            self.connections.peek(connection_id),
            Some(ConnectionState::SseActive { .. })
        )
    }

    /// Check if there are any pending connections
    pub fn has_pending(&self) -> bool {
        !self.connections.is_empty()
    }

    /// Clear all connections
    pub fn clear(&mut self) {
        self.connections.clear();
        self.sse_continuation_buffers.clear();
        self.last_appended_src_ptr.clear();
        self.last_activity.clear();
        self.idle_snapshotted.clear();
    }

    /// Drain all connections (for force complete)
    ///
    /// No in-crate caller today; kept (dead_code-allowed) through the review
    /// F3 visibility narrowing for force-complete tooling.
    #[allow(dead_code)]
    pub(crate) fn drain_connections(&mut self) -> Vec<(ConnectionId, ConnectionState)> {
        self.connections
            .iter_mut()
            .map(|(k, v)| (*k, v.clone()))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(k, _)| {
                self.last_activity.pop(&k);
                self.idle_snapshotted.pop(&k);
                (k, self.connections.pop(&k).unwrap())
            })
            .collect()
    }

    /// Drain all connections belonging to a specific PID.
    ///
    /// Returns `(ConnectionId, ConnectionState)` for entries that were in
    /// `RequestPending` or `SseActive` state.  `Idle` entries are silently
    /// discarded.  Used by crash detection on `ProcMon::Exit`.
    pub(crate) fn drain_connections_for_pid(
        &mut self,
        pid: u32,
    ) -> Vec<(ConnectionId, ConnectionState)> {
        let keys: Vec<ConnectionId> = self
            .connections
            .iter()
            .filter(|(k, _)| k.pid == pid)
            .map(|(k, _)| *k)
            .collect();

        if keys.is_empty() {
            return vec![];
        }

        let mut result = Vec::new();
        for key in keys {
            if let Some(state) = self.connections.pop(&key) {
                self.sse_continuation_buffers.pop(&key);
                self.last_appended_src_ptr.pop(&key);
                self.last_activity.pop(&key);
                self.idle_snapshotted.pop(&key);
                match state {
                    ConnectionState::Idle => {}
                    _ => {
                        log::debug!(
                            "[HttpAggregator] Draining connection for exited PID: pid={} ssl_ptr={:#x}",
                            key.pid,
                            key.ssl_ptr,
                        );
                        result.push((key, state));
                    }
                }
            }
        }

        if !result.is_empty() {
            log::info!(
                "[HttpAggregator] Drained {} connection(s) for exited pid={}",
                result.len(),
                pid,
            );
        }

        result
    }

    /// Drain connections whose PID is no longer alive.
    ///
    /// Checks `/proc/{pid}` for each unique PID in the connection pool.
    /// Returns `(ConnectionId, ConnectionState)` for dead-PID entries that
    /// were in `RequestPending` or `SseActive` state.  `Idle` entries are
    /// silently discarded.  This allows the caller to persist orphaned
    /// in-flight requests before they are lost.
    pub(crate) fn drain_dead_pid_connections(&mut self) -> Vec<(ConnectionId, ConnectionState)> {
        use std::collections::HashSet;

        // 1. Collect unique PIDs
        let pids: HashSet<u32> = self.connections.iter().map(|(k, _)| k.pid).collect();

        // 2. Determine which PIDs are dead
        let dead_pids: HashSet<u32> = pids
            .into_iter()
            .filter(|pid| !crate::utils::procfs::proc_pid(pid).exists())
            .collect();

        if dead_pids.is_empty() {
            return vec![];
        }

        // 3. Collect keys for dead PIDs (can't mutate while iterating)
        let dead_keys: Vec<ConnectionId> = self
            .connections
            .iter()
            .filter(|(k, _)| dead_pids.contains(&k.pid))
            .map(|(k, _)| *k)
            .collect();

        // 4. Pop dead entries and return non-Idle ones
        let mut result = Vec::new();
        for key in dead_keys {
            if let Some(state) = self.connections.pop(&key) {
                self.sse_continuation_buffers.pop(&key);
                self.last_appended_src_ptr.pop(&key);
                self.last_activity.pop(&key);
                self.idle_snapshotted.pop(&key);
                match state {
                    ConnectionState::Idle => {
                        // Silently discard idle entries
                    }
                    _ => {
                        log::debug!(
                            "[HttpAggregator] Draining dead-PID connection: pid={} ssl_ptr={:#x}",
                            key.pid,
                            key.ssl_ptr,
                        );
                        result.push((key, state));
                    }
                }
            }
        }

        if !result.is_empty() {
            log::info!(
                "[HttpAggregator] Drained {} connection(s) for dead PIDs: {:?}",
                result.len(),
                dead_pids,
            );
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::rc::Rc;

    fn create_mock_ssl_event(pid: u32, ssl_ptr: u64) -> Rc<SslEvent> {
        Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1000,
            delta_ns: 0,
            pid,
            tid: 1,
            uid: 0,
            len: 0,
            rw: 0,
            comm: String::new(),
            buf: Vec::new(),
            is_handshake: false,
            ssl_ptr,
        })
    }

    #[test]
    fn test_connection_id() {
        let id = ConnectionId {
            pid: 1234,
            ssl_ptr: 0x1000,
        };
        assert_eq!(id.pid, 1234);
        assert_eq!(id.ssl_ptr, 0x1000);
    }

    #[test]
    fn test_process_request_response_pair() {
        let mut aggregator = HttpConnectionAggregator::new();
        let event = create_mock_ssl_event(1234, 0x1000);

        // Process request
        let request = ParsedRequest {
            method: "GET".to_string(),
            path: "/api/test".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: event.clone(),
            reassembled_body: None,
        };
        aggregator.process_request(request);

        assert!(aggregator.has_pending_request(&ConnectionId {
            pid: 1234,
            ssl_ptr: 0x1000
        }));

        // Process response
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers: HashMap::from([("content-length".into(), "0".into())]),
            body_offset: 0,
            body_len: 0,
            source_event: event,
        };

        let result = aggregator.process_response(response);
        assert!(result.is_some());

        if let Some(AggregatedResult::HttpComplete(pair)) = result {
            assert_eq!(pair.request.method, "GET");
            assert_eq!(pair.response.status_code(), 200);
            assert!(pair.response.sse_events.is_empty());
        } else {
            panic!("Expected HttpComplete result");
        }
    }

    #[test]
    fn test_sse_detection() {
        let mut aggregator = HttpConnectionAggregator::new();
        let event = create_mock_ssl_event(1234, 0x1000);

        // Process request
        let request = ParsedRequest {
            method: "GET".to_string(),
            path: "/stream".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: event.clone(),
            reassembled_body: None,
        };
        aggregator.process_request(request);

        // Process SSE response
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());

        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: 0,
            source_event: event,
        };

        let result = aggregator.process_response(response);

        // SSE response should not return result immediately, but should activate SSE state
        assert!(result.is_none());
        assert!(aggregator.is_sse_active(&ConnectionId {
            pid: 1234,
            ssl_ptr: 0x1000
        }));
    }

    fn create_mock_ssl_event_with_buf(
        pid: u32,
        ssl_ptr: u64,
        buf: Vec<u8>,
        rw: i32,
    ) -> Rc<SslEvent> {
        Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1000,
            delta_ns: 0,
            pid,
            tid: 1,
            uid: 0,
            len: buf.len() as u32,
            rw,
            comm: String::new(),
            buf,
            is_handshake: false,
            ssl_ptr,
        })
    }

    #[test]
    fn test_request_body_aggregation_content_length() {
        let mut aggregator = HttpConnectionAggregator::new();

        // Simulate a request with Content-Length: 20 but only 5 bytes in first event
        let headers_and_partial_body = b"POST /api HTTP/1.1\r\nContent-Length: 20\r\n\r\nhello";
        let event1 =
            create_mock_ssl_event_with_buf(1234, 0x2000, headers_and_partial_body.to_vec(), 1);

        // Parse as request (simulating what HttpParser would produce)
        let header_end = headers_and_partial_body
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap()
            + 4;
        let body_len = headers_and_partial_body.len() - header_end;

        let mut headers = HashMap::new();
        headers.insert("content-length".to_string(), "20".to_string());

        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/api".to_string(),
            version: 1,
            headers,
            body_offset: header_end,
            body_len,
            source_event: event1,
            reassembled_body: None,
        };

        // Process request - should enter RequestBodyPending since body_len(5) < content_length(20)
        aggregator.process_request(request);
        let conn_id = ConnectionId {
            pid: 1234,
            ssl_ptr: 0x2000,
        };
        assert!(!aggregator.has_pending_request(&conn_id));

        // Send continuation data (10 bytes)
        let continuation1 = SslEvent {
            source: 0,
            timestamp_ns: 2000,
            delta_ns: 0,
            pid: 1234,
            tid: 1,
            uid: 0,
            len: 10,
            rw: 1,
            comm: String::new(),
            buf: b" world fir".to_vec(),
            is_handshake: false,
            ssl_ptr: 0x2000,
        };
        let result = aggregator.process_raw_body_data(&continuation1);
        assert!(result.is_none()); // Still incomplete (15 < 20)

        // Send final continuation (5 bytes, total = 5 + 10 + 5 = 20)
        let continuation2 = SslEvent {
            source: 0,
            timestamp_ns: 3000,
            delta_ns: 0,
            pid: 1234,
            tid: 1,
            uid: 0,
            len: 5,
            rw: 1,
            comm: String::new(),
            buf: b"st!!!".to_vec(),
            is_handshake: false,
            ssl_ptr: 0x2000,
        };
        let result = aggregator.process_raw_body_data(&continuation2);
        assert!(result.is_none()); // Transitioned to RequestPending

        // Now the request should be in RequestPending with full body
        assert!(aggregator.has_pending_request(&conn_id));

        // Sending a response should complete the pair
        let resp_event = create_mock_ssl_event_with_buf(
            1234,
            0x2000,
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK".to_vec(),
            0,
        );
        let response = ParsedResponse {
            version: 1,
            status_code: 200,
            reason: "OK".to_string(),
            headers: HashMap::from([("content-length".into(), "2".into())]),
            body_offset: 38,
            body_len: 2,
            source_event: resp_event,
        };

        let result = aggregator.process_response(response);
        assert!(result.is_some());
        if let Some(AggregatedResult::HttpComplete(pair)) = result {
            assert_eq!(pair.request.method, "POST");
            // Verify the reassembled body
            let body = pair.request.body();
            assert_eq!(body, b"hello world first!!!");
            assert_eq!(body.len(), 20);
        } else {
            panic!("Expected HttpComplete result");
        }
    }

    #[test]
    fn test_request_body_aggregation_response_completion() {
        let mut aggregator = HttpConnectionAggregator::new();

        // Request with Content-Length but body will be completed by response arrival
        let headers_and_partial = b"POST /chat HTTP/1.1\r\nContent-Length: 100\r\n\r\npartial";
        let event = create_mock_ssl_event_with_buf(5678, 0x3000, headers_and_partial.to_vec(), 1);

        let header_end = headers_and_partial
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap()
            + 4;
        let body_len = headers_and_partial.len() - header_end;

        let mut headers = HashMap::new();
        headers.insert("content-length".to_string(), "100".to_string());

        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/chat".to_string(),
            version: 1,
            headers,
            body_offset: header_end,
            body_len,
            source_event: event,
            reassembled_body: None,
        };

        aggregator.process_request(request);

        // Send some continuation
        let cont = SslEvent {
            source: 0,
            timestamp_ns: 2000,
            delta_ns: 0,
            pid: 5678,
            tid: 1,
            uid: 0,
            len: 10,
            rw: 1,
            comm: String::new(),
            buf: b"_more_data".to_vec(),
            is_handshake: false,
            ssl_ptr: 0x3000,
        };
        aggregator.process_raw_body_data(&cont);

        // Response arrives before Content-Length is satisfied → force-complete
        let resp_event = create_mock_ssl_event_with_buf(
            5678,
            0x3000,
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
            0,
        );
        let response = ParsedResponse {
            version: 1,
            status_code: 200,
            reason: "OK".to_string(),
            headers: HashMap::from([("content-length".into(), "2".into())]),
            body_offset: 38,
            body_len: 2,
            source_event: resp_event,
        };

        let result = aggregator.process_response(response);
        assert!(result.is_some());
        if let Some(AggregatedResult::HttpComplete(pair)) = result {
            // Body should be truncated to content_length (100) but we only have 17 bytes
            // Since total buffer (17) < content_length (100), truncate does nothing
            let body = pair.request.body();
            assert_eq!(body, b"partial_more_data");
        } else {
            panic!("Expected HttpComplete result");
        }
    }

    #[test]
    fn test_request_body_single_event_no_aggregation() {
        let mut aggregator = HttpConnectionAggregator::new();

        // Request where body fits in single event (body_len >= content_length)
        let full_request = b"POST /api HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello";
        let event = create_mock_ssl_event_with_buf(1234, 0x4000, full_request.to_vec(), 1);

        let header_end = full_request
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap()
            + 4;
        let body_len = full_request.len() - header_end;

        let mut headers = HashMap::new();
        headers.insert("content-length".to_string(), "5".to_string());

        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/api".to_string(),
            version: 1,
            headers,
            body_offset: header_end,
            body_len,
            source_event: event,
            reassembled_body: None,
        };

        // Should go directly to RequestPending (no aggregation needed)
        aggregator.process_request(request);
        let conn_id = ConnectionId {
            pid: 1234,
            ssl_ptr: 0x4000,
        };
        assert!(aggregator.has_pending_request(&conn_id));
    }

    #[test]
    fn test_raw_data_ignored_when_no_pending() {
        let mut aggregator = HttpConnectionAggregator::new();

        // Send raw data for a connection that has no pending body
        let raw = SslEvent {
            source: 0,
            timestamp_ns: 1000,
            delta_ns: 0,
            pid: 9999,
            tid: 1,
            uid: 0,
            len: 5,
            rw: 1,
            comm: String::new(),
            buf: b"hello".to_vec(),
            is_handshake: false,
            ssl_ptr: 0x5000,
        };
        let result = aggregator.process_raw_body_data(&raw);
        assert!(result.is_none());
        assert_eq!(aggregator.active_connections(), 0);
    }

    #[test]
    fn test_request_body_pending_with_sse_response() {
        let mut aggregator = HttpConnectionAggregator::new();

        // Request with incomplete body
        let partial = b"POST /stream HTTP/1.1\r\nContent-Length: 50\r\n\r\ndata";
        let event = create_mock_ssl_event_with_buf(1234, 0x6000, partial.to_vec(), 1);

        let header_end = partial.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let body_len = partial.len() - header_end;

        let mut headers = HashMap::new();
        headers.insert("content-length".to_string(), "50".to_string());

        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/stream".to_string(),
            version: 1,
            headers,
            body_offset: header_end,
            body_len,
            source_event: event,
            reassembled_body: None,
        };

        aggregator.process_request(request);

        // SSE response arrives → should force-complete body and enter SseActive
        let resp_event = create_mock_ssl_event_with_buf(
            1234,
            0x6000,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n".to_vec(),
            0,
        );
        let mut resp_headers = HashMap::new();
        resp_headers.insert("content-type".to_string(), "text/event-stream".to_string());

        let response = ParsedResponse {
            version: 1,
            status_code: 200,
            reason: "OK".to_string(),
            headers: resp_headers,
            body_offset: 0,
            body_len: 0,
            source_event: resp_event,
        };

        let result = aggregator.process_response(response);
        // SSE response should not return immediately, should enter SseActive
        assert!(result.is_none());
        let conn_id = ConnectionId {
            pid: 1234,
            ssl_ptr: 0x6000,
        };
        assert!(aggregator.is_sse_active(&conn_id));
    }

    #[test]
    fn test_sse_first_chunk_in_initial_response_body_is_preserved() {
        let mut aggregator = HttpConnectionAggregator::new();

        let req_event = create_mock_ssl_event_with_buf(
            4321,
            0x7000,
            b"POST /stream HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
            1,
        );
        let mut req_headers = HashMap::new();
        req_headers.insert("content-length".to_string(), "2".to_string());
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/stream".to_string(),
            version: 1,
            headers: req_headers,
            body_offset: 43,
            body_len: 2,
            source_event: req_event,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        let resp_buf = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"3\"}}]}\n\n".to_vec();
        let resp_event = create_mock_ssl_event_with_buf(4321, 0x7000, resp_buf.clone(), 0);
        let response = ParsedResponse {
            version: 1,
            status_code: 200,
            reason: "OK".to_string(),
            headers: {
                let mut h = HashMap::new();
                h.insert("content-type".to_string(), "text/event-stream".to_string());
                h
            },
            body_offset: resp_buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4,
            body_len: resp_buf.len()
                - (resp_buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4),
            source_event: resp_event,
        };

        let result = aggregator.process_response(response);
        assert!(result.is_none());

        let done_event =
            create_mock_ssl_event_with_buf(4321, 0x7000, b"data: [DONE]\n\n".to_vec(), 0);
        let done = ParsedSseEvent::new(None, None, None, 6, 6, done_event);
        let conn_id = ConnectionId {
            pid: 4321,
            ssl_ptr: 0x7000,
        };
        let result = aggregator.process_sse_event(&conn_id, done);
        let pair = match result {
            Some(AggregatedResult::SseComplete(pair)) => pair,
            other => panic!("expected SseComplete, got {other:?}"),
        };

        assert_eq!(pair.response.sse_event_count(), 2);
        let chunks = pair.response.json_body();
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            chunks[0]["choices"][0]["delta"]["content"].as_str(),
            Some("3")
        );
        assert!(pair.response.parsed.body_str().is_empty());
    }

    // ── Compressed SSE tests ────────────────────────────────────────────

    fn make_zstd_chunked_sse() -> (Vec<u8>, Vec<u8>) {
        let sse_plain =
            b"event: message_start\ndata: {\"type\":\"message_start\"}\n\ndata: [DONE]\n\n";
        let compressed = zstd::encode_all(&sse_plain[..], 3).unwrap();
        let mut chunked = Vec::new();
        chunked.extend_from_slice(format!("{:x}\r\n", compressed.len()).as_bytes());
        chunked.extend_from_slice(&compressed);
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");
        (sse_plain.to_vec(), chunked)
    }

    #[test]
    fn test_sse_entry_state_detects_zstd_header() {
        let (_, chunked) = make_zstd_chunked_sse();
        let event = create_mock_ssl_event_with_buf(1, 0x100, chunked.clone(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        headers.insert("content-encoding".to_string(), "zstd".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: chunked.len(),
            source_event: event,
        };
        let (sse_events, buf, enc, _decoder) = HttpConnectionAggregator::sse_entry_state(&response);
        assert!(buf.is_some(), "compressed buffer should be Some for zstd");
        assert!(sse_events.is_empty());
        assert_eq!(enc.as_deref(), Some("zstd"));
    }

    #[test]
    fn test_sse_entry_state_detects_zstd_magic() {
        let plain = b"data: hi\n\n";
        let compressed = zstd::encode_all(&plain[..], 3).unwrap();
        let event = create_mock_ssl_event_with_buf(1, 0x101, compressed.clone(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: compressed.len(),
            source_event: event,
        };
        let (_, buf, _, _) = HttpConnectionAggregator::sse_entry_state(&response);
        assert!(buf.is_some(), "should detect zstd via magic bytes");
    }

    #[test]
    fn test_sse_entry_state_gzip_header_never_builds_zstd_decoder() {
        // Review F2: an explicit non-zstd Content-Encoding must win over the
        // magic sniff even when the payload bytes collide with the zstd magic.
        // Behavior must stay identical to origin/main: buffer-until-complete,
        // no incremental decoder.
        let mut body = vec![0x28, 0xb5, 0x2f, 0xfd]; // zstd magic collision
        body.extend_from_slice(b"rest-of-gzip-payload");
        let event = create_mock_ssl_event_with_buf(1, 0x102, body.clone(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        headers.insert("content-encoding".to_string(), "gzip".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: body.len(),
            source_event: event,
        };
        let (sse_events, buf, enc, decoder) = HttpConnectionAggregator::sse_entry_state(&response);
        assert!(buf.is_some(), "gzip stream must still buffer as compressed");
        assert!(sse_events.is_empty());
        assert_eq!(enc.as_deref(), Some("gzip"));
        assert!(
            decoder.is_none(),
            "gzip header must never enable the zstd incremental decoder"
        );
    }

    #[test]
    fn test_sse_entry_state_uncompressed() {
        let body = b"data: hello\n\n";
        let event = create_mock_ssl_event_with_buf(1, 0x102, body.to_vec(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: body.len(),
            source_event: event,
        };
        let (_, buf, _, _) = HttpConnectionAggregator::sse_entry_state(&response);
        assert!(buf.is_none(), "uncompressed SSE should have no buffer");
    }

    #[test]
    fn test_compressed_sse_request_pending_immediate_finish() {
        let mut aggregator = HttpConnectionAggregator::new();
        let (_, chunked) = make_zstd_chunked_sse();

        let req_event = create_mock_ssl_event(1, 0x200);
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: req_event,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        let resp_event = create_mock_ssl_event_with_buf(1, 0x200, chunked.clone(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        headers.insert("content-encoding".to_string(), "zstd".to_string());
        headers.insert("transfer-encoding".to_string(), "chunked".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: chunked.len(),
            source_event: resp_event,
        };

        let result = aggregator.process_response(response);
        match result {
            Some(AggregatedResult::SseComplete(pair)) => {
                assert!(!pair.response.sse_events.is_empty());
            }
            other => panic!("expected SseComplete, got {other:?}"),
        }
    }

    /// Put `aggregator` into a compressed (zstd, chunked) `SseActive` state with
    /// an empty buffer, ready to receive body fragments via
    /// `process_raw_body_data`.
    fn enter_compressed_sse_active(
        aggregator: &mut HttpConnectionAggregator,
        pid: u32,
        ssl_ptr: u64,
    ) {
        let req_event = create_mock_ssl_event(pid, ssl_ptr);
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: req_event,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        let resp_event = create_mock_ssl_event_with_buf(pid, ssl_ptr, Vec::new(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        headers.insert("content-encoding".to_string(), "zstd".to_string());
        headers.insert("transfer-encoding".to_string(), "chunked".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: 0,
            source_event: resp_event,
        };
        aggregator.process_response(response);
    }

    fn raw_frag(pid: u32, ssl_ptr: u64, buf: Vec<u8>) -> SslEvent {
        SslEvent {
            source: 0,
            timestamp_ns: 2000,
            delta_ns: 0,
            pid,
            tid: 1,
            uid: 0,
            len: buf.len() as u32,
            rw: 0,
            comm: String::new(),
            buf,
            is_handshake: false,
            ssl_ptr,
        }
    }

    #[test]
    fn compressed_completion_ignores_embedded_terminator() {
        // fix(#973): completion must walk chunk framing, not scan the raw bytes
        // for b"0\r\n\r\n" — that pattern can occur *inside* a compressed payload
        // and finish the stream early, truncating it so the call is silently
        // dropped. Reverting to `windows(5).any` makes this return Some and fail.
        let mut aggregator = HttpConnectionAggregator::new();
        enter_compressed_sse_active(&mut aggregator, 9, 0x900);

        let data = b"AB0\r\n\r\nCD"; // chunk data that itself contains the terminator
        let mut raw = Vec::new();
        raw.extend_from_slice(format!("{:x}\r\n", data.len()).as_bytes());
        raw.extend_from_slice(data);
        raw.extend_from_slice(b"\r\n"); // chunk-data CRLF, but NO zero-size chunk yet
        assert!(
            raw.windows(5).any(|w| w == b"0\r\n\r\n"),
            "precondition: the old naive scan WOULD falsely finish here"
        );

        let result = aggregator.process_raw_body_data(&raw_frag(9, 0x900, raw));
        assert!(
            result.is_none(),
            "embedded terminator must not finish the stream prematurely"
        );
    }

    #[test]
    fn compressed_buffer_cap_finalizes_when_exceeded() {
        // fix(#973): a never-terminating compressed stream must not grow the
        // buffer unboundedly; past MAX_COMPRESSED_SSE_BUFFER it finalizes
        // best-effort. Without the cap this keeps buffering (returns None).
        let mut aggregator = HttpConnectionAggregator::new();
        enter_compressed_sse_active(&mut aggregator, 10, 0xA00);

        let over = vec![b'x'; MAX_COMPRESSED_SSE_BUFFER + 1024]; // over cap, no terminator
        let result = aggregator.process_raw_body_data(&raw_frag(10, 0xA00, over));
        assert!(
            result.is_some(),
            "over-cap compressed buffer must finalize early, not keep buffering"
        );
    }

    #[test]
    fn decode_compressed_sse_decodes_chunked_zstd() {
        // The shared decode reused by both the live finalizer and the drain path.
        let (_plain, chunked) = make_zstd_chunked_sse();
        let src = create_mock_ssl_event(11, 0xB00);
        let events = HttpConnectionAggregator::decode_compressed_sse(
            &chunked,
            Some("zstd"),
            true,
            None,
            &src,
        );
        assert!(
            !events.is_empty(),
            "must decode chunked zstd SSE into parsed events"
        );
    }

    #[test]
    fn test_compressed_sse_no_prior_state_returns_none() {
        let mut aggregator = HttpConnectionAggregator::new();
        let (_, chunked) = make_zstd_chunked_sse();

        // Response for a connection not in the cache → pop returns None → result is None
        let resp_event = create_mock_ssl_event_with_buf(1, 0x201, chunked.clone(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        headers.insert("content-encoding".to_string(), "zstd".to_string());
        headers.insert("transfer-encoding".to_string(), "chunked".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: chunked.len(),
            source_event: resp_event,
        };

        let result = aggregator.process_response(response);
        assert!(result.is_none(), "no prior state → None");
    }

    #[test]
    fn test_compressed_sse_process_sse_event_done_triggers_finish() {
        let mut aggregator = HttpConnectionAggregator::new();
        let sse_plain = b"data: via-sse-event\n\ndata: [DONE]\n\n";
        let compressed = zstd::encode_all(&sse_plain[..], 3).unwrap();
        let mut chunked = Vec::new();
        chunked.extend_from_slice(format!("{:x}\r\n", compressed.len()).as_bytes());
        chunked.extend_from_slice(&compressed);
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");

        let req_event = create_mock_ssl_event(6, 0x700);
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: req_event,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        // Empty-body SSE response → SseActive with compressed buffer
        let resp_event = create_mock_ssl_event_with_buf(6, 0x700, Vec::new(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        headers.insert("content-encoding".to_string(), "zstd".to_string());
        headers.insert("transfer-encoding".to_string(), "chunked".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: 0,
            source_event: resp_event,
        };
        aggregator.process_response(response);

        // Buffer the compressed data via process_raw_body_data (without terminator to stay buffering)
        let partial = SslEvent {
            source: 0,
            timestamp_ns: 2000,
            delta_ns: 0,
            pid: 6,
            tid: 1,
            uid: 0,
            len: chunked.len() as u32,
            rw: 0,
            comm: String::new(),
            buf: chunked.clone(),
            is_handshake: false,
            ssl_ptr: 0x700,
        };
        // This will finish via raw terminator detection
        let result = aggregator.process_raw_body_data(&partial);
        assert!(result.is_some());
    }

    #[test]
    fn test_compressed_sse_non_done_event_keeps_buffering() {
        let mut aggregator = HttpConnectionAggregator::new();

        let req_event = create_mock_ssl_event(7, 0x800);
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: req_event,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        let resp_event = create_mock_ssl_event_with_buf(7, 0x800, Vec::new(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        headers.insert("content-encoding".to_string(), "zstd".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: 0,
            source_event: resp_event,
        };
        aggregator.process_response(response);

        // Send a non-DONE SSE event while in compressed mode → should keep buffering
        let conn_id = ConnectionId {
            pid: 7,
            ssl_ptr: 0x800,
        };
        let evt = create_mock_ssl_event_with_buf(7, 0x800, b"data: partial\n\n".to_vec(), 0);
        let non_done = ParsedSseEvent::new(None, None, None, 0, 0, evt);
        let result = aggregator.process_sse_event(&conn_id, non_done);
        assert!(result.is_none(), "non-DONE event should keep buffering");
        assert!(aggregator.is_sse_active(&conn_id));
    }

    #[test]
    fn test_compressed_sse_buffering_then_finish() {
        let mut aggregator = HttpConnectionAggregator::new();
        let sse_plain = b"data: buffered\n\ndata: [DONE]\n\n";
        let compressed = zstd::encode_all(&sse_plain[..], 3).unwrap();
        let mut chunked = Vec::new();
        chunked.extend_from_slice(format!("{:x}\r\n", compressed.len()).as_bytes());
        chunked.extend_from_slice(&compressed);
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");

        let req_event = create_mock_ssl_event(2, 0x300);
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: req_event,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        // Response with empty body → enters SseActive with compressed_buffer
        let resp_event = create_mock_ssl_event_with_buf(2, 0x300, Vec::new(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        headers.insert("content-encoding".to_string(), "zstd".to_string());
        headers.insert("transfer-encoding".to_string(), "chunked".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: 0,
            source_event: resp_event,
        };
        let result = aggregator.process_response(response);
        assert!(result.is_none());
        let conn_id = ConnectionId {
            pid: 2,
            ssl_ptr: 0x300,
        };
        assert!(aggregator.is_sse_active(&conn_id));

        // Send first chunk of compressed data (partial, no terminator)
        let mid = chunked.len() / 2;
        let partial = SslEvent {
            source: 0,
            timestamp_ns: 2000,
            delta_ns: 0,
            pid: 2,
            tid: 1,
            uid: 0,
            len: mid as u32,
            rw: 0,
            comm: String::new(),
            buf: chunked[..mid].to_vec(),
            is_handshake: false,
            ssl_ptr: 0x300,
        };
        let result = aggregator.process_raw_body_data(&partial);
        assert!(result.is_none(), "should still be buffering");

        // Send remainder including "0\r\n\r\n" terminator
        let rest = SslEvent {
            source: 0,
            timestamp_ns: 3000,
            delta_ns: 0,
            pid: 2,
            tid: 1,
            uid: 0,
            len: (chunked.len() - mid) as u32,
            rw: 0,
            comm: String::new(),
            buf: chunked[mid..].to_vec(),
            is_handshake: false,
            ssl_ptr: 0x300,
        };
        let result = aggregator.process_raw_body_data(&rest);
        match result {
            Some(AggregatedResult::SseComplete(pair)) => {
                assert!(pair.response.sse_event_count() >= 2);
            }
            other => panic!("expected SseComplete after terminator, got {other:?}"),
        }
    }

    #[test]
    fn test_compressed_sse_done_event_triggers_finish() {
        let mut aggregator = HttpConnectionAggregator::new();
        let sse_plain = b"data: done-event\n\ndata: [DONE]\n\n";
        let compressed = zstd::encode_all(&sse_plain[..], 3).unwrap();
        let mut chunked = Vec::new();
        chunked.extend_from_slice(format!("{:x}\r\n", compressed.len()).as_bytes());
        chunked.extend_from_slice(&compressed);
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");

        let req_event = create_mock_ssl_event(3, 0x400);
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: req_event,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        // Empty-body response → SseActive with compressed buffer
        let resp_event = create_mock_ssl_event_with_buf(3, 0x400, Vec::new(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        headers.insert("content-encoding".to_string(), "zstd".to_string());
        headers.insert("transfer-encoding".to_string(), "chunked".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: 0,
            source_event: resp_event,
        };
        aggregator.process_response(response);

        // Buffer the full chunked data via raw
        let raw = SslEvent {
            source: 0,
            timestamp_ns: 2000,
            delta_ns: 0,
            pid: 3,
            tid: 1,
            uid: 0,
            len: chunked.len() as u32,
            rw: 0,
            comm: String::new(),
            buf: chunked.clone(),
            is_handshake: false,
            ssl_ptr: 0x400,
        };
        let result = aggregator.process_raw_body_data(&raw);
        match result {
            Some(AggregatedResult::SseComplete(_)) => {}
            other => panic!("expected SseComplete via raw terminator, got {other:?}"),
        }
    }

    #[test]
    fn test_compressed_sse_body_pending_immediate_finish() {
        let mut aggregator = HttpConnectionAggregator::new();
        let (_, chunked) = make_zstd_chunked_sse();

        // Request with incomplete body
        let partial = b"POST /stream HTTP/1.1\r\nContent-Length: 50\r\n\r\ndata";
        let event = create_mock_ssl_event_with_buf(4, 0x500, partial.to_vec(), 1);
        let header_end = partial.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let body_len = partial.len() - header_end;
        let mut req_headers = HashMap::new();
        req_headers.insert("content-length".to_string(), "50".to_string());
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/stream".to_string(),
            version: 1,
            headers: req_headers,
            body_offset: header_end,
            body_len,
            source_event: event,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        // SSE compressed response arrives, body pending → force-complete + immediate finish
        let resp_event = create_mock_ssl_event_with_buf(4, 0x500, chunked.clone(), 0);
        let mut resp_headers = HashMap::new();
        resp_headers.insert("content-type".to_string(), "text/event-stream".to_string());
        resp_headers.insert("content-encoding".to_string(), "zstd".to_string());
        resp_headers.insert("transfer-encoding".to_string(), "chunked".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers: resp_headers,
            body_offset: 0,
            body_len: chunked.len(),
            source_event: resp_event,
        };

        let result = aggregator.process_response(response);
        match result {
            Some(AggregatedResult::SseComplete(pair)) => {
                assert_eq!(pair.request.method, "POST");
                assert!(!pair.response.sse_events.is_empty());
            }
            other => panic!("expected SseComplete from body-pending, got {other:?}"),
        }
    }

    #[test]
    fn test_finish_compressed_sse_non_chunked() {
        let sse_plain = b"data: no-chunks\n\ndata: [DONE]\n\n";
        let compressed = zstd::encode_all(&sse_plain[..], 3).unwrap();
        let event = create_mock_ssl_event(5, 0x600);
        let response_headers = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: event,
        };
        let conn_id = ConnectionId {
            pid: 5,
            ssl_ptr: 0x600,
        };
        let result = HttpConnectionAggregator::finish_compressed_sse(
            conn_id,
            None,
            response_headers,
            Some("zstd".to_string()),
            compressed,
            None,
        );
        match result {
            AggregatedResult::ResponseOnly { response, .. } => {
                assert!(response.sse_event_count() >= 2);
            }
            other => panic!("expected ResponseOnly, got {other:?}"),
        }
    }

    // ── Fragmented zstd streaming tests (issue #2267) ────────────────────

    /// Build a realistic Anthropic SSE stream compressed the way a streaming
    /// gateway does: ONE zstd frame, flushed per event so the client can
    /// decode incrementally, and never finished while the response is open.
    /// Returns `(plain, compressed)` where `compressed` has no frame epilogue.
    fn make_streaming_zstd_sse() -> (Vec<u8>, Vec<u8>) {
        use std::io::Write;
        let events: [&[u8]; 3] = [
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01ABC\",\"model\":\"claude-sonnet-4\",\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n\n",
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}\n\n",
        ];
        let mut plain = Vec::new();
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        for ev in events {
            plain.extend_from_slice(ev);
            encoder.write_all(ev).unwrap();
            // Per-event flush ends the current block and emits it, exactly
            // like a live gateway; the frame itself stays open.
            encoder.flush().unwrap();
        }
        let compressed = encoder.get_ref().clone();
        (plain, compressed)
    }

    #[test]
    fn fragmented_zstd_stream_without_terminator_decodes_incrementally() {
        // issue #2267: a gateway that zstd-compresses a streamed SSE response
        // delivers one long open frame in many fragments (HTTP/2 style — no
        // chunked terminator, no frame epilogue mid-stream). Whole-buffer
        // decompression fails with UnexpectedEof "incomplete frame" and the
        // raw compressed bytes silently reach the SSE parser: events=0, all
        // usage lost. Incremental decoding must recover every flushed event.
        let mut aggregator = HttpConnectionAggregator::new();
        let (plain, compressed) = make_streaming_zstd_sse();

        let req_event = create_mock_ssl_event(40, 0xF000);
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            version: 2,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: req_event,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        // HTTP/2-style response: zstd content-encoding, NO transfer-encoding.
        let resp_event = create_mock_ssl_event_with_buf(40, 0xF000, Vec::new(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        headers.insert("content-encoding".to_string(), "zstd".to_string());
        let response = ParsedResponse {
            version: 2,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: 0,
            source_event: resp_event,
        };
        assert!(aggregator.process_response(response).is_none());

        // Feed the compressed frame in small fragments, as TLS records arrive.
        for frag in compressed.chunks(11) {
            let result = aggregator.process_raw_body_data(&raw_frag(40, 0xF000, frag.to_vec()));
            assert!(result.is_none(), "fragments must keep buffering");
        }

        // Stream-end signal: a chunked terminator never arrives on this path.
        let conn_id = ConnectionId {
            pid: 40,
            ssl_ptr: 0xF000,
        };
        let done_event =
            create_mock_ssl_event_with_buf(40, 0xF000, b"data: [DONE]\n\n".to_vec(), 0);
        let done = ParsedSseEvent::new_done_marker(done_event);
        let pair = match aggregator.process_sse_event(&conn_id, done) {
            Some(AggregatedResult::SseComplete(pair)) => pair,
            other => panic!("expected SseComplete, got {other:?}"),
        };

        // Full-fidelity check: the decoded stream must parse into exactly the
        // same events as the original plaintext would (not merely "some
        // events"), so a decoder that silently drops a lagging block or
        // corrupts a boundary cannot pass.
        let expected =
            SseParser::new().parse(create_mock_ssl_event_with_buf(40, 0xF000, plain.clone(), 0));
        assert!(!expected.is_empty(), "precondition: plaintext parses");
        assert_eq!(
            pair.response.sse_event_count(),
            expected.len(),
            "decoded stream must yield exactly the plaintext's event count"
        );
        for (got, want) in pair.response.sse_events.iter().zip(&expected) {
            assert_eq!(got.event, want.event, "event name must survive");
            assert_eq!(got.body_str(), want.body_str(), "data must be byte-equal");
        }
        let all_data: String = pair
            .response
            .sse_events
            .iter()
            .map(|e| e.body_str().to_string())
            .collect();
        assert!(
            all_data.contains("\"input_tokens\":25"),
            "message_start usage must survive decompression: {all_data:?}"
        );
        assert!(
            all_data.contains("\"output_tokens\":42"),
            "message_delta usage must survive decompression: {all_data:?}"
        );
        // Review F4: response id and finish reason feed builder.rs recovery,
        // so pin them explicitly.
        assert!(
            all_data.contains("\"id\":\"msg_01ABC\""),
            "response id must survive decompression: {all_data:?}"
        );
        assert!(
            all_data.contains("\"stop_reason\":\"end_turn\""),
            "finish reason must survive decompression: {all_data:?}"
        );
    }

    /// Drive one full fragmented zstd SSE request/response cycle on the given
    /// connection and assert full-fidelity decoding (issue #2267 plumbing,
    /// shared by the keep-alive and HTTP/1.1 variants). `version` is the HTTP
    /// version marker carried by both request and response.
    fn run_fragmented_zstd_cycle(
        aggregator: &mut HttpConnectionAggregator,
        pid: u32,
        ssl_ptr: u64,
        version: u8,
    ) {
        let (plain, compressed) = make_streaming_zstd_sse();

        let req_event = create_mock_ssl_event(pid, ssl_ptr);
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            version,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: req_event,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        // zstd content-encoding, NO transfer-encoding: no chunked terminator
        // will ever arrive, so only incremental decoding can recover events.
        let resp_event = create_mock_ssl_event_with_buf(pid, ssl_ptr, Vec::new(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        headers.insert("content-encoding".to_string(), "zstd".to_string());
        let response = ParsedResponse {
            version,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: 0,
            source_event: resp_event,
        };
        assert!(aggregator.process_response(response).is_none());

        for frag in compressed.chunks(11) {
            let result = aggregator.process_raw_body_data(&raw_frag(pid, ssl_ptr, frag.to_vec()));
            assert!(result.is_none(), "fragments must keep buffering");
        }

        let conn_id = ConnectionId { pid, ssl_ptr };
        let done_event =
            create_mock_ssl_event_with_buf(pid, ssl_ptr, b"data: [DONE]\n\n".to_vec(), 0);
        let done = ParsedSseEvent::new_done_marker(done_event);
        let pair = match aggregator.process_sse_event(&conn_id, done) {
            Some(AggregatedResult::SseComplete(pair)) => pair,
            other => panic!("expected SseComplete, got {other:?}"),
        };

        let expected =
            SseParser::new().parse(create_mock_ssl_event_with_buf(pid, ssl_ptr, plain, 0));
        assert_eq!(
            pair.response.sse_event_count(),
            expected.len(),
            "decoded stream must yield exactly the plaintext's event count"
        );
        for (got, want) in pair.response.sse_events.iter().zip(&expected) {
            assert_eq!(got.event, want.event, "event name must survive");
            assert_eq!(got.body_str(), want.body_str(), "data must be byte-equal");
        }
    }

    #[test]
    fn keepalive_second_zstd_response_gets_fresh_decoder() {
        // Review F6: completing the first stream pops the SseActive entry
        // without re-inserting it, dropping the aggregator's Rc to the first
        // decoder; the second response then builds a brand-new decoder in
        // sse_entry_state. If the first stream's decoder leaked into the
        // second, its open-frame state would reject the second frame header
        // (poison) and the full-fidelity assertions inside the cycle would
        // fail — so two clean back-to-back cycles prove the isolation.
        let mut aggregator = HttpConnectionAggregator::new();
        run_fragmented_zstd_cycle(&mut aggregator, 50, 0xAB00, 2);
        run_fragmented_zstd_cycle(&mut aggregator, 50, 0xAB00, 2);
    }

    #[test]
    fn http11_non_chunked_zstd_stream_decodes_incrementally() {
        // Review F7: HTTP/1.1 without Transfer-Encoding rides the same
        // no-terminator path as HTTP/2 framing and must equally benefit from
        // incremental decoding.
        let mut aggregator = HttpConnectionAggregator::new();
        run_fragmented_zstd_cycle(&mut aggregator, 51, 0xAC00, 11);
    }

    // ── OpenAI Responses API SSE continuation buffer tests ───────────────

    #[test]
    fn test_with_capacity_creates_continuation_buffers() {
        let aggregator = HttpConnectionAggregator::with_capacity(4);
        assert_eq!(aggregator.active_connections(), 0);
    }

    #[test]
    fn test_needs_sse_continuation_buffer_none_request() {
        assert!(!needs_sse_continuation_buffer(None));
    }

    fn enter_uncompressed_responses_sse_active(
        aggregator: &mut HttpConnectionAggregator,
        pid: u32,
        ssl_ptr: u64,
    ) -> ConnectionId {
        let req_event = create_mock_ssl_event_with_buf(pid, ssl_ptr, Vec::new(), 1);
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: req_event,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        let resp_event = create_mock_ssl_event_with_buf(pid, ssl_ptr, Vec::new(), 0);
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: 0,
            source_event: resp_event,
        };
        aggregator.process_response(response);
        ConnectionId { pid, ssl_ptr }
    }

    #[test]
    fn test_sse_continuation_buffer_in_process_raw_body_data() {
        let mut aggregator = HttpConnectionAggregator::new();
        let conn_id = enter_uncompressed_responses_sse_active(&mut aggregator, 20, 0xA000);
        assert!(aggregator.is_sse_active(&conn_id));

        let chunk = b"event:response.completed\ndata:{\"usage\":{\"input_tokens\":57";
        let raw = create_mock_ssl_event_with_buf(20, 0xA000, chunk.to_vec(), 0);
        let result = aggregator.process_raw_body_data(&raw);
        assert!(
            result.is_none(),
            "raw body data on SseActive should keep buffering"
        );

        // Complete the stream and inspect the continuation buffer captured.
        let done_event =
            create_mock_ssl_event_with_buf(20, 0xA000, b"data: [DONE]\n\n".to_vec(), 0);
        let done = ParsedSseEvent::new(None, None, None, 6, 6, done_event);
        let result = aggregator.process_sse_event(&conn_id, done);
        let pair = match result {
            Some(AggregatedResult::SseComplete(pair)) => pair,
            other => panic!("expected SseComplete, got {other:?}"),
        };
        let buf = pair
            .response
            .sse_continuation_bytes
            .expect("continuation buffer should be present");
        assert!(buf.windows(chunk.len()).any(|w| w == chunk));
    }

    #[test]
    fn test_sse_continuation_buffer_in_process_sse_event() {
        let mut aggregator = HttpConnectionAggregator::new();
        let conn_id = enter_uncompressed_responses_sse_active(&mut aggregator, 21, 0xB000);

        // Build an SSE event whose underlying SSL buffer carries extra bytes.
        let payload = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}";
        let src_event = create_mock_ssl_event_with_buf(21, 0xB000, payload.to_vec(), 0);
        let event = ParsedSseEvent::new(None, None, None, 0, payload.len(), src_event);
        let result = aggregator.process_sse_event(&conn_id, event);
        assert!(
            result.is_none(),
            "non-terminal SSE event should keep streaming"
        );

        // Complete the stream and verify the source buffer was appended.
        let done_event =
            create_mock_ssl_event_with_buf(21, 0xB000, b"data: [DONE]\n\n".to_vec(), 0);
        let done = ParsedSseEvent::new(None, None, None, 6, 6, done_event);
        let result = aggregator.process_sse_event(&conn_id, done);
        let pair = match result {
            Some(AggregatedResult::SseComplete(pair)) => pair,
            other => panic!("expected SseComplete, got {other:?}"),
        };
        let buf = pair
            .response
            .sse_continuation_bytes
            .expect("continuation buffer should be populated");
        assert!(buf.windows(payload.len()).any(|w| w == payload));
    }

    // ── Boundary tests requested in PR review (#8) ──────────────────────

    #[test]
    fn test_needs_sse_continuation_buffer_non_responses_path() {
        // Negative: chat completions and sub-paths must not trigger buffering.
        let event = create_mock_ssl_event_with_buf(1, 1, Vec::new(), 1);
        let make_req = |path: &str| ParsedRequest {
            method: "POST".to_string(),
            path: path.to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: event.clone(),
            reassembled_body: None,
        };
        assert!(!needs_sse_continuation_buffer(Some(&make_req(
            "/v1/chat/completions"
        ))));
        assert!(!needs_sse_continuation_buffer(Some(&make_req(
            "/v1/responses/abc/items"
        ))));
        // Positive: exact and query-string variants should match.
        assert!(needs_sse_continuation_buffer(Some(&make_req(
            "/v1/responses"
        ))));
        assert!(needs_sse_continuation_buffer(Some(&make_req(
            "/compatible-mode/v1/responses"
        ))));
        assert!(needs_sse_continuation_buffer(Some(&make_req(
            "/v1/responses?stream=true"
        ))));
    }

    #[test]
    fn test_sse_continuation_buffer_max_bytes_truncation() {
        // Feed > 1 MiB through process_raw_body_data and verify the buffer
        // is capped at MAX_CONTINUATION_BYTES (1 << 20).
        let mut aggregator = HttpConnectionAggregator::new();
        let conn_id = enter_uncompressed_responses_sse_active(&mut aggregator, 30, 0xC000);

        const MAX_CAP: usize = 1 << 20; // 1 MiB
        let oversized: Vec<u8> = vec![b'x'; MAX_CAP + 4096];
        let raw = create_mock_ssl_event_with_buf(30, 0xC000, oversized.clone(), 0);
        let _ = aggregator.process_raw_body_data(&raw);

        // Complete the stream.
        let done_event =
            create_mock_ssl_event_with_buf(30, 0xC000, b"data: [DONE]\n\n".to_vec(), 0);
        let done = ParsedSseEvent::new(None, None, None, 6, 6, done_event);
        let result = aggregator.process_sse_event(&conn_id, done);
        let pair = match result {
            Some(AggregatedResult::SseComplete(pair)) => pair,
            other => panic!("expected SseComplete, got {other:?}"),
        };
        let buf = pair
            .response
            .sse_continuation_bytes
            .expect("continuation buffer should exist");
        assert_eq!(
            buf.len(),
            MAX_CAP,
            "continuation buffer must be capped at 1 MiB"
        );
    }

    #[test]
    fn test_sse_continuation_buffer_dedup_same_src_ptr() {
        // Two SSE events sharing the same source SslEvent (same Rc pointer)
        // must contribute to the continuation buffer only once.
        let mut aggregator = HttpConnectionAggregator::new();
        let conn_id = enter_uncompressed_responses_sse_active(&mut aggregator, 31, 0xD000);

        let payload = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}";
        // Both ParsedSseEvents reference the *same* Rc<SslEvent>.
        let src_event = create_mock_ssl_event_with_buf(31, 0xD000, payload.to_vec(), 0);
        let event1 = ParsedSseEvent::new(None, None, None, 0, payload.len(), src_event.clone());
        let _ = aggregator.process_sse_event(&conn_id, event1);

        let event2 = ParsedSseEvent::new(None, None, None, 0, payload.len(), src_event);
        let _ = aggregator.process_sse_event(&conn_id, event2);

        // Complete the stream.
        let done_event =
            create_mock_ssl_event_with_buf(31, 0xD000, b"data: [DONE]\n\n".to_vec(), 0);
        let done = ParsedSseEvent::new(None, None, None, 6, 6, done_event);
        let result = aggregator.process_sse_event(&conn_id, done);
        let pair = match result {
            Some(AggregatedResult::SseComplete(pair)) => pair,
            other => panic!("expected SseComplete, got {other:?}"),
        };
        let buf = pair
            .response
            .sse_continuation_bytes
            .expect("continuation buffer should exist");
        // Count occurrences of the payload — dedup means it should appear
        // at most once.
        let count = buf.windows(payload.len()).filter(|w| *w == payload).count();
        assert_eq!(
            count, 1,
            "same-source SSE events must contribute to the buffer only once"
        );
    }

    #[test]
    fn test_sse_continuation_buffer_done_event_bypasses_dedup() {
        // The done event's source chunk must always be appended to the
        // continuation buffer, even when it shares the same source_event
        // pointer as a prior event. Without this bypass, the usage data
        // in the done event's chunk would be lost to dedup.
        let mut aggregator = HttpConnectionAggregator::new();
        let conn_id = enter_uncompressed_responses_sse_active(&mut aggregator, 32, 0xE000);

        // Two events sharing the same source SslEvent — first is non-done,
        // second is done. The done event carries usage data in its source
        // buffer that must not be skipped.
        let payload = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}";
        let usage_payload = b"data: {\"usage\":{\"input_tokens\":42,\"output_tokens\":7}}";

        // Non-done event from a different source
        let src1 = create_mock_ssl_event_with_buf(32, 0xE000, payload.to_vec(), 0);
        let event1 = ParsedSseEvent::new(None, None, None, 0, payload.len(), src1);
        let _ = aggregator.process_sse_event(&conn_id, event1);

        // Done event sharing the SAME source pointer as event1 — its buffer
        // contains usage data that differs from event1's payload.
        let src_done = create_mock_ssl_event_with_buf(32, 0xE000, usage_payload.to_vec(), 0);
        let done = ParsedSseEvent::new_done_marker(src_done);
        let result = aggregator.process_sse_event(&conn_id, done);
        let pair = match result {
            Some(AggregatedResult::SseComplete(pair)) => pair,
            other => panic!("expected SseComplete, got {other:?}"),
        };
        let buf = pair
            .response
            .sse_continuation_bytes
            .expect("continuation buffer should exist");
        // The done event's payload must appear in the buffer despite dedup.
        assert!(
            buf.windows(usage_payload.len()).any(|w| w == usage_payload),
            "done event source chunk must be in continuation buffer even with dedup"
        );
    }

    #[test]
    fn test_with_limits_sets_fields() {
        let agg = HttpConnectionAggregator::with_limits(10, 4096, Duration::from_secs(30));
        assert_eq!(agg.max_body_bytes(), 4096);
    }

    #[test]
    fn test_with_limits_min_body_bytes() {
        // max_body_bytes should be at least 1024
        let agg = HttpConnectionAggregator::with_limits(10, 100, Duration::from_secs(60));
        assert_eq!(agg.max_body_bytes(), 1024);
    }

    #[test]
    fn test_with_limits_zero_capacity_does_not_panic() {
        // Regression: capacity 0 used to unwrap on NonZeroUsize::new and
        // panic; it must clamp like max_body_bytes does.
        let agg = HttpConnectionAggregator::with_limits(0, 4096, Duration::from_secs(30));
        assert_eq!(agg.max_body_bytes(), 4096);
        assert_eq!(agg.active_connections(), 0);
    }

    #[test]
    fn test_evict_idle_and_oversized_empty() {
        let mut agg = HttpConnectionAggregator::with_limits(10, 8192, Duration::from_secs(1));
        // Should not panic on empty aggregator
        agg.evict_idle_and_oversized();
    }

    #[test]
    fn test_touch_and_evict_after_timeout() {
        let mut agg = HttpConnectionAggregator::with_limits(10, 8192, Duration::from_millis(50));
        let conn_id = ConnectionId { pid: 1, ssl_ptr: 1 };
        agg.last_activity.push(conn_id, Instant::now());
        agg.connections.push(conn_id, ConnectionState::Idle);

        // Before timeout: should not evict
        agg.evict_idle_and_oversized();
        assert!(agg.connections.peek(&conn_id).is_some());

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(60));
        agg.evict_idle_and_oversized();
        assert!(agg.connections.peek(&conn_id).is_none());
        assert_eq!(agg.metrics().eviction_count, 1);
    }

    #[test]
    fn test_idle_snapshot_keeps_sse_active_for_resumed_stream() {
        let mut agg = HttpConnectionAggregator::with_limits(10, 8192, Duration::from_millis(50));
        let conn_id = ConnectionId {
            pid: 1234,
            ssl_ptr: 0x7000,
        };
        let event = create_mock_ssl_event(conn_id.pid, conn_id.ssl_ptr);
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: event.clone(),
            reassembled_body: None,
        };
        agg.process_request(request);

        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/event-stream".to_string());
        let response = ParsedResponse {
            version: 11,
            status_code: 200,
            reason: "OK".to_string(),
            headers,
            body_offset: 0,
            body_len: 0,
            source_event: event,
        };
        assert!(agg.process_response(response).is_none());

        std::thread::sleep(Duration::from_millis(60));
        let snapshots = agg.snapshot_idle_connections();

        assert_eq!(snapshots.len(), 1);
        assert!(matches!(snapshots[0].1, ConnectionState::SseActive { .. }));

        let done_event = create_mock_ssl_event_with_buf(
            conn_id.pid,
            conn_id.ssl_ptr,
            b"data: [DONE]\n\n".to_vec(),
            0,
        );
        let done = ParsedSseEvent::new(None, None, None, 6, 6, done_event);
        let result = agg.process_sse_event(&conn_id, done);
        assert!(
            matches!(result, Some(AggregatedResult::SseComplete(_))),
            "idle snapshot must not remove the in-flight stream state"
        );
    }

    #[test]
    fn test_oversized_request_body_pending_is_evicted() {
        let mut agg = HttpConnectionAggregator::with_limits(10, 1024, Duration::from_secs(60));
        let conn_id = ConnectionId {
            pid: 1234,
            ssl_ptr: 0x7100,
        };
        let event = create_mock_ssl_event(conn_id.pid, conn_id.ssl_ptr);
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: event,
            reassembled_body: None,
        };

        agg.connections.push(
            conn_id,
            ConnectionState::RequestBodyPending {
                request,
                expected_body_len: Some(4096),
                body_buffer: vec![b'x'; 2048],
            },
        );
        agg.last_activity.push(conn_id, Instant::now());
        agg.sse_continuation_buffers
            .push(conn_id, b"stale".to_vec());
        agg.last_appended_src_ptr.push(conn_id, 42);

        agg.evict_idle_and_oversized();

        assert!(agg.connections.peek(&conn_id).is_none());
        assert!(agg.last_activity.peek(&conn_id).is_none());
        assert!(agg.sse_continuation_buffers.peek(&conn_id).is_none());
        assert!(agg.last_appended_src_ptr.peek(&conn_id).is_none());
        let metrics = agg.metrics();
        assert_eq!(metrics.pending_connection_count, 0);
        assert_eq!(metrics.pending_connection_bytes, 0);
        assert_eq!(metrics.eviction_count, 1);
    }

    /// The per-connection SSE side caches are released on insert, on LRU
    /// eviction and on SSE completion, but the two PID drain paths (crash
    /// detection and the periodic dead-PID sweep) only released the
    /// connection, activity and snapshot maps — so a dead process's
    /// continuation buffer stayed resident and a later connection reusing the
    /// same (pid, ssl_ptr) key appended onto the stale bytes.
    #[test]
    fn test_pid_drains_release_the_side_caches() {
        let mut agg = HttpConnectionAggregator::with_limits(10, 1024, Duration::from_secs(60));
        let conn_id = ConnectionId {
            pid: 4242,
            ssl_ptr: 0x9000,
        };
        let event = create_mock_ssl_event(conn_id.pid, conn_id.ssl_ptr);
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: event,
            reassembled_body: None,
        };
        agg.connections.push(
            conn_id,
            ConnectionState::RequestBodyPending {
                request,
                expected_body_len: Some(4096),
                body_buffer: Vec::new(),
            },
        );
        agg.last_activity.push(conn_id, Instant::now());
        agg.sse_continuation_buffers
            .push(conn_id, b"stale".to_vec());
        agg.last_appended_src_ptr.push(conn_id, 42);

        agg.drain_connections_for_pid(conn_id.pid);

        assert!(agg.connections.peek(&conn_id).is_none());
        assert!(
            agg.sse_continuation_buffers.peek(&conn_id).is_none(),
            "the crash drain must not leave the continuation buffer behind"
        );
        assert!(
            agg.last_appended_src_ptr.peek(&conn_id).is_none(),
            "the crash drain must not leave the append cursor behind"
        );
    }

    #[test]
    fn test_capacity_eviction_releases_the_side_caches() {
        let mut agg = HttpConnectionAggregator::with_capacity(1);
        let conn_a = ConnectionId { pid: 1, ssl_ptr: 1 };
        let conn_b = ConnectionId { pid: 1, ssl_ptr: 2 };

        agg.insert(conn_a, ConnectionState::Idle);
        agg.sse_continuation_buffers.push(conn_a, vec![0u8; 1024]);
        agg.last_appended_src_ptr.push(conn_a, 7);

        // A second connection evicts the first by capacity.
        agg.insert(conn_b, ConnectionState::Idle);

        assert!(agg.connections.peek(&conn_a).is_none());
        assert!(
            agg.sse_continuation_buffers.peek(&conn_a).is_none(),
            "the evicted connection's continuation buffer must be released with it"
        );
        assert!(agg.last_appended_src_ptr.peek(&conn_a).is_none());
    }

    #[test]
    fn test_connection_metrics_report_retained_payload_bytes() {
        let mut agg = HttpConnectionAggregator::with_capacity(2);
        let conn_id = ConnectionId {
            pid: 1234,
            ssl_ptr: 0x7200,
        };
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            version: 11,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: create_mock_ssl_event(conn_id.pid, conn_id.ssl_ptr),
            reassembled_body: None,
        };
        agg.connections.push(
            conn_id,
            ConnectionState::RequestBodyPending {
                request,
                expected_body_len: Some(1024),
                body_buffer: vec![b'x'; 640],
            },
        );
        agg.sse_continuation_buffers.push(conn_id, vec![b'y'; 128]);

        assert_eq!(
            agg.metrics(),
            ConnectionMetrics {
                connection_cache_bytes: 768,
                pending_connection_count: 1,
                pending_connection_bytes: 640,
                eviction_count: 0,
            }
        );

        agg.connections.push(
            conn_id,
            ConnectionState::ResponsePending {
                request: None,
                assembly: PendingResponse::Headers(create_mock_ssl_event_with_buf(
                    conn_id.pid,
                    conn_id.ssl_ptr,
                    vec![b'z'; 512],
                    0,
                )),
            },
        );
        assert_eq!(agg.metrics().pending_connection_bytes, 512);
    }

    #[test]
    fn test_connection_metrics_count_lru_eviction() {
        let mut agg = HttpConnectionAggregator::with_capacity(1);
        for ssl_ptr in [0x7300, 0x7301] {
            let request = ParsedRequest {
                method: "POST".to_string(),
                path: "/v1/messages".to_string(),
                version: 11,
                headers: HashMap::new(),
                body_offset: 0,
                body_len: 0,
                source_event: create_mock_ssl_event(1234, ssl_ptr),
                reassembled_body: None,
            };
            agg.process_request(request);
        }

        let metrics = agg.metrics();
        assert_eq!(metrics.pending_connection_count, 1);
        assert_eq!(metrics.eviction_count, 1);
    }

    #[test]
    fn test_connection_metrics_merge_saturates_counters() {
        let combined = ConnectionMetrics {
            connection_cache_bytes: usize::MAX,
            pending_connection_count: usize::MAX,
            pending_connection_bytes: usize::MAX,
            eviction_count: u64::MAX,
        }
        .saturating_add(ConnectionMetrics {
            connection_cache_bytes: 1,
            pending_connection_count: 1,
            pending_connection_bytes: 1,
            eviction_count: 1,
        });

        assert_eq!(combined.connection_cache_bytes, usize::MAX);
        assert_eq!(combined.pending_connection_count, usize::MAX);
        assert_eq!(combined.pending_connection_bytes, usize::MAX);
        assert_eq!(combined.eviction_count, u64::MAX);
    }

    /// `Transfer-Encoding` values are case-insensitive (RFC 7230 §3.3.1) and
    /// may be a list. The request path matched the literal lower-case
    /// substring while the response path compares tokens case-insensitively,
    /// so `Transfer-Encoding: Chunked` was read as "no length, not chunked",
    /// the body was declared complete on the first read and every remaining
    /// body read was dropped.
    #[test]
    fn test_chunked_request_detection_is_case_insensitive() {
        let mut aggregator = HttpConnectionAggregator::new();

        let partial_body = b"POST /chat HTTP/1.1\r\nTransfer-Encoding: Chunked\r\n\r\n5\r\nhe";
        let event = create_mock_ssl_event_with_buf(9102, 0xE100, partial_body.to_vec(), 1);
        let header_end = partial_body
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap()
            + 4;

        let mut headers = HashMap::new();
        headers.insert("transfer-encoding".to_string(), "Chunked".to_string());
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/chat".to_string(),
            version: 1,
            headers,
            body_offset: header_end,
            body_len: partial_body.len() - header_end,
            source_event: event,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        let conn_id = ConnectionId {
            pid: 9102,
            ssl_ptr: 0xE100,
        };
        assert!(
            matches!(
                aggregator.connections.peek(&conn_id),
                Some(ConnectionState::RequestBodyPending { .. })
            ),
            "a mixed-case chunked encoding must keep the request body open"
        );
    }

    #[test]
    fn test_chunked_request_body_with_embedded_terminator_is_not_complete() {
        // Valid chunked framing whose first chunk's *data* contains the 5-byte
        // terminator pattern (an agent prompt quoting a chunked example, say).
        // The compressed-response path detects completion by walking the chunk
        // framing (`chunked_stream_complete`, e6c787258); the request paths
        // still scan for the byte pattern, so the body is declared complete
        // mid-stream and the remaining chunks are parsed as a new request.
        let mut aggregator = HttpConnectionAggregator::new();

        let headers_and_partial =
            b"POST /chat HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n9\r\nAB0\r\n\r\nCD\r\n";
        let event1 = create_mock_ssl_event_with_buf(9101, 0xE000, headers_and_partial.to_vec(), 1);
        let header_end = headers_and_partial
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap()
            + 4;
        let body_len = headers_and_partial.len() - header_end;

        let mut headers = HashMap::new();
        headers.insert("transfer-encoding".to_string(), "chunked".to_string());
        let request = ParsedRequest {
            method: "POST".to_string(),
            path: "/chat".to_string(),
            version: 1,
            headers,
            body_offset: header_end,
            body_len,
            source_event: event1,
            reassembled_body: None,
        };
        aggregator.process_request(request);

        let conn_id = ConnectionId {
            pid: 9101,
            ssl_ptr: 0xE000,
        };
        assert!(
            matches!(
                aggregator.connections.peek(&conn_id),
                Some(ConnectionState::RequestBodyPending { .. })
            ),
            "an embedded terminator pattern must not complete a chunked request body"
        );

        // The real terminating chunk completes it.
        let rest = SslEvent {
            source: 0,
            timestamp_ns: 2000,
            delta_ns: 0,
            pid: 9101,
            tid: 1,
            uid: 0,
            len: 12,
            rw: 1,
            comm: String::new(),
            buf: b"2\r\nEF\r\n0\r\n\r\n".to_vec(),
            is_handshake: false,
            ssl_ptr: 0xE000,
        };
        aggregator.process_raw_body_data(&rest);
        assert!(
            aggregator.has_pending_request(&conn_id),
            "the real terminating chunk completes the body"
        );
    }
}
