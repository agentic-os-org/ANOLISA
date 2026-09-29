//! Value types returned by security-event repository queries.

use crate::event::SecurityEvent;

/// Security event row plus the original epoch used for correlation sorting.
#[derive(Debug, Clone, PartialEq)]
pub struct CorrelationCandidate {
    /// The reconstructed event.
    pub event: SecurityEvent,
    /// Stored `timestamp_epoch`, kept so callers can sort without re-parsing.
    pub timestamp_epoch: f64,
}

/// Dashboard summary data returned by one repository call.
///
/// The `by_*` maps use ordered keys so that summary output is reproducible
/// across runs; v1 relies on SQL `GROUP BY` ordering for the same effect.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SecurityEventsSummary {
    /// Total matching events.
    pub total: u64,
    /// Counts grouped by `category`.
    pub by_category: Vec<(Option<String>, u64)>,
    /// Counts grouped by `event_type`.
    pub by_event_type: Vec<(Option<String>, u64)>,
    /// Counts grouped by `result`.
    pub by_result: Vec<(Option<String>, u64)>,
    /// Counts grouped by `session_id`.
    pub by_session: Vec<(Option<String>, u64)>,
    /// Counts grouped by `run_id`.
    pub by_run: Vec<(Option<String>, u64)>,
    /// Most recent events, newest first.
    pub latest_events: Vec<SecurityEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_summary_is_all_zero() {
        let summary = SecurityEventsSummary::default();
        assert_eq!(summary.total, 0);
        assert!(summary.by_category.is_empty());
        assert!(summary.by_event_type.is_empty());
        assert!(summary.by_result.is_empty());
        assert!(summary.by_session.is_empty());
        assert!(summary.by_run.is_empty());
        assert!(summary.latest_events.is_empty());
    }
}
