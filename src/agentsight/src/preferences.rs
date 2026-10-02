//! Rule-based user preference mining from captured agent trajectories.
//!
//! Analysis is on-demand and table-free: the `/api/preferences` handlers
//! read a time window of turns from one of two sources — `genai_events.db`
//! (Linux eBPF pipeline, [`genai_source`]) or `trajectories.db` (trajectory
//! collector, [`trajectory_source`], the only source on macOS). Both
//! providers flatten their storage format into the same
//! [`detector::PreferenceEventRow`] shape, so the pure keyword / statistical
//! rules ([`detector`]) and the signal folding ([`aggregator`]) never learn
//! where a turn came from. Nothing is persisted — the server keeps a
//! short-lived in-process cache only ([`api`]). An optional LLM layer lives
//! in the `agentsight-opt` crate and merges its findings at the API level.

pub mod aggregator;
#[cfg(feature = "server")]
pub mod api;
pub mod detector;
#[cfg(target_os = "linux")]
pub mod genai_source;
pub mod signals;
pub mod trajectory_source;

use detector::PreferenceEventRow;
use signals::Preference;

/// Default analysis window when the request omits `window_days`.
pub const DEFAULT_WINDOW_DAYS: u32 = 7;
/// Hard cap for `window_days` — wider windows only add stale evidence.
pub const MAX_WINDOW_DAYS: u32 = 30;
/// Minimum confidence for a preference to appear in the Markdown export.
pub const EXPORT_MIN_CONFIDENCE: f64 = 0.6;

/// Run the full rule pipeline over one window of event rows:
/// detect signals, then aggregate them into preference entries.
pub fn analyze_rows(rows: &[PreferenceEventRow]) -> Vec<Preference> {
    let signals = detector::detect_signals(rows);
    aggregator::aggregate(&signals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use signals::{PreferenceCategory, PreferenceSource, PreferenceStatus};

    #[test]
    fn pipeline_produces_aggregated_rule_preferences() {
        let rows: Vec<PreferenceEventRow> = (1..=3)
            .map(|i| PreferenceEventRow {
                id: i,
                session_id: Some("s1".to_string()),
                conversation_id: Some(format!("c{i}")),
                timestamp_ns: Some(i * 1_000_000_000_000),
                user_text: Some("改完之后跑测试，确认没有回归问题".to_string()),
                tool_names: Vec::new(),
            })
            .collect();
        let prefs = analyze_rows(&rows);
        let testing = prefs
            .iter()
            .find(|p| p.key == "testing")
            .expect("testing preference");
        assert_eq!(testing.category, PreferenceCategory::Technical);
        assert_eq!(testing.evidence_count, 3);
        assert_eq!(testing.source, PreferenceSource::Rule);
        assert_eq!(testing.status, PreferenceStatus::Active);
    }

    #[test]
    fn empty_window_yields_no_preferences() {
        assert!(analyze_rows(&[]).is_empty());
    }
}
