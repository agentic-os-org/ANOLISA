//! Mount-session summary statistics for the session metrics log.
//!
//! This module provides an in-memory session collector and a serializable
//! summary record. It is **separate** from the existing audit JSONL sink,
//! security event stream, and protocol event log.
//!
//! ## Decision outcome mapping
//!
//! The following mapping converts SkillFS runtime targets into session
//! outcomes:
//!
//! | SkillFS ActiveTarget / state    | Session outcome |
//! |---------------------------------|--------------------|
//! | `ActiveTarget::Current`         | allow              |
//! | `SkillEventAction::Allowed`     | allow              |
//! | `ActiveTarget::Snapshot`        | fallback           |
//! | `ActiveTarget::Hidden`          | deny               |
//! | `SkillEventAction::Rejected`    | deny               |
//! | `SkillEventKind::PolicyDenied`  | deny               |
//!
//! ## `skill_hit_times` rule (V1)
//!
//! V1 semantics: count of distinct skills that **exist in the store AND
//! are listed in the default view** at mount startup. This is a startup
//! snapshot, NOT a runtime FUSE access counter — no `lookup`/`open`/`read`
//! callbacks are wired in V1. In practice the value equals
//! `default_exposed_count` (total skills minus pruned skills when a
//! `skillfs-views.toml` exists, or total skills when it does not).
//! Future versions may switch to real per-request FUSE hit counting.
//!
//! ## `prompt_token_saved_estimate`
//!
//! Estimated as: total character count of pruned (non-default-view) skill
//! body text, divided by 4 (conservative chars-per-token ratio). The caller
//! supplies this value; computation happens at the CLI layer where the skill
//! store is accessible. When no `skillfs-views.toml` exists, all skills are
//! in the default view and the estimate is 0.
//!
//! ## `allow_times` / `fallback_times` / `deny_times` (V1)
//!
//! V1 semantics: these are **startup snapshot** counts from the initial
//! active resolver state at mount time, not running-period decision counters.
//! Future versions will wire into active resolver refresh, policy deny, and
//! reload outcome paths for runtime accumulation.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::Serialize;

// ---------------------------------------------------------------------------
// RuntimeDecisionOutcome
// ---------------------------------------------------------------------------

/// Coarse outcome for the session metrics log.
///
/// Maps SkillFS runtime decisions into three buckets for session metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeDecisionOutcome {
    /// ActiveTarget::Current or SkillEventAction::Allowed.
    Allow,
    /// ActiveTarget::Snapshot (fallback serving).
    Fallback,
    /// ActiveTarget::Hidden / PolicyDenied / Rejected.
    Deny,
}

// ---------------------------------------------------------------------------
// SkillfsSessionStats (in-memory collector)
// ---------------------------------------------------------------------------

/// In-memory collector for one mount session's statistics.
///
/// Thread-safe via interior `Mutex`. All mutation methods are best-effort:
/// a poisoned lock is silently ignored (the stats may be incomplete, but
/// FUSE behavior is unaffected).
///
/// The collector supports a **flush-once** semantic via
/// [`SkillfsSessionStats::try_build_summary_once`]: the first call returns
/// `Some(summary)`, subsequent calls return `None`. This prevents double
/// writes when multiple exit paths race (e.g. signal handler + normal exit).
pub struct SkillfsSessionStats {
    inner: Mutex<StatsInner>,
    flushed: AtomicBool,
}

struct StatsInner {
    /// Instant the collector was created (before mount starts).
    _created_at: Instant,
    /// Instant the mount became ready (FUSE session started).
    mount_ready_at: Option<Instant>,
    /// Instant the mount exited.
    mount_end_at: Option<Instant>,
    /// Deduplicated set of skill names that received a hit.
    skill_hits: HashSet<String>,
    /// Total loaded skill count (source).
    source_skill_count: u64,
    /// Skills exposed in the default view.
    default_exposed_count: u64,
    /// Token savings estimate (caller-supplied).
    prompt_token_saved_estimate: u64,
    /// Decision outcome counters.
    allow_count: u64,
    fallback_count: u64,
    deny_count: u64,
}

impl SkillfsSessionStats {
    /// Create a new session collector. Call this before mount starts.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(StatsInner {
                _created_at: Instant::now(),
                mount_ready_at: None,
                mount_end_at: None,
                skill_hits: HashSet::new(),
                source_skill_count: 0,
                default_exposed_count: 0,
                prompt_token_saved_estimate: 0,
                allow_count: 0,
                fallback_count: 0,
                deny_count: 0,
            }),
            flushed: AtomicBool::new(false),
        }
    }

    /// Mark the mount as ready (FUSE session started successfully).
    pub fn mark_mount_ready(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.mount_ready_at = Some(Instant::now());
        }
    }

    /// Mark the mount as ended.
    pub fn mark_mount_end(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.mount_end_at = Some(Instant::now());
        }
    }

    /// Set source skill count and default exposed count for pruned
    /// calculation. Should be called once at startup.
    pub fn set_skill_counts(&self, source_count: u64, default_exposed: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.source_skill_count = source_count;
            inner.default_exposed_count = default_exposed;
        }
    }

    /// Set the prompt token savings estimate.
    pub fn set_prompt_token_saved_estimate(&self, estimate: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.prompt_token_saved_estimate = estimate;
        }
    }

    /// Record a skill hit (session-level deduplicated).
    pub fn record_skill_hit(&self, skill_name: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.skill_hits.insert(skill_name.to_string());
        }
    }

    /// Record a runtime decision outcome.
    pub fn record_decision(&self, outcome: RuntimeDecisionOutcome) {
        if let Ok(mut inner) = self.inner.lock() {
            match outcome {
                RuntimeDecisionOutcome::Allow => inner.allow_count += 1,
                RuntimeDecisionOutcome::Fallback => inner.fallback_count += 1,
                RuntimeDecisionOutcome::Deny => inner.deny_count += 1,
            }
        }
    }

    /// Compute the pruned skill count: source - default exposed.
    pub fn pruned_skill_count(&self) -> u64 {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .source_skill_count
                    .saturating_sub(inner.default_exposed_count)
            })
            .unwrap_or(0)
    }

    /// Build the final summary **once**. Returns `None` if already flushed.
    ///
    /// This is the primary API for mount-exit paths. The first call atomically
    /// marks the collector as flushed and returns the summary; all subsequent
    /// calls return `None`. This prevents double-writes when signal handlers
    /// and the normal exit path both try to flush.
    pub fn try_build_summary_once(
        &self,
        session_id: &str,
        agent_name: &str,
    ) -> Option<SkillfsSessionSummary> {
        // Atomically flip false -> true. If it was already true, another
        // caller already flushed.
        if self
            .flushed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        self.mark_mount_end();
        Some(self.build_summary_inner(session_id, agent_name))
    }

    /// Build the summary without flush-once guard. Test-only.
    #[cfg(test)]
    pub fn build_summary(&self, session_id: &str, agent_name: &str) -> SkillfsSessionSummary {
        self.build_summary_inner(session_id, agent_name)
    }

    fn build_summary_inner(&self, session_id: &str, agent_name: &str) -> SkillfsSessionSummary {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        let mount_duration_ms = match (inner.mount_ready_at, inner.mount_end_at) {
            (Some(start), Some(end)) => end.duration_since(start).as_millis() as u64,
            (Some(start), None) => Instant::now().duration_since(start).as_millis() as u64,
            _ => 0,
        };

        let pruned = inner
            .source_skill_count
            .saturating_sub(inner.default_exposed_count);

        SkillfsSessionSummary {
            component_name: "skillfs".to_string(),
            component_version: env!("CARGO_PKG_VERSION").to_string(),
            component_agent_name: agent_name.to_string(),
            session_id: session_id.to_string(),
            mount_times: 1,
            mount_duration_ms,
            skill_hit_times: inner.skill_hits.len() as u64,
            pruned_skill_count: pruned,
            prompt_token_saved_estimate: inner.prompt_token_saved_estimate,
            allow_times: inner.allow_count,
            fallback_times: inner.fallback_count,
            deny_times: inner.deny_count,
        }
    }
}

impl Default for SkillfsSessionStats {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// SkillfsSessionSummary (serializable output)
// ---------------------------------------------------------------------------

/// Serializable mount-session summary for the session metrics log.
///
/// Field naming follows the log rotation contract:
/// - Fixed component fields use dotted names in the JSON output.
/// - All field names are lowercase / snake_case.
/// - Output target: `/var/log/anolisa/sls/ops/skillfs.jsonl`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillfsSessionSummary {
    /// Component name. Always `"skillfs"`.
    #[serde(rename = "component.name")]
    pub component_name: String,
    /// Component version from Cargo.toml.
    #[serde(rename = "component.version")]
    pub component_version: String,
    /// Agent name that this component is serving.
    #[serde(rename = "component.agent_name")]
    pub component_agent_name: String,
    /// Unique session identifier for this mount session.
    pub session_id: String,
    /// Number of successful mounts in this summary (always 1 for V1).
    pub mount_times: u64,
    /// Total mount duration in milliseconds.
    pub mount_duration_ms: u64,
    /// V1: number of distinct skills that exist in the store AND are listed
    /// in the default view at mount startup. This is a startup snapshot, not
    /// a runtime FUSE access counter — equals `default_exposed_count` in V1.
    pub skill_hit_times: u64,
    /// Number of skills pruned from the default view.
    pub pruned_skill_count: u64,
    /// Estimated prompt tokens saved by pruning.
    pub prompt_token_saved_estimate: u64,
    /// Decision outcomes: allowed/current serving count (startup snapshot, V1).
    pub allow_times: u64,
    /// Decision outcomes: fallback snapshot serving count (startup snapshot, V1).
    pub fallback_times: u64,
    /// Decision outcomes: hidden/denied/rejected count (startup snapshot, V1).
    pub deny_times: u64,
}

/// Serialize a session summary as a single JSONL line (no trailing newline).
pub fn serialize_session_summary(summary: &SkillfsSessionSummary) -> String {
    serde_json::to_string(summary).unwrap_or_else(|_| {
        String::from("{\"component.name\":\"skillfs\",\"error\":\"unserializable\"}")
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn new_collector_has_zero_counts() {
        let stats = SkillfsSessionStats::new();
        let summary = stats.build_summary("test-session", "agent");
        assert_eq!(summary.mount_times, 1);
        assert_eq!(summary.mount_duration_ms, 0);
        assert_eq!(summary.skill_hit_times, 0);
        assert_eq!(summary.pruned_skill_count, 0);
        assert_eq!(summary.prompt_token_saved_estimate, 0);
        assert_eq!(summary.allow_times, 0);
        assert_eq!(summary.fallback_times, 0);
        assert_eq!(summary.deny_times, 0);
    }

    #[test]
    fn mount_duration_is_computed_correctly() {
        let stats = SkillfsSessionStats::new();
        stats.mark_mount_ready();
        thread::sleep(Duration::from_millis(50));
        stats.mark_mount_end();
        let summary = stats.build_summary("dur-test", "agent");
        // Should be at least 50ms but less than 500ms.
        assert!(
            summary.mount_duration_ms >= 40,
            "duration too short: {}ms",
            summary.mount_duration_ms
        );
        assert!(
            summary.mount_duration_ms < 500,
            "duration too long: {}ms",
            summary.mount_duration_ms
        );
    }

    #[test]
    fn skill_hit_is_deduplicated() {
        let stats = SkillfsSessionStats::new();
        stats.record_skill_hit("alpha");
        stats.record_skill_hit("alpha");
        stats.record_skill_hit("beta");
        stats.record_skill_hit("alpha");
        let summary = stats.build_summary("hit-test", "agent");
        assert_eq!(summary.skill_hit_times, 2);
    }

    #[test]
    fn pruned_skill_count_calculation() {
        let stats = SkillfsSessionStats::new();
        stats.set_skill_counts(30, 6);
        assert_eq!(stats.pruned_skill_count(), 24);
        let summary = stats.build_summary("prune-test", "agent");
        assert_eq!(summary.pruned_skill_count, 24);
    }

    #[test]
    fn pruned_skill_count_saturates_at_zero() {
        let stats = SkillfsSessionStats::new();
        stats.set_skill_counts(3, 10);
        assert_eq!(stats.pruned_skill_count(), 0);
    }

    #[test]
    fn decision_outcome_counters() {
        let stats = SkillfsSessionStats::new();
        stats.record_decision(RuntimeDecisionOutcome::Allow);
        stats.record_decision(RuntimeDecisionOutcome::Allow);
        stats.record_decision(RuntimeDecisionOutcome::Fallback);
        stats.record_decision(RuntimeDecisionOutcome::Deny);
        stats.record_decision(RuntimeDecisionOutcome::Deny);
        stats.record_decision(RuntimeDecisionOutcome::Deny);
        let summary = stats.build_summary("decision-test", "agent");
        assert_eq!(summary.allow_times, 2);
        assert_eq!(summary.fallback_times, 1);
        assert_eq!(summary.deny_times, 3);
    }

    #[test]
    fn summary_json_shape_has_dotted_component_fields() {
        let stats = SkillfsSessionStats::new();
        stats.set_skill_counts(10, 4);
        stats.set_prompt_token_saved_estimate(1200);
        stats.record_skill_hit("weather");
        stats.record_decision(RuntimeDecisionOutcome::Allow);
        let summary = stats.build_summary("json-test", "agent");
        let json_str = serialize_session_summary(&summary);
        let parsed: serde_json::Value =
            serde_json::from_str(&json_str).expect("must be valid JSON");
        let obj = parsed.as_object().expect("top-level must be object");

        // Dotted component fields.
        assert_eq!(obj["component.name"].as_str().unwrap(), "skillfs");
        assert_eq!(
            obj["component.version"].as_str().unwrap(),
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(obj["component.agent_name"].as_str().unwrap(), "agent");

        // Stats fields.
        assert_eq!(obj["session_id"].as_str().unwrap(), "json-test");
        assert_eq!(obj["mount_times"].as_u64().unwrap(), 1);
        assert_eq!(obj["skill_hit_times"].as_u64().unwrap(), 1);
        assert_eq!(obj["pruned_skill_count"].as_u64().unwrap(), 6);
        assert_eq!(obj["prompt_token_saved_estimate"].as_u64().unwrap(), 1200);
        assert_eq!(obj["allow_times"].as_u64().unwrap(), 1);
        assert_eq!(obj["fallback_times"].as_u64().unwrap(), 0);
        assert_eq!(obj["deny_times"].as_u64().unwrap(), 0);
    }

    #[test]
    fn summary_component_version_matches_cargo_pkg() {
        let stats = SkillfsSessionStats::new();
        let summary = stats.build_summary("ver-test", "agent");
        assert_eq!(summary.component_version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn try_build_summary_once_returns_none_on_second_call() {
        let stats = SkillfsSessionStats::new();
        stats.mark_mount_ready();
        stats.record_skill_hit("alpha");
        let first = stats.try_build_summary_once("once-test", "agent");
        assert!(first.is_some(), "first call must return Some");
        let second = stats.try_build_summary_once("once-test", "agent");
        assert!(
            second.is_none(),
            "second call must return None (flush-once)"
        );
    }

    #[test]
    fn try_build_summary_once_marks_mount_end() {
        let stats = SkillfsSessionStats::new();
        stats.mark_mount_ready();
        std::thread::sleep(Duration::from_millis(20));
        let summary = stats
            .try_build_summary_once("end-test", "agent")
            .expect("must return summary");
        assert!(
            summary.mount_duration_ms >= 15,
            "duration too short: {}ms",
            summary.mount_duration_ms
        );
    }
}
