//! Unified Aggregator - high-level entry point for event aggregation
//!
//! This module provides a unified interface for aggregating parsed messages.
//! It combines HTTP Connection Aggregator and Process Event Aggregator.

use super::RemovedConnection;
use super::http::{ConnectionId, ConnectionMetrics, HttpConnectionAggregator};
use super::http2::Http2StreamAggregator;
use super::proctrace::ProcessEventAggregator;
use super::result::AggregatedResult;
use crate::chrome_trace::export_trace_events;
use crate::config::{DEFAULT_CONNECTION_CAPACITY, RuntimeLimits};
use crate::parser::http::ParsedRequest;
use crate::parser::{ParseResult, ParsedMessage};
use crate::runtime_metrics::StageTimer;
use std::time::{Duration, Instant};

/// Unified aggregator for all event types
///
/// This aggregator provides a unified entry point for aggregating parsed messages.
/// It internally manages HTTP connections, HTTP/2 streams, and process lifecycles.
pub struct Aggregator {
    http: HttpConnectionAggregator,
    http2: Http2StreamAggregator,
    process: ProcessEventAggregator,
    /// Last time idle/overweight HTTP connections were evicted.
    last_eviction: Instant,
    /// Eviction period for idle/overweight HTTP connections.
    eviction_period: Duration,
    bytes_budget: usize,
}

impl Default for Aggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl Aggregator {
    /// Create new unified aggregator with default limits.
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_CONNECTION_CAPACITY, &RuntimeLimits::default())
    }

    /// Create new unified aggregator with explicit memory/time limits.
    pub fn with_limits(connection_capacity: usize, limits: &RuntimeLimits) -> Self {
        let idle_timeout = Duration::from_secs(limits.connection_idle_timeout_secs);
        let mut http = HttpConnectionAggregator::with_limits(
            connection_capacity,
            limits.max_connection_body_bytes,
            idle_timeout,
        );
        http.configure_retention(limits);
        http.set_compact_sse(!limits.retain_raw_sse);
        let mut http2 = Http2StreamAggregator::with_limits(
            connection_capacity.saturating_mul(4),
            limits.max_connection_body_bytes,
            Duration::from_secs(limits.connection_hard_timeout_secs),
        );
        http2.configure_retention(limits);
        Aggregator {
            http,
            http2,
            process: ProcessEventAggregator::new(),
            last_eviction: Instant::now(),
            eviction_period: idle_timeout.min(Duration::from_secs(10)),
            bytes_budget: limits.connection_max_bytes,
        }
    }

    /// Set the HTTP/1 header cap the connection aggregator applies when it
    /// re-parses header blocks (interim 1xx chains, byte-assembly path).
    ///
    /// Threads `AgentsightConfig::max_headers` to the last two parse sites
    /// that do not go through [`crate::parser::Parser`]; the cap is clamped
    /// to at least one, like the parser's own clamp.
    pub fn with_http_max_headers(mut self, max_headers: usize) -> Self {
        self.http.set_max_headers(max_headers);
        self
    }

    /// Process a parsed message
    ///
    /// Returns aggregated results when complete units are formed.
    /// Note: Returns a Vec because HTTP/2 frame processing can produce multiple completed streams.
    fn process_message(&mut self, msg: ParsedMessage) -> Vec<AggregatedResult> {
        match msg {
            ParsedMessage::Request(req) => {
                self.http.process_request(req);
                vec![]
            }
            ParsedMessage::Response(resp) => self.http.process_response(resp).into_iter().collect(),
            ParsedMessage::SseEvent(sse_event) => {
                let conn_id = ConnectionId::from_ssl_event(sse_event.source_event());
                self.http
                    .process_sse_event(&conn_id, sse_event)
                    .into_iter()
                    .collect()
            }
            ParsedMessage::ProcEvent(proc_event) => self
                .process
                .process_parsed_event(&proc_event)
                .map(AggregatedResult::ProcessComplete)
                .into_iter()
                .collect(),
            ParsedMessage::Http2Frames(frames) => {
                // Use HTTP/2 stream aggregator to correlate frames by stream_id
                let completed_streams = self.http2.process_frames(frames);
                completed_streams
                    .into_iter()
                    .map(AggregatedResult::Http2StreamComplete)
                    .collect()
            }
            ParsedMessage::RawData(ssl_event) => self
                .http
                .process_raw_body_data(&ssl_event)
                .into_iter()
                .collect(),
        }
    }

    /// Process parse result
    pub fn process_result(&mut self, result: ParseResult) -> Vec<AggregatedResult> {
        let timer = StageTimer::start("aggregator");
        log::trace!(
            "Aggregating parsed results({}): {}",
            result.messages.len(),
            result
                .messages
                .iter()
                .map(|x| x.message_type())
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Periodically evict idle or overweight HTTP connection states to keep
        // memory bounded.  Runs cheaply on every parse result because the check
        // is O(number of active connections).
        let now = Instant::now();
        if now.duration_since(self.last_eviction) >= self.eviction_period {
            self.http.evict_idle_and_oversized();
            self.http2.evict_idle_and_oversized();
            self.last_eviction = now;
        }

        // One SSL read may yield several SSE messages (including a synthetic
        // chunk terminator). Feed its original, longest source buffer only once
        // when HTTP response framing owns the connection.
        let response_event = result
            .messages
            .iter()
            .filter_map(|message| match message {
                ParsedMessage::Request(r) => Some(r.source_event.as_ref()),
                ParsedMessage::Response(r) => Some(r.source_event.as_ref()),
                ParsedMessage::RawData(event) => Some(event.as_ref()),
                ParsedMessage::SseEvent(event) => Some(event.source_event()),
                ParsedMessage::Http2Frames(frames) => {
                    frames.first().map(|f| f.source_event.as_ref())
                }
                ParsedMessage::ProcEvent(_) => None,
            })
            .max_by_key(|event| event.buf_size());
        let captured_sse = result
            .messages
            .iter()
            .any(|message| matches!(message, ParsedMessage::SseEvent(_)));
        let handled = response_event.and_then(|event| {
            if self.http.accepts_response_bytes(event) || self.http.accepts_compact_sse_bytes(event)
            {
                Some(self.http.process_raw_body_data(event))
            } else if captured_sse && self.http.accepts_unmatched_sse(event) {
                // A chunk terminator arriving after semantic completion has
                // no response payload and must not start another connection.
                Some(if result.messages.iter().any(|message| {
                    matches!(message, ParsedMessage::SseEvent(event) if !event.is_synthetic_done())
                }) {
                    self.http.start_unmatched_sse(event)
                } else {
                    None
                })
            } else {
                None
            }
        });
        if let Some(completed) = handled {
            let mut results = self.http.take_ready();
            results.extend(completed);
            self.enforce_bytes_budget();
            results.extend(self.http.take_ready());
            results.extend(self.http2.take_ready());
            for result in &results {
                export_trace_events(result);
            }
            return results;
        }

        let mut results = self.http.take_ready();
        results.extend(self.http2.take_ready());
        for message in result.messages {
            let completed = self.process_message(message);
            for result in &completed {
                let id = match result {
                    AggregatedResult::HttpComplete(pair) | AggregatedResult::SseComplete(pair) => {
                        Some(pair.connection_id)
                    }
                    AggregatedResult::RequestOnly { connection_id, .. }
                    | AggregatedResult::ResponseOnly { connection_id, .. } => Some(*connection_id),
                    _ => None,
                };
                if let Some(id) = id {
                    self.http.cleanup_completed(&id);
                }
            }
            results.extend(self.http.take_ready());
            results.extend(completed);
            self.enforce_bytes_budget();
            results.extend(self.http.take_ready());
            results.extend(self.http2.take_ready());
        }

        self.enforce_bytes_budget();
        results.extend(self.http.take_ready());
        results.extend(self.http2.take_ready());

        // Export chrome trace if enabled
        for r in &results {
            export_trace_events(r);
        }

        timer.record_outputs(results.len());
        results
    }

    pub(crate) fn set_compact_sse(&mut self, enabled: bool) {
        self.http.set_compact_sse(enabled);
        self.http2.set_compact_sse(enabled);
    }

    /// Bypass message-list construction for an already-framed semantic-only SSE read.
    pub(crate) fn process_sse_capture(
        &mut self,
        event: &crate::probes::sslsniff::SslEvent,
    ) -> Option<Vec<AggregatedResult>> {
        if !self.http.accepts_compact_sse_bytes(event) {
            return None;
        }
        let complete = self.http.process_raw_body_data(event);
        self.enforce_bytes_budget();
        let mut results = self.http.take_ready();
        results.extend(self.http2.take_ready());
        results.extend(complete);
        for result in &results {
            export_trace_events(result);
        }
        Some(results)
    }

    /// Get reference to HTTP aggregator
    pub fn http(&self) -> &HttpConnectionAggregator {
        &self.http
    }

    /// Get mutable reference to HTTP aggregator
    pub fn http_mut(&mut self) -> &mut HttpConnectionAggregator {
        &mut self.http
    }

    /// Return combined HTTP/1 connection and HTTP/2 stream correlation metrics.
    pub(crate) fn connection_metrics(&self) -> ConnectionMetrics {
        self.http.metrics().saturating_add(self.http2.metrics())
    }

    /// Get reference to process aggregator
    pub fn process(&self) -> &ProcessEventAggregator {
        &self.process
    }

    /// Get mutable reference to process aggregator
    pub fn process_mut(&mut self) -> &mut ProcessEventAggregator {
        &mut self.process
    }

    /// Check if there are any pending aggregations
    pub fn has_pending(&self) -> bool {
        self.http.has_pending() || self.http2.has_pending() || self.process.has_pending()
    }

    /// Clear all aggregations
    pub fn clear(&mut self) {
        self.http.clear();
        self.http2.clear();
        self.process.clear();
        self.last_eviction = Instant::now();
    }

    /// Drain all connections belonging to a specific PID.
    ///
    /// Used by crash detection on `ProcMon::Exit` to immediately extract
    /// in-flight connections before the periodic drain check runs.
    pub(crate) fn drain_connections_for_pid(&mut self, pid: u32) -> Vec<RemovedConnection> {
        self.http2.drain_for_pid(pid);
        self.http.drain_connections_for_pid(pid)
    }

    /// Drain connections whose PID is no longer alive.
    ///
    /// Delegates to the HTTP aggregator's dead-PID drain.
    pub(crate) fn drain_dead_pid_connections(&mut self) -> Vec<RemovedConnection> {
        self.http2.drain_dead_pids();
        self.http.drain_dead_pid_connections()
    }

    /// Snapshot in-flight HTTP connections that exceeded the idle timeout.
    ///
    /// Used to persist evidence for manually interrupted streams where the
    /// agent process remains alive, so dead-PID draining would never run.
    pub(crate) fn snapshot_idle_connections<T>(
        &mut self,
        make: impl FnMut(&ConnectionId, &ParsedRequest) -> Option<T>,
    ) -> Vec<T> {
        self.http.snapshot_idle_connections(make)
    }

    pub(crate) fn queue_removed(&mut self, removed: Vec<RemovedConnection>) {
        self.http.queue_removed(removed);
    }

    pub(crate) fn drain_pending(&mut self) -> Vec<AggregatedResult> {
        self.drain_pending_with_reason(super::RemoveReason::Shutdown)
    }

    pub(crate) fn drain_pending_with_reason(
        &mut self,
        reason: super::RemoveReason,
    ) -> Vec<AggregatedResult> {
        self.http.drain_pending(reason);
        self.http2.abandon_pending(reason);
        self.enforce_bytes_budget();
        let mut results = self.http.take_ready();
        results.extend(self.http2.take_ready());
        results
    }

    /// Run correlation maintenance without requiring captured traffic.
    pub(crate) fn tick(&mut self, now: Instant) -> Vec<AggregatedResult> {
        self.http.tick(now);
        self.http2.tick(now);
        self.enforce_bytes_budget();
        let mut results = self.http.take_ready();
        results.extend(self.http2.take_ready());
        results
    }

    fn enforce_bytes_budget(&mut self) {
        while self.bytes_budget != 0
            && self.connection_metrics().connection_cache_bytes > self.bytes_budget
        {
            let h1 = self.http.oldest_activity();
            let h2 = self.http2.oldest_activity();
            let removed = match (h1, h2) {
                (Some(a), Some(b)) if a <= b => self.http.abandon_oldest(),
                (Some(_), None) => self.http.abandon_oldest(),
                (_, Some(_)) => self.http2.abandon_oldest(),
                _ => self.http2.abandon_oldest(),
            };
            if !removed {
                break;
            }
        }
        let metrics = self.connection_metrics();
        metrics::gauge!("agentsight_http_conn_active").set(metrics.pending_connection_count as f64);
        metrics::gauge!("agentsight_http_conn_retained_bytes")
            .set(metrics.connection_cache_bytes as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;
    use crate::parser::http2::{Http2FrameType, ParsedHttp2Frame};
    use crate::probes::sslsniff::SslEvent;
    use std::rc::Rc;

    fn ssl_event(pid: u32, ssl_ptr: u64, buf: &[u8], rw: i32) -> SslEvent {
        SslEvent {
            source: 0,
            timestamp_ns: 0,
            delta_ns: 0,
            pid,
            tid: 1,
            uid: 0,
            len: buf.len() as u32,
            rw,
            comm: String::new(),
            buf: buf.to_vec(),
            is_handshake: false,
            ssl_ptr,
        }
    }

    /// End-to-end regression for the write-direction chunked terminator: a
    /// chunked request whose final write is `0\r\n\r\n` must complete its
    /// body. The parser used to cut the terminator from that write and emit a
    /// synthetic SSE done event, which the aggregator silently dropped while
    /// in RequestBodyPending — so the terminator never reached body_buffer and
    /// `chunked_stream_complete` never fired.
    #[test]
    fn test_chunked_request_final_terminator_write_completes_body() {
        let parser = Parser::new();
        let mut aggregator = Aggregator::new();
        let conn = ConnectionId {
            pid: 4242,
            ssl_ptr: 0x4321,
        };

        // rw == 1 is the write direction. The request head completes, but its
        // chunked body still lacks the zero-size terminating chunk, so the
        // connection stays in RequestBodyPending.
        let headers = b"POST /v1/messages HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        aggregator.process_result(parser.parse_ssl_event(Rc::new(ssl_event(
            conn.pid,
            conn.ssl_ptr,
            headers,
            1,
        ))));

        let chunk = b"7\r\n{\"x\":1}\r\n";
        aggregator.process_result(parser.parse_ssl_event(Rc::new(ssl_event(
            conn.pid,
            conn.ssl_ptr,
            chunk,
            1,
        ))));

        // The final write carries only the zero-size terminating chunk.
        aggregator.process_result(parser.parse_ssl_event(Rc::new(ssl_event(
            conn.pid,
            conn.ssl_ptr,
            b"0\r\n\r\n",
            1,
        ))));

        assert!(
            aggregator.http().has_pending_request(&conn),
            "the terminator write must complete the chunked request body"
        );
    }

    fn test_event(pid: u32, ssl_ptr: u64, rw: i32, timestamp_ns: u64) -> Rc<SslEvent> {
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

    fn test_frame(
        stream_id: u32,
        frame_type: u8,
        flags: u8,
        payload: Vec<u8>,
        source: Rc<SslEvent>,
    ) -> ParsedHttp2Frame {
        let payload_len = payload.len();
        let mut buf = Vec::with_capacity(9 + payload_len);
        buf.push(((payload_len >> 16) & 0xFF) as u8);
        buf.push(((payload_len >> 8) & 0xFF) as u8);
        buf.push((payload_len & 0xFF) as u8);
        buf.push(frame_type);
        buf.push(flags);
        buf.push(((stream_id >> 24) & 0x7F) as u8);
        buf.push(((stream_id >> 16) & 0xFF) as u8);
        buf.push(((stream_id >> 8) & 0xFF) as u8);
        buf.push((stream_id & 0xFF) as u8);
        buf.extend_from_slice(&payload);

        let source_event = Rc::new(SslEvent {
            source: source.source,
            timestamp_ns: source.timestamp_ns,
            delta_ns: source.delta_ns,
            pid: source.pid,
            tid: source.tid,
            uid: source.uid,
            len: buf.len() as u32,
            rw: source.rw,
            comm: source.comm.clone(),
            buf,
            is_handshake: source.is_handshake,
            ssl_ptr: source.ssl_ptr,
        });

        ParsedHttp2Frame {
            frame_type: Http2FrameType::from_u8(frame_type),
            flags,
            stream_id,
            payload_offset: 9,
            payload_len,
            source_event,
        }
    }

    fn feed_http2(
        aggregator: &mut Aggregator,
        frames: Vec<ParsedHttp2Frame>,
    ) -> Vec<AggregatedResult> {
        aggregator.process_result(ParseResult {
            messages: vec![ParsedMessage::Http2Frames(frames)],
        })
    }

    #[test]
    fn http2_honors_max_connection_body_bytes() {
        // Regression: the HTTP/2 aggregator was built without the configured
        // connection limits, so DATA frames were buffered without any size
        // check and a stream that never sees END_STREAM grew forever.
        const LIMIT: usize = 4096;
        let limits = RuntimeLimits {
            max_connection_body_bytes: LIMIT,
            ..Default::default()
        };
        let mut aggregator = Aggregator::with_limits(4, &limits);

        // Request HEADERS without END_STREAM, followed by a body that never ends.
        let mut frames = vec![test_frame(
            1,
            1,
            0x04,
            b":method: POST\n:path: /v1/chat".to_vec(),
            test_event(7, 0xABC, 1, 1000),
        )];
        for i in 0..16 {
            frames.push(test_frame(
                1,
                0,
                0x00,
                vec![b'x'; 1024],
                test_event(7, 0xABC, 1, 2000 + i),
            ));
        }
        let results = feed_http2(&mut aggregator, frames);
        assert!(results.iter().any(|result| matches!(
            result,
            AggregatedResult::Abandoned {
                reason: super::super::RemoveReason::OverBudget,
                ..
            }
        )));

        let metrics = aggregator.connection_metrics();
        assert!(
            metrics.pending_connection_bytes <= LIMIT,
            "retained http/2 body must respect max_connection_body_bytes: {} > {}",
            metrics.pending_connection_bytes,
            LIMIT
        );
    }

    #[test]
    fn http2_hard_timeout_emits_partial_stream_without_traffic() {
        // Regression: there was no idle sweep for HTTP/2, so a stream that
        // stopped making progress stayed in memory for the process lifetime.
        let limits = RuntimeLimits {
            connection_hard_timeout_secs: 1,
            ..Default::default()
        };
        let mut aggregator = Aggregator::with_limits(4, &limits);

        feed_http2(
            &mut aggregator,
            vec![test_frame(
                1,
                0,
                0x00,
                b"partial".to_vec(),
                test_event(8, 0xDEF, 1, 1),
            )],
        );
        assert_eq!(aggregator.connection_metrics().pending_connection_count, 1);

        let results = aggregator.tick(Instant::now() + Duration::from_secs(2));
        assert!(matches!(
            results.as_slice(),
            [AggregatedResult::Abandoned {
                reason: super::super::RemoveReason::HardTimeout,
                ..
            }]
        ));

        assert_eq!(
            aggregator.connection_metrics().pending_connection_count,
            0,
            "an idle http/2 stream must be dropped by the periodic sweep"
        );
    }

    #[test]
    fn hard_timeout_resumption_is_response_only_and_preserves_the_new_stream() {
        let limits = RuntimeLimits {
            connection_hard_timeout_secs: 1,
            ..Default::default()
        };
        let mut aggregator = Aggregator::with_limits(4, &limits);
        let parser = Parser::new();
        for (rw, bytes) in [
            (
                1,
                b"POST /v1/chat/completions HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}".as_slice(),
            ),
            (
                0,
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n".as_slice(),
            ),
        ] {
            assert!(
                aggregator
                    .process_result(
                        parser.parse_event(crate::event::Event::Ssl(ssl_event(1, 1, bytes, rw)))
                    )
                    .is_empty()
            );
        }
        let expired = aggregator.tick(Instant::now() + Duration::from_secs(2));
        assert!(matches!(
            expired.as_slice(),
            [AggregatedResult::Abandoned {
                reason: super::super::RemoveReason::HardTimeout,
                ..
            }]
        ));
        let resumed =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n\ndata: [DONE]\n\n";
        let results = aggregator.process_result(
            parser.parse_event(crate::event::Event::Ssl(ssl_event(1, 1, resumed, 0))),
        );
        let [AggregatedResult::ResponseOnly { response, .. }] = results.as_slice() else {
            panic!("late response must survive");
        };
        assert_eq!(
            response.json_body()[0]["choices"][0]["delta"]["content"],
            "late"
        );
        assert!(!aggregator.has_pending());
    }

    #[test]
    fn http1_and_http2_share_one_byte_budget() {
        let limits = RuntimeLimits {
            connection_max_bytes: 5000,
            ..Default::default()
        };
        let mut aggregator = Aggregator::with_limits(8, &limits);
        let parser = Parser::new();
        let mut request =
            b"POST /v1/chat/completions HTTP/1.1\r\nContent-Length: 3000\r\n\r\n".to_vec();
        request.extend_from_slice(&vec![b'x'; 3000]);
        assert!(
            aggregator
                .process_result(
                    parser.parse_event(crate::event::Event::Ssl(ssl_event(1, 1, &request, 1)))
                )
                .is_empty()
        );
        let results = feed_http2(
            &mut aggregator,
            vec![test_frame(
                1,
                0,
                0,
                vec![b'x'; 3000],
                test_event(1, 2, 1, 1),
            )],
        );
        assert!(matches!(
            results.as_slice(),
            [AggregatedResult::Abandoned {
                reason: super::super::RemoveReason::OverBudget,
                ..
            }]
        ));
        assert!(
            aggregator.connection_metrics().connection_cache_bytes <= limits.connection_max_bytes
        );
        assert_eq!(aggregator.connection_metrics().pending_connection_count, 1);
        assert_eq!(aggregator.http().active_connections(), 0);
    }

    #[test]
    fn http2_pid_exit_preserves_partial_evidence_in_every_pending_phase() {
        for phase in 0..3 {
            let mut aggregator = Aggregator::with_limits(8, &RuntimeLimits::default());
            let mut encoder = hpack::Encoder::new();
            let headers = encoder.encode([
                (b":method".as_slice(), b"POST".as_slice()),
                (b":path".as_slice(), b"/v1/chat".as_slice()),
            ]);
            let mut frames = vec![
                test_frame(1, 1, 4, headers.clone(), test_event(41, 1, 1, 100)),
                test_frame(
                    1,
                    0,
                    u8::from(phase != 0),
                    b"request".to_vec(),
                    test_event(41, 1, 1, 200),
                ),
                test_frame(1, 1, 5, headers, test_event(42, 2, 1, 100)),
            ];
            if phase == 2 {
                let response = hpack::Encoder::new().encode([
                    (b":status".as_slice(), b"200".as_slice()),
                    (b"content-type".as_slice(), b"application/json".as_slice()),
                ]);
                frames.push(test_frame(1, 1, 4, response, test_event(41, 1, 0, 300)));
                frames.push(test_frame(
                    1,
                    0,
                    0,
                    b"partial response".to_vec(),
                    test_event(41, 1, 0, 400),
                ));
            }
            assert!(feed_http2(&mut aggregator, frames).is_empty());
            assert!(aggregator.drain_connections_for_pid(41).is_empty());
            let results = feed_http2(&mut aggregator, Vec::new());
            let [AggregatedResult::Abandoned { reason, partial }] = results.as_slice() else {
                panic!("PID exit must emit one partial stream in phase {phase}");
            };
            assert_eq!(*reason, super::super::RemoveReason::PidExited);
            let AggregatedResult::Http2StreamComplete(stream) = partial.as_ref() else {
                panic!("PID exit must preserve HTTP/2 evidence");
            };
            assert_eq!(stream.stream_id.connection_id.pid, 41);
            assert_eq!(stream.request_body(), b"request");
            assert_eq!(stream.request_complete, phase != 0);
            assert!(!stream.response_complete);
            assert_eq!(stream.start_timestamp_ns, 100);
            assert_eq!(stream.end_timestamp_ns, if phase == 2 { 400 } else { 200 });
            assert_eq!(
                stream.response_body(),
                if phase == 2 {
                    b"partial response".as_slice()
                } else {
                    b""
                },
            );
            assert_eq!(aggregator.connection_metrics().pending_connection_count, 1);
            aggregator.drain_connections_for_pid(41);
            assert!(feed_http2(&mut aggregator, Vec::new()).is_empty());
            aggregator.drain_connections_for_pid(42);
            assert_eq!(feed_http2(&mut aggregator, Vec::new()).len(), 1);
            assert!(!aggregator.has_pending());
            assert_eq!(aggregator.connection_metrics().connection_cache_bytes, 0);
        }
    }

    #[test]
    fn http2_dead_pid_releases_hpack_after_a_completed_stream() {
        let mut aggregator = Aggregator::new();
        let pid = u32::MAX;
        assert!(!crate::utils::procfs::proc_pid(pid).exists());
        let request = hpack::Encoder::new().encode([
            (b":method".as_slice(), b"GET".as_slice()),
            (b":path".as_slice(), b"/fixture".as_slice()),
        ]);
        let response = hpack::Encoder::new().encode([
            (b":status".as_slice(), b"200".as_slice()),
            (b"x-fixture".as_slice(), b"retained header".as_slice()),
        ]);
        let results = feed_http2(
            &mut aggregator,
            vec![
                test_frame(1, 1, 5, request, test_event(pid, 1, 1, 100)),
                test_frame(1, 1, 5, response, test_event(pid, 1, 0, 200)),
            ],
        );
        assert!(matches!(
            results.as_slice(),
            [AggregatedResult::Http2StreamComplete(_)]
        ));
        assert!(!aggregator.has_pending());
        assert!(aggregator.connection_metrics().connection_cache_bytes > 0);
        assert!(aggregator.drain_dead_pid_connections().is_empty());
        assert_eq!(aggregator.connection_metrics().connection_cache_bytes, 0);
        assert!(feed_http2(&mut aggregator, Vec::new()).is_empty());
    }

    #[test]
    fn http2_early_sse_completion_decodes_fragmented_trailers_for_the_next_stream() {
        let mut aggregator = Aggregator::new();
        let mut requests = hpack::Encoder::new();
        let mut responses = hpack::Encoder::new();
        let request = requests.encode([
            (b":method".as_slice(), b"GET".as_slice()),
            (b":path".as_slice(), b"/fixture".as_slice()),
        ]);
        let response = responses.encode([
            (b":status".as_slice(), b"200".as_slice()),
            (b"content-type".as_slice(), b"text/event-stream".as_slice()),
        ]);
        let completed = feed_http2(
            &mut aggregator,
            vec![
                test_frame(1, 1, 5, request, test_event(41, 1, 1, 100)),
                test_frame(1, 1, 4, response, test_event(41, 1, 0, 200)),
                test_frame(
                    1,
                    0,
                    0,
                    b"data: [DONE]\n\n".to_vec(),
                    test_event(41, 1, 0, 300),
                ),
            ],
        );
        assert!(matches!(
            completed.as_slice(),
            [AggregatedResult::Http2StreamComplete(_)]
        ));
        let trailers = responses.encode([(b"x-fixture".as_slice(), b"trailer value".as_slice())]);
        assert!(
            feed_http2(
                &mut aggregator,
                vec![
                    test_frame(1, 1, 1, trailers[..1].to_vec(), test_event(41, 1, 0, 400)),
                    test_frame(1, 9, 4, trailers[1..].to_vec(), test_event(41, 1, 0, 500)),
                ]
            )
            .is_empty()
        );
        let request = requests.encode([
            (b":method".as_slice(), b"GET".as_slice()),
            (b":path".as_slice(), b"/fixture".as_slice()),
        ]);
        let response = responses.encode([
            (b":status".as_slice(), b"200".as_slice()),
            (b"x-fixture".as_slice(), b"trailer value".as_slice()),
        ]);
        let completed = feed_http2(
            &mut aggregator,
            vec![
                test_frame(3, 1, 5, request, test_event(41, 1, 1, 600)),
                test_frame(3, 1, 5, response, test_event(41, 1, 0, 700)),
            ],
        );
        let [AggregatedResult::Http2StreamComplete(stream)] = completed.as_slice() else {
            panic!("the next stream must complete without replaying the previous SSE");
        };
        assert_eq!(stream.stream_id.stream_id, 3);
        assert!(
            stream
                .decoded_response_headers
                .as_ref()
                .unwrap()
                .iter()
                .any(|(key, value)| key == "x-fixture" && value == "trailer value")
        );
        assert!(!aggregator.has_pending());
    }

    #[test]
    fn orphan_chunk_terminator_does_not_reopen_a_completed_sse_connection() {
        for retain_raw_sse in [false, true] {
            let mut aggregator = Aggregator::with_limits(
                4,
                &RuntimeLimits {
                    retain_raw_sse,
                    ..Default::default()
                },
            );
            let parser = Parser::new();
            let mut feed = |rw, bytes: &[u8]| {
                aggregator.process_result(
                    parser.parse_event(crate::event::Event::Ssl(ssl_event(7, 1, bytes, rw))),
                )
            };
            let request = b"POST /v1/chat/completions HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}";
            assert!(feed(1, request).is_empty());
            assert!(feed(0, b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").is_empty());
            let body =
                "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n";
            let chunk = format!("{:x}\r\n{body}\r\n", body.len());
            assert_eq!(feed(0, chunk.as_bytes()).len(), 1);
            assert!(feed(0, b"0\r\n\r\n").is_empty());
            assert!(feed(1, request).is_empty());
            assert!(matches!(
                feed(0, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").as_slice(),
                [AggregatedResult::HttpComplete(_)]
            ));
            assert!(!aggregator.has_pending());
            assert!(
                aggregator
                    .tick(Instant::now() + Duration::from_secs(1000))
                    .is_empty()
            );
        }
    }

    fn start_http2_sse(aggregator: &mut Aggregator, stream: u32) {
        let request = hpack::Encoder::new().encode([
            (b":method".as_slice(), b"POST".as_slice()),
            (b":path".as_slice(), b"/v1/chat/completions".as_slice()),
        ]);
        let response = hpack::Encoder::new().encode([
            (b":status".as_slice(), b"200".as_slice()),
            (b"content-type".as_slice(), b"text/event-stream".as_slice()),
        ]);
        assert!(
            feed_http2(
                aggregator,
                vec![
                    test_frame(stream, 1, 5, request, test_event(7, 1, 1, 100)),
                    test_frame(stream, 1, 4, response, test_event(7, 1, 0, 200)),
                ]
            )
            .is_empty()
        );
    }

    #[test]
    fn byte_pressure_evicts_idle_http2_sse_before_a_stream_still_producing_data() {
        let mut aggregator = Aggregator::new();
        let delta = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"}}]}\n\n";
        for stream in [1, 3] {
            start_http2_sse(&mut aggregator, stream);
            assert!(
                feed_http2(
                    &mut aggregator,
                    vec![test_frame(
                        stream,
                        0,
                        0,
                        delta.to_vec(),
                        test_event(7, 1, 0, 300),
                    )]
                )
                .is_empty()
            );
        }
        let budget = aggregator.connection_metrics().connection_cache_bytes;
        aggregator.bytes_budget = budget;
        aggregator.http2.configure_retention(&RuntimeLimits {
            connection_max_bytes: budget,
            ..Default::default()
        });
        let results = feed_http2(
            &mut aggregator,
            vec![test_frame(
                1,
                0,
                0,
                format!(
                    "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
                    "x".repeat(32)
                )
                .into_bytes(),
                test_event(7, 1, 0, 400),
            )],
        );
        let [AggregatedResult::Abandoned { reason, partial }] = results.as_slice() else {
            panic!("byte pressure must report the idle stream")
        };
        assert_eq!(*reason, super::super::RemoveReason::OverBudget);
        let AggregatedResult::Http2StreamComplete(stream) = partial.as_ref() else {
            panic!("expected HTTP/2 evidence")
        };
        assert_eq!(stream.stream_id.stream_id, 3);
        assert_eq!(aggregator.connection_metrics().pending_connection_count, 1);
        assert!(aggregator.connection_metrics().connection_cache_bytes <= budget);
    }

    #[test]
    fn http2_multiline_sse_survives_both_completion_and_abandonment() {
        for complete in [false, true] {
            let mut aggregator = Aggregator::new();
            start_http2_sse(&mut aggregator, 1);
            let mut body = b"data: {\"id\":\"multiline\",\"model\":\"fixture\",\ndata: \"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}],\ndata: \"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15}}".to_vec();
            if complete {
                body.extend_from_slice(b"\n\ndata: [DONE]\n\n");
            }
            let mut results = feed_http2(
                &mut aggregator,
                vec![test_frame(1, 0, 0, body, test_event(7, 1, 0, 900))],
            );
            if !complete {
                assert!(results.is_empty());
                results = aggregator.drain_pending();
            }
            assert_eq!(results.len(), 1);
            let result = match &results[0] {
                AggregatedResult::Abandoned { reason, partial } => {
                    assert!(!complete);
                    assert_eq!(*reason, super::super::RemoveReason::Shutdown);
                    partial.as_ref()
                }
                result => {
                    assert!(complete);
                    result
                }
            };
            let AggregatedResult::Http2StreamComplete(stream) = result else {
                panic!("expected captured multiline SSE")
            };
            let json = stream.response_sse_json_array().unwrap();
            assert_eq!(json[0]["choices"][0]["delta"]["content"], "hello");
            assert_eq!(json[0]["usage"]["total_tokens"], 15);
            assert_eq!(stream.response_complete, complete);
            let analysis = crate::analyzer::Analyzer::new().analyze_aggregated(&results[0]);
            let token = analysis
                .iter()
                .find_map(|result| match result {
                    crate::analyzer::AnalysisResult::Token(token) => Some(token),
                    _ => None,
                })
                .unwrap();
            assert_eq!(token.output_tokens, 3);
            assert!(!aggregator.has_pending());
        }
    }
}
