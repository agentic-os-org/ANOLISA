//! HTTP aggregation module
//
//! Provides aggregators for HTTP requests/responses.

mod aggregator;
mod chunk_stream;
mod pair;
mod response;
mod sse_prefix;
mod sse_reduce;
mod sse_stream;
pub(crate) use sse_stream::SseStream;

// Re-export main types
pub use aggregator::{ConnectionId, HttpConnectionAggregator};
// Crate-internal: connection state machine (see review F3 note on the enum).
pub(crate) use aggregator::ConnectionMetrics;
pub(crate) use aggregator::ConnectionState;
pub(crate) use aggregator::lifecycle::RemovedConnection;
pub use pair::HttpPair;
pub use response::AggregatedResponse;
pub(crate) use response::event_has_meaningful_output;

// Re-export ParsedRequest from parser (replaces AggregatedRequest)
pub use crate::parser::http::ParsedRequest;
