use crate::command::{FailureAutoEligibility, FailureClass, FailureConfidence, FailureSemantics};

use super::model::{InsightCandidate, InterventionDecision, PromptSuggestion};
use super::policy::{decide_candidate_intervention, AnalysisPolicyMode, InterventionGates};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureInsightKind {
    CommandNotFound,
    PermissionDenied,
    BuildOrTestFailure,
    RuntimeException,
    AbnormalSignal,
}

pub(crate) fn map_failure_semantics(semantics: &FailureSemantics) -> Option<FailureInsightKind> {
    match semantics.class {
        FailureClass::CommandNotFound => Some(FailureInsightKind::CommandNotFound),
        FailureClass::PermissionDenied => Some(FailureInsightKind::PermissionDenied),
        FailureClass::BuildOrTestFailure => Some(FailureInsightKind::BuildOrTestFailure),
        FailureClass::RuntimeException => Some(FailureInsightKind::RuntimeException),
        FailureClass::AbnormalSignal => Some(FailureInsightKind::AbnormalSignal),
        _ => None,
    }
}

pub(crate) fn decide_failure_intervention(
    kind: FailureInsightKind,
    confidence: FailureConfidence,
    auto_eligibility: FailureAutoEligibility,
    output_usable: bool,
    candidate: &InsightCandidate,
    mode: AnalysisPolicyMode,
    gates: InterventionGates,
) -> InterventionDecision {
    if confidence != FailureConfidence::High {
        return InterventionDecision::Silent;
    }
    let has_agent_prompt = matches!(
        candidate.suggestion.as_ref(),
        Some(PromptSuggestion::AgentPrompt { .. })
    );
    let auto_analyze = mode == AnalysisPolicyMode::Auto
        && auto_eligibility == FailureAutoEligibility::LegacyAllowlisted
        && has_agent_prompt
        && (kind == FailureInsightKind::PermissionDenied
            || (matches!(
                kind,
                FailureInsightKind::BuildOrTestFailure | FailureInsightKind::AbnormalSignal
            ) && output_usable));
    match (kind, candidate.suggestion.as_ref()) {
        (
            FailureInsightKind::CommandNotFound,
            Some(PromptSuggestion::ShellRewrite { .. } | PromptSuggestion::AgentPrompt { .. }),
        )
        | (
            FailureInsightKind::PermissionDenied
            | FailureInsightKind::BuildOrTestFailure
            | FailureInsightKind::RuntimeException
            | FailureInsightKind::AbnormalSignal,
            Some(PromptSuggestion::AgentPrompt { .. }),
        ) => decide_candidate_intervention(candidate, mode, gates, auto_analyze),
        _ => InterventionDecision::Silent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{
        FailureAutoEligibility, FailureClass, FailureConfidence, FailureSemantics,
    };
    use crate::insight::model::{
        CommandIntent, EntityKey, ExecutionScope, InsightBinding, InsightCandidate,
        InsightConfidence, InsightSeverity, InsightSource, InsightTarget, OutputExcerptStatus,
        PromptSuggestion, SuppressionKey, SuppressionTopic,
    };

    fn semantics(class: FailureClass, confidence: FailureConfidence) -> FailureSemantics {
        FailureSemantics {
            class,
            confidence,
            auto_eligibility: FailureAutoEligibility::SuggestOnly,
            reasons: Vec::new(),
        }
    }

    fn candidate() -> InsightCandidate {
        let scope = ExecutionScope::local("session-1");
        InsightCandidate {
            source: InsightSource::FailedCommand,
            topic: SuppressionTopic::BuildOrTestFailure,
            entity: EntityKey::Program("cargo".to_string()),
            severity: InsightSeverity::Warning,
            confidence: InsightConfidence::High,
            evidence: Vec::new(),
            suggestion: Some(PromptSuggestion::AgentPrompt {
                binding: Box::new(InsightBinding {
                    suggestion_id: "suggestion-1".to_string(),
                    target: InsightTarget {
                        insight_id: "insight-1".to_string(),
                        source_session_id: "session-1".to_string(),
                        source_command_block_id: "command-1".to_string(),
                        scope: scope.clone(),
                        evidence_handle: None,
                        evidence_status: OutputExcerptStatus::Available,
                        severity: InsightSeverity::Warning,
                        confidence: InsightConfidence::High,
                        evidence: Vec::new(),
                        created_at_ms: 1_000,
                    },
                }),
            }),
            scope: scope.clone(),
            suppression_key: SuppressionKey {
                version: 1,
                topic: SuppressionTopic::BuildOrTestFailure,
                entity: EntityKey::Program("cargo".to_string()),
                scope,
                intent: CommandIntent::AnalyzeFailure,
            },
        }
    }

    #[test]
    fn failure_semantics_map_only_actionable_classes() {
        assert_eq!(
            map_failure_semantics(&semantics(
                FailureClass::PermissionDenied,
                FailureConfidence::High
            )),
            Some(FailureInsightKind::PermissionDenied)
        );
        assert_eq!(
            map_failure_semantics(&semantics(
                FailureClass::BuildOrTestFailure,
                FailureConfidence::High
            )),
            Some(FailureInsightKind::BuildOrTestFailure)
        );
        assert_eq!(
            map_failure_semantics(&semantics(
                FailureClass::RuntimeException,
                FailureConfidence::High
            )),
            Some(FailureInsightKind::RuntimeException)
        );
        for class in [
            FailureClass::UsageOrHelp,
            FailureClass::ExpectedNoResult,
            FailureClass::GenericRuntimeFailure,
            FailureClass::UnknownFailure,
        ] {
            assert_eq!(
                map_failure_semantics(&semantics(class, FailureConfidence::High)),
                None
            );
        }
    }

    #[test]
    fn policy_matrix_keeps_auto_allowlist_narrow() {
        let gates = InterventionGates::eligible();
        assert!(matches!(
            decide_failure_intervention(
                FailureInsightKind::BuildOrTestFailure,
                FailureConfidence::High,
                FailureAutoEligibility::LegacyAllowlisted,
                true,
                &candidate(),
                AnalysisPolicyMode::Smart,
                gates
            ),
            InterventionDecision::Suggest { .. }
        ));
        assert!(matches!(
            decide_failure_intervention(
                FailureInsightKind::BuildOrTestFailure,
                FailureConfidence::High,
                FailureAutoEligibility::LegacyAllowlisted,
                true,
                &candidate(),
                AnalysisPolicyMode::Auto,
                gates
            ),
            InterventionDecision::AutoAnalyze { .. }
        ));
        assert!(matches!(
            decide_failure_intervention(
                FailureInsightKind::BuildOrTestFailure,
                FailureConfidence::High,
                FailureAutoEligibility::LegacyAllowlisted,
                false,
                &candidate(),
                AnalysisPolicyMode::Auto,
                gates
            ),
            InterventionDecision::Suggest { .. }
        ));
        assert_eq!(
            decide_failure_intervention(
                FailureInsightKind::BuildOrTestFailure,
                FailureConfidence::High,
                FailureAutoEligibility::LegacyAllowlisted,
                true,
                &candidate(),
                AnalysisPolicyMode::Manual,
                gates
            ),
            InterventionDecision::Silent
        );
    }

    #[test]
    fn auto_runtime_gate_failure_expires_instead_of_downgrading() {
        let mut gates = InterventionGates::eligible();
        gates.active_runtime_idle = false;
        assert_eq!(
            decide_failure_intervention(
                FailureInsightKind::PermissionDenied,
                FailureConfidence::High,
                FailureAutoEligibility::LegacyAllowlisted,
                true,
                &candidate(),
                AnalysisPolicyMode::Auto,
                gates
            ),
            InterventionDecision::Silent
        );
    }

    #[test]
    fn suggest_only_failure_never_auto_analyzes() {
        assert!(matches!(
            decide_failure_intervention(
                FailureInsightKind::BuildOrTestFailure,
                FailureConfidence::High,
                FailureAutoEligibility::SuggestOnly,
                true,
                &candidate(),
                AnalysisPolicyMode::Auto,
                InterventionGates::eligible(),
            ),
            InterventionDecision::Suggest { .. }
        ));
    }
}
