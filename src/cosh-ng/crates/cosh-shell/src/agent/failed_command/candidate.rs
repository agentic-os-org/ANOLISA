use crate::evidence::model::EvidenceExcerpt;
use crate::insight::model::{
    InsightBinding, InsightCandidate, InsightConfidence, InsightEvidence, InsightSeverity,
    InsightSource, InsightTarget, PromptSuggestion, SuppressionTopic,
};
use crate::insight::policy::failure_suppression_key;
use crate::insight::scope::resolve_execution_scope;
use crate::types::CommandBlock;

use super::failure_output_status;

pub(super) fn command_not_found_agent_candidate(
    block: &CommandBlock,
    excerpt: &EvidenceExcerpt,
) -> InsightCandidate {
    let scope = resolve_execution_scope(&block.session_id, &block.command);
    let suppression_key = failure_suppression_key(
        SuppressionTopic::CommandNotFound,
        &block.command,
        scope.clone(),
    );
    let target = InsightTarget {
        insight_id: format!("failure-{}", block.id),
        source_session_id: block.session_id.clone(),
        source_command_block_id: block.id.clone(),
        scope: scope.clone(),
        evidence_handle: Some(crate::evidence::terminal_output_id(
            &block.session_id,
            &block.id,
        )),
        evidence_status: failure_output_status(block, excerpt),
        severity: InsightSeverity::Warning,
        confidence: InsightConfidence::High,
        evidence: vec![InsightEvidence {
            key: "top_level_missing".to_string(),
            value: "proven".to_string(),
        }],
        created_at_ms: block.ended_at_ms,
    };
    InsightCandidate {
        source: InsightSource::FailedCommand,
        topic: SuppressionTopic::CommandNotFound,
        entity: suppression_key.entity.clone(),
        severity: InsightSeverity::Warning,
        confidence: InsightConfidence::High,
        evidence: target.evidence.clone(),
        suggestion: Some(PromptSuggestion::AgentPrompt {
            binding: Box::new(InsightBinding {
                suggestion_id: format!("failure-suggestion-{}", block.id),
                target,
            }),
        }),
        scope,
        suppression_key,
    }
}
