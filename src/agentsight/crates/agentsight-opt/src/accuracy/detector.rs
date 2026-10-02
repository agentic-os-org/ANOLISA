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
