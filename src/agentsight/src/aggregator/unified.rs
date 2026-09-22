//! Unified Aggregator - high-level entry point for event aggregation
//!
//! This module provides a unified interface for aggregating parsed messages.
//! It combines HTTP Connection Aggregator and Process Event Aggregator.

use super::http::{ConnectionId, ConnectionState, HttpConnectionAggregator};
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
            http2: Http2StreamAggregator::new(),
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
