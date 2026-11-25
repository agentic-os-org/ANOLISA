//! SQLite storage submodules
//!
//! Provides unified SQLite-based persistence for all record types:
//! - `audit`: Audit event persistence
//! - `token`: Token usage persistence and querying
//! - `token_consumption`: TokenConsumptionBreakdown persistence and querying
//! - `http`: HTTP request/response persistence

pub mod audit;
pub mod genai;
pub mod http;
pub mod interruption;
pub mod token;
pub mod token_consumption;
pub mod tokenless;

// Re-export audit storage
pub use audit::{AuditStore, SqliteStore};

// Re-export token storage
pub use token::{
    TimePeriod, TokenBreakdown, TokenComparison, TokenQuery, TokenQueryResult, TokenStore, Trend,
    format_tokens, format_tokens_with_commas,
};

// Re-export token consumption storage
pub use token_consumption::{
    TokenConsumptionFilter, TokenConsumptionQueryResult, TokenConsumptionRecord,
    TokenConsumptionStore,
};

// Re-export HTTP storage
pub use http::HttpStore;

// Re-export GenAI SQLite storage
pub use genai::{
    GenAISqliteStore, PendingCallInfo, PendingOrigin, ResourceSample, SessionPhase,
    SessionResourceTimeline, SseEnrichment,
};

// Re-export Interruption SQLite storage
pub use interruption::{
    InterruptionRecord, InterruptionStore, InterruptionTypeStat, UNASSIGNED_CONVERSATION_ID,
    UNASSIGNED_SESSION_ID,
};

// Re-export tokenless stats storage
pub use tokenless::{TokenlessStatsStore, TokenlessWindowSummary};

/// Clamp an unsigned nanosecond cutoff to the maximum signed 64-bit integer SQLite binds.
///
/// Cutoffs above `i64::MAX` preserve monotonic "purge everything older" semantics rather than
/// wrapping negative.
pub(super) fn clamp_cutoff_ns(cutoff_ns: u64) -> i64 {
    cutoff_ns.min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::clamp_cutoff_ns;

    #[test]
    fn clamp_cutoff_ns_preserves_representable_and_clamps_overflow() {
        assert_eq!(clamp_cutoff_ns(0), 0);
        assert_eq!(clamp_cutoff_ns(1_000_000), 1_000_000);
        assert_eq!(clamp_cutoff_ns(i64::MAX as u64), i64::MAX);
        assert_eq!(clamp_cutoff_ns((i64::MAX as u64) + 1), i64::MAX);
        assert_eq!(clamp_cutoff_ns(u64::MAX), i64::MAX);
    }
}
