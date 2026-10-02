//! Conversation quality evaluation for AgentSight.
//!
//! The MVP is a manual, rule-based grader for conversation snapshots.

mod evidence;
pub mod input;
pub mod rule;
pub mod storage;
pub mod types;

pub use input::{EvaluationInput, load_conversation_input};
pub use rule::RuleGrader;
pub use storage::EvaluationStore;
pub use types::{
    EvaluationDimension, EvaluationFinding, EvaluationMetadata, EvaluationRef, EvaluationRequest,
    EvaluationResponse, EvaluationResult, EvaluationRunRecord, EvaluationStatus, EvidenceDeeplink,
    EvidenceTarget, EvidenceType, GraderError, GraderType, RULE_GRADER_VERSION, RootCause,
    TargetType, Verdict,
};
