//! Aggregated Result types for event aggregation
//!
//! This module defines the `AggregatedResult` enum which represents
//! the output of aggregating parsed messages from various sources.

use super::http::{AggregatedResponse, ConnectionId, HttpPair, ParsedRequest};
use super::http2::Http2Stream;
use super::proctrace::AggregatedProcess;
use crate::chrome_trace::{ChromeTraceEvent, ToChromeTraceEvent};
use crate::parser::http2::ParsedHttp2Frame;

/// Why correlation ended before protocol completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveReason {
    /// A newer connection displaced the least recently used entry.
    LruEvicted,
    /// Capacity was exhausted by recently active streams, so admission was refused.
    CapacityRejected,
    /// A new request reused a connection with an unfinished call.
    Replaced,
    /// An empty connection exceeded the soft inactivity timeout.
    IdleTimeout,
    /// No traffic arrived within the hard inactivity timeout.
    HardTimeout,
    /// Retained payload exceeded a byte limit.
    OverBudget,
    /// The owning process exited.
    PidExited,
    /// Invalid framing or content encoding prevented further decoding.
    ProtocolError,
    /// Capture stopped while correlation was still in flight.
    Shutdown,
}

impl RemoveReason {
    /// Stable value used by metadata and metric labels.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LruEvicted => "lru_evicted",
            Self::CapacityRejected => "capacity_rejected",
            Self::Replaced => "replaced",
            Self::IdleTimeout => "idle_timeout",
            Self::HardTimeout => "hard_timeout",
            Self::OverBudget => "over_budget",
            Self::PidExited => "pid_exited",
            Self::ProtocolError => "protocol_error",
            Self::Shutdown => "shutdown",
        }
    }

    /// Resource pressure reports bounded metadata instead of expanding partial payloads.
    pub(crate) fn metadata_only(self) -> bool {
        matches!(
            self,
            Self::LruEvicted | Self::CapacityRejected | Self::OverBudget
        )
    }
}

/// Capacity eviction must spare in-flight connections with traffic inside this window.
pub(crate) const ACTIVE_CONNECTION_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Bound each diagnostic field without splitting UTF-8 characters.
pub(crate) fn metadata_text(value: &str) -> String {
    let mut end = value.len().min(2048);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Keep only routing and correlation headers on resource-pressure diagnostics.
pub(crate) fn metadata_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "x-request-id"
            | "content-type"
            | ":method"
            | ":path"
            | ":authority"
            | ":scheme"
            | ":status"
    )
}

/// Aggregated result from any aggregator
#[derive(Debug, Clone)]
pub enum AggregatedResult {
    /// Captured evidence from a call that ended before protocol completion.
    Abandoned {
        /// Lifecycle or resource limit that ended correlation.
        reason: RemoveReason,
        /// Owned partial evidence; resource removals retain only bounded metadata.
        partial: Box<AggregatedResult>,
    },
    /// HTTP request/response pair complete
    HttpComplete(HttpPair),
    /// SSE stream complete (received [DONE])
    SseComplete(HttpPair),
    /// Process lifecycle complete
    ProcessComplete(AggregatedProcess),
    /// Standalone request (no matching response yet)
    RequestOnly {
        connection_id: ConnectionId,
        request: ParsedRequest,
    },
    /// Standalone response (no matching request)
    ResponseOnly {
        connection_id: ConnectionId,
        response: AggregatedResponse,
    },
    /// HTTP/2 frames (pass-through, no stream reassembly)
    Http2Frames {
        connection_id: ConnectionId,
        frames: Vec<ParsedHttp2Frame>,
    },
    /// HTTP/2 stream complete (request/response aggregated by stream_id)
    Http2StreamComplete(Http2Stream),
}

impl AggregatedResult {
    /// Identify the owner before delayed analysis, including abandoned evidence.
    pub(crate) fn pid(&self) -> u32 {
        match self {
            Self::Abandoned { partial, .. } => partial.pid(),
            Self::HttpComplete(pair) | Self::SseComplete(pair) => pair.connection_id.pid,
            Self::ProcessComplete(process) => process.pid,
            Self::RequestOnly { connection_id, .. }
            | Self::ResponseOnly { connection_id, .. }
            | Self::Http2Frames { connection_id, .. } => connection_id.pid,
            Self::Http2StreamComplete(stream) => stream.stream_id.connection_id.pid,
        }
    }
    /// Get the result type as a string
    pub fn result_type(&self) -> &'static str {
        match self {
            Self::Abandoned { .. } => "abandoned",
            AggregatedResult::HttpComplete(_) => "http_complete",
            AggregatedResult::SseComplete(_) => "sse_complete",
            AggregatedResult::ProcessComplete(_) => "process_complete",
            AggregatedResult::RequestOnly { .. } => "request_only",
            AggregatedResult::ResponseOnly { .. } => "response_only",
            AggregatedResult::Http2Frames { .. } => "http2_frames",
            AggregatedResult::Http2StreamComplete(_) => "http2_stream_complete",
        }
    }
}

impl ToChromeTraceEvent for AggregatedResult {
    fn to_chrome_trace_events(&self) -> Vec<ChromeTraceEvent> {
        match self {
            Self::Abandoned { partial, .. } => partial.to_chrome_trace_events(),
            AggregatedResult::HttpComplete(pair) => pair.to_chrome_trace_events(),
            AggregatedResult::SseComplete(pair) => pair.to_chrome_trace_events(),
            AggregatedResult::ProcessComplete(process) => process.to_chrome_trace_events(),
            AggregatedResult::RequestOnly { .. } => {
                log::warn!("RequestOnly: {self:?}");
                vec![]
            }
            AggregatedResult::ResponseOnly { .. } => {
                log::warn!("ResponseOnly: {self:?}");
                vec![]
            }
            AggregatedResult::Http2Frames { frames, .. } => frames
                .iter()
                .flat_map(|f| f.to_chrome_trace_events())
                .collect(),
            AggregatedResult::Http2StreamComplete(stream) => stream.to_chrome_trace_events(),
        }
    }
}
