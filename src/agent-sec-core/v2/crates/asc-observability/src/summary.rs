//! Aggregate value types returned by observability repository reads.
//!
//! Migrated from v1 `observability/repositories.py` lines 17-36.

/// Preview length applied to `user_input` in [`RunSummary`].
///
/// Mirrors v1 `_USER_INPUT_PREVIEW_LIMIT`.
pub const USER_INPUT_PREVIEW_LIMIT: usize = 80;

/// Aggregated stats for one session, used by the review session list.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    /// Session identifier.
    pub session_id: String,
    /// Epoch of the earliest record in the session.
    pub first_seen_epoch: f64,
    /// Epoch of the latest record in the session.
    pub last_seen_epoch: f64,
    /// Number of distinct runs (user turns).
    pub turn_count: u64,
    /// Number of records in the session.
    pub event_count: u64,
}

/// Aggregated stats for one run (one user turn) inside a session.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSummary {
    /// Run identifier.
    pub run_id: String,
    /// Epoch of the first record in the run.
    pub started_at_epoch: f64,
    /// Epoch of the last record in the run.
    pub ended_at_epoch: f64,
    /// Truncated preview of the run's user input, when one is recoverable.
    pub user_input_preview: Option<String>,
    /// Number of records in the run.
    pub event_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_limit_matches_v1() {
        assert_eq!(USER_INPUT_PREVIEW_LIMIT, 80);
    }

    #[test]
    fn summaries_are_plain_value_types() {
        let run = RunSummary {
            run_id: "r".to_owned(),
            started_at_epoch: 1.0,
            ended_at_epoch: 2.0,
            user_input_preview: None,
            event_count: 3,
        };
        assert_eq!(run.clone(), run);
    }
}
