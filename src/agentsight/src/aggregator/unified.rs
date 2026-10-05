//! Unified Aggregator - high-level entry point for event aggregation
//!
//! This module provides a unified interface for aggregating parsed messages.
//! It combines HTTP Connection Aggregator and Process Event Aggregator.

use super::http::{ConnectionId, ConnectionMetrics, ConnectionState, HttpConnectionAggregator};
use super::http2::Http2StreamAggregator;
use super::proctrace::ProcessEventAggregator;
use super::result::AggregatedResult;
use crate::chrome_trace::export_trace_events;
use crate::config::{DEFAULT_CONNECTION_CAPACITY, RuntimeLimits};
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
        Aggregator {
            http: HttpConnectionAggregator::with_limits(
                connection_capacity,
                limits.max_connection_body_bytes,
                idle_timeout,
            ),
            // HTTP/2 multiplexes many streams per connection; keep the stream
            // LRU proportional to the connection capacity, like the default
            // constructor, but enforce the configured byte and idle limits.
            http2: Http2StreamAggregator::with_limits(
                connection_capacity.saturating_mul(4),
                limits.max_connection_body_bytes,
                idle_timeout,
            ),
            process: ProcessEventAggregator::new(),
            last_eviction: Instant::now(),
            eviction_period: idle_timeout.min(Duration::from_secs(10)),
        }
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
        if let Some(event) = response_event.filter(|event| self.http.accepts_response_bytes(event))
        {
            let results: Vec<_> = self.http.process_raw_body_data(event).into_iter().collect();
            for result in &results {
                export_trace_events(result);
            }
            return results;
        }

        let results: Vec<AggregatedResult> = result
            .messages
            .into_iter()
            .flat_map(|msg| self.process_message(msg))
            .collect();

        // Export chrome trace if enabled
        for r in &results {
            export_trace_events(r);
        }

        timer.record_outputs(results.len());
        results
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
    pub(crate) fn drain_connections_for_pid(
        &mut self,
        pid: u32,
    ) -> Vec<(ConnectionId, ConnectionState)> {
        self.http.drain_connections_for_pid(pid)
    }

    /// Drain connections whose PID is no longer alive.
    ///
    /// Delegates to the HTTP aggregator's dead-PID drain.
    pub(crate) fn drain_dead_pid_connections(&mut self) -> Vec<(ConnectionId, ConnectionState)> {
        self.http.drain_dead_pid_connections()
    }

    /// Snapshot in-flight HTTP connections that exceeded the idle timeout.
    ///
    /// Used to persist evidence for manually interrupted streams where the
    /// agent process remains alive, so dead-PID draining would never run.
    pub(crate) fn snapshot_idle_connections(&mut self) -> Vec<(ConnectionId, ConnectionState)> {
        self.http.snapshot_idle_connections()
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

    fn feed_http2(aggregator: &mut Aggregator, frames: Vec<ParsedHttp2Frame>) {
        let results = aggregator.process_result(ParseResult {
            messages: vec![ParsedMessage::Http2Frames(frames)],
        });
        assert!(results.is_empty(), "no stream ended in this test");
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
        feed_http2(&mut aggregator, frames);

        let metrics = aggregator.connection_metrics();
        assert!(
            metrics.pending_connection_bytes <= LIMIT,
            "retained http/2 body must respect max_connection_body_bytes: {} > {}",
            metrics.pending_connection_bytes,
            LIMIT
        );
    }

    #[test]
    fn http2_idle_stream_is_swept_by_periodic_eviction() {
        // Regression: there was no idle sweep for HTTP/2, so a stream that
        // stopped making progress stayed in memory for the process lifetime.
        let limits = RuntimeLimits {
            connection_idle_timeout_secs: 0,
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

        std::thread::sleep(Duration::from_millis(20));
        // The next parse result runs the periodic eviction tick (period 0 here).
        let results = aggregator.process_result(ParseResult {
            messages: Vec::new(),
        });
        assert!(results.is_empty());

        assert_eq!(
            aggregator.connection_metrics().pending_connection_count,
            0,
            "an idle http/2 stream must be dropped by the periodic sweep"
        );
    }
}
