//! Public data types shared by grader storage, API handlers, and dashboard clients.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Rule-based grader version used by the MVP.
pub const RULE_GRADER_VERSION: &str = "rule-v3";

/// Evaluation target kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetType {
    /// A grouped Agent conversation.
    Conversation,
}

impl TargetType {
    /// Stable string used in SQLite idempotency keys.
    pub fn as_str(self) -> &'static str {
        match self {
            TargetType::Conversation => "conversation",
        }
    }
}

/// Evaluator implementation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraderType {
    /// Deterministic rules over captured evidence.
    Rule,
    /// Reserved for future LLM-as-a-Judge.
    Llm,
    /// Reserved for future Agent-as-a-Judge.
    Agent,
}

impl GraderType {
    /// Stable string used in SQLite idempotency keys.
    pub fn as_str(self) -> &'static str {
        match self {
            GraderType::Rule => "rule",
            GraderType::Llm => "llm",
            GraderType::Agent => "agent",
        }
    }
}

/// Top-level evaluation verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The conversation appears successful.
    Pass,
    /// The conversation is usable but has notable risks.
    Warn,
    /// The conversation did not produce a usable outcome.
    Fail,
}

impl Verdict {
    /// Stable string used in SQLite summary columns.
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Warn => "warn",
            Verdict::Fail => "fail",
        }
    }
}

/// Stored run status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationStatus {
    /// Evaluation completed and `result_json` is available.
    Completed,
    /// Evaluation failed before a result was produced.
    Failed,
}

impl EvaluationStatus {
    /// Stable string used in SQLite summary columns.
    pub fn as_str(self) -> &'static str {
        match self {
            EvaluationStatus::Completed => "completed",
            EvaluationStatus::Failed => "failed",
        }
    }
}

/// Single primary cause selected for the top-level result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootCause {
    /// No actionable cause was detected.
    None,
    /// No usable assistant output was captured.
    NoFinalAnswer,
    /// The primary LLM call was interrupted.
    InterruptedMainCall,
    /// The agent process crashed.
    AgentCrash,
    /// Runtime or provider errors were observed.
    RuntimeError,
    /// Tool calls failed or repeated abnormally.
    ToolFailure,
    /// Security or safety signal was non-pass.
    SafetyRisk,
    /// Repeated calls indicate a likely loop.
    LoopDetected,
    /// Token or call count was unusually high.
    ExcessiveCost,
    /// The user intentionally evaluated an incomplete snapshot.
    PartialSnapshot,
}

impl RootCause {
    /// Stable string used in SQLite summary columns.
    pub fn as_str(self) -> &'static str {
        match self {
            RootCause::None => "none",
            RootCause::NoFinalAnswer => "no_final_answer",
            RootCause::InterruptedMainCall => "interrupted_main_call",
            RootCause::AgentCrash => "agent_crash",
            RootCause::RuntimeError => "runtime_error",
            RootCause::ToolFailure => "tool_failure",
            RootCause::SafetyRisk => "safety_risk",
            RootCause::LoopDetected => "loop_detected",
            RootCause::ExcessiveCost => "excessive_cost",
            RootCause::PartialSnapshot => "partial_snapshot",
        }
    }
}

/// Evidence source kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    /// Captured GenAI LLM call row.
    GenaiEvent,
    /// Captured interruption row.
    Interruption,
    /// Reserved for agent-sec security events.
    SecurityEvent,
    /// Trace-level navigation target.
    Trace,
    /// Tool call inside a GenAI message payload.
    ToolCall,
    /// Reserved for persisted ATIF step identifiers.
    AtifStep,
}

/// Navigation target for an evidence reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceTarget {
    /// Conversation detail route anchor.
    pub conversation_id: String,
    /// Optional trace identifier when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Optional LLM call identifier when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Reserved ATIF step identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
}

/// UI deeplink hint for a piece of evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceDeeplink {
    /// Client-side route name.
    pub route: String,
    /// Route parameters and highlight hints.
    pub query: serde_json::Value,
}

/// Evidence reference included in dimensions and findings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationRef {
    /// Evidence source kind.
    #[serde(rename = "type")]
    pub evidence_type: EvidenceType,
    /// Source-local evidence identifier.
    pub id: String,
    /// Short user-facing label.
    pub label: String,
    /// Optional severity string from the source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// Target used by Dashboard navigation.
    pub target: EvidenceTarget,
    /// Optional deeplink target for richer Dashboard routes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deeplink: Option<EvidenceDeeplink>,
    /// Source-specific structured metadata.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

/// Score and explanation for one evaluation dimension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationDimension {
    /// Dimension key, such as `completion` or `runtime_health`.
    pub name: String,
    /// Dimension score in `[0, 1]`.
    pub score: f64,
    /// Dimension-level verdict.
    pub verdict: Verdict,
    /// Human-readable reason.
    pub reason: String,
    /// Supporting evidence references.
    #[serde(default)]
    pub evidence_refs: Vec<EvaluationRef>,
}

/// Actionable issue found during evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationFinding {
    /// Stable finding code.
    pub code: String,
    /// `critical`, `high`, `medium`, or `low`.
    pub severity: String,
    /// Human-readable finding message.
    pub message: String,
    /// Supporting evidence references.
    #[serde(default)]
    pub evidence_refs: Vec<EvaluationRef>,
}

/// Extra metadata that keeps future LLM/Agent judge fields stable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationMetadata {
    /// True when `force=true` evaluated an incomplete snapshot.
    pub evaluated_with_pending: bool,
    /// Number of pending LLM calls in the evaluated input.
    pub pending_call_count: usize,
    /// Number of GenAI LLM call rows used as input.
    pub input_event_count: usize,
    /// Evaluator kind.
    pub grader_type: GraderType,
    /// Evaluator version.
    pub grader_version: String,
    /// Reserved rubric version for LLM/Agent judges.
    pub rubric_version: Option<String>,
    /// Reserved judge model name.
    pub judge_model: Option<String>,
    /// Reserved prompt hash for judge prompts.
    pub prompt_hash: Option<String>,
    /// Reserved confidence score.
    pub confidence: Option<f64>,
}

/// Full persisted evaluation result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationResult {
    /// Evaluated target kind.
    pub target_type: TargetType,
    /// Evaluated target identifier.
    pub target_id: String,
    /// Unique evaluation run identifier.
    pub run_id: String,
    /// Stable hash of the evaluated input snapshot.
    pub input_hash: String,
    /// Top-level verdict.
    pub verdict: Verdict,
    /// Weighted score in `[0, 1]`.
    pub score: f64,
    /// One-sentence summary.
    pub summary: String,
    /// Single primary root cause.
    pub root_cause: RootCause,
    /// Suggested next action.
    pub recommended_action: String,
    /// Per-dimension scores.
    pub dimensions: Vec<EvaluationDimension>,
    /// Actionable findings.
    pub findings: Vec<EvaluationFinding>,
    /// Additional run metadata.
    pub metadata: EvaluationMetadata,
}

/// Stored run projection returned by persistence queries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationRunRecord {
    /// SQLite row id.
    pub id: i64,
    /// Unique evaluation run identifier.
    pub run_id: String,
    /// Evaluated target kind.
    pub target_type: TargetType,
    /// Evaluated target id.
    pub target_id: String,
    /// Stable input hash.
    pub input_hash: String,
    /// Evaluator kind.
    pub grader_type: GraderType,
    /// Evaluator version.
    pub grader_version: String,
    /// Stored run status.
    pub status: EvaluationStatus,
    /// Top-level verdict.
    pub verdict: Option<Verdict>,
    /// Weighted score.
    pub score: Option<f64>,
    /// Single primary root cause.
    pub root_cause: Option<RootCause>,
    /// Creation timestamp as stored by SQLite.
    pub created_at: String,
    /// Completion timestamp as stored by SQLite.
    pub completed_at: Option<String>,
    /// Full result payload for completed runs.
    pub result: Option<EvaluationResult>,
}

/// Evaluation request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationRequest {
    /// Evaluation target kind.
    pub target_type: String,
    /// Evaluation target identifier.
    pub target_id: String,
    /// Evaluate incomplete snapshots instead of returning 409.
    #[serde(default)]
    pub force: bool,
}

/// Evaluation API response body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationResponse {
    /// Completed evaluation result.
    pub result: EvaluationResult,
    /// True when an idempotent completed run was reused.
    pub reused_existing_run: bool,
}

/// Errors returned by grader components.
#[derive(Debug, Error)]
pub enum GraderError {
    /// The requested conversation has no captured events.
    #[error("conversation not found: {0}")]
    ConversationNotFound(String),
    /// The requested conversation still has pending LLM calls.
    #[error("conversation still has {pending_count} pending LLM call(s)")]
    ConversationNotReady {
        /// Number of pending calls found.
        pending_count: usize,
    },
    /// The request uses a target kind this MVP does not support.
    #[error("unsupported target type: {0}")]
    UnsupportedTarget(String),
    /// SQLite or storage-layer failure.
    #[error("storage error: {0}")]
    Storage(String),
    /// JSON serialization or deserialization failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl GraderError {
    /// Stable machine-readable error code for HTTP handlers and tests.
    pub fn code(&self) -> &'static str {
        match self {
            GraderError::ConversationNotFound(_) => "conversation_not_found",
            GraderError::ConversationNotReady { .. } => "conversation_not_ready",
            GraderError::UnsupportedTarget(_) => "unsupported_target",
            GraderError::Storage(_) => "storage_error",
            GraderError::Json(_) => "json_error",
        }
    }
}
