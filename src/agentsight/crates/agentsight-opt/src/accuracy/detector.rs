//! Detector trait and shared types for the accuracy oracle engine.
//!
//! Each `Detector` implementation owns a single oracle layer (rule-based, grep,
//! LLM-semantic) and produces `RawIssue`s with an explicit `EvidenceTier`.
//! The orchestrator merges and deduplicates issues from all detectors.

use std::path::Path;

use async_trait::async_trait;

use crate::llm::LlmClient;
use crate::trace::TraceInventory;
use crate::types::{DefectType, EvidenceTier, RootObject};

use crate::accuracy::extract::SharedExtraction;

/// Shared context passed to every detector.
pub struct AnalysisCtx<'a> {
    pub inv: &'a TraceInventory,
    pub client: &'a LlmClient,
    pub repo_root: Option<&'a Path>,
    pub extraction: &'a SharedExtraction,
    /// Shared tally of the LLM judgments this run attempts.
    pub judgments: &'a JudgmentLog,
}

/// Tally of one run's LLM-backed judgments, so a run where every call failed
/// can be told apart from a run that legitimately found nothing (the same
/// distinction `perf::llm` and `cost::llm` make).
#[derive(Debug, Default)]
pub struct JudgmentLog {
    tally: std::sync::Mutex<JudgmentTally>,
}

/// Snapshot of a [`JudgmentLog`].
#[derive(Debug, Default, Clone)]
pub struct JudgmentTally {
    /// Judgment calls made; a run with nothing to judge stays at zero.
    pub attempted: usize,
    /// Judgment calls that returned no usable verdict.
    pub failed: usize,
    /// The last failure, for the error returned when the whole run failed.
    pub last_error: Option<String>,
}

impl JudgmentTally {
    /// `Some(last_error)` when at least one judgment was attempted and every
    /// one of them failed.
    pub fn all_failed(&self) -> Option<&str> {
        if self.attempted > 0 && self.failed == self.attempted {
            self.last_error.as_deref()
        } else {
            None
        }
    }
}

impl JudgmentLog {
    /// Record a judgment whose LLM call returned a usable verdict.
    pub fn record_ok(&self) {
        self.tally().attempted += 1;
    }

    /// Record a judgment whose LLM call failed (transport or parse).
    pub fn record_failure(&self, err: &anyhow::Error) {
        let mut tally = self.tally();
        tally.attempted += 1;
        tally.failed += 1;
        tally.last_error = Some(format!("{err:#}"));
    }

    /// Current counts.
    pub fn snapshot(&self) -> JudgmentTally {
        self.tally().clone()
    }

    fn tally(&self) -> std::sync::MutexGuard<'_, JudgmentTally> {
        self.tally.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(msg: &str) -> anyhow::Error {
        anyhow::anyhow!("{msg}")
    }

    #[test]
    fn tally_is_all_failed_only_when_every_judgment_failed() {
        let log = JudgmentLog::default();
        // Nothing to judge is a legitimate clean run, not a failed one.
        assert!(log.snapshot().all_failed().is_none());

        log.record_ok();
        assert!(log.snapshot().all_failed().is_none());

        log.record_failure(&failure("first"));
        let partial = log.snapshot();
        assert_eq!((partial.attempted, partial.failed), (2, 1));
        assert!(partial.all_failed().is_none());

        let log = JudgmentLog::default();
        log.record_failure(&failure("first"));
        log.record_failure(&failure("second"));
        let all = log.snapshot();
        assert_eq!((all.attempted, all.failed), (2, 2));
        assert_eq!(all.all_failed(), Some("second"));
    }
}

/// A raw issue produced by a single detector, before rule-derived gates.
#[derive(Debug, Clone)]
pub struct RawIssue {
    pub symptom: String,
    pub defect_type: DefectType,
    pub primary_object: RootObject,
    pub evidence_tier: EvidenceTier,
    pub tool_call_id: Option<String>,
    pub detail: String,
    pub verify: String,
    pub fix: String,
}

/// Trait that every detector must implement.
#[async_trait]
pub trait Detector: Send + Sync {
    /// A stable name for logging and metrics.
    fn name(&self) -> &'static str;

    /// Run detection against the shared context.
    /// Returns zero or more raw issues.
    async fn detect(&self, ctx: &AnalysisCtx<'_>) -> Vec<RawIssue>;
}
