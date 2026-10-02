use std::collections::HashMap;

use super::model::{
    CommandIntent, EntityKey, ExecutionScope, InsightCandidate, InsightSeverity,
    InterventionDecision, PromptSuggestion, SuppressionKey, SuppressionTopic,
};
use super::scope::direct_program;

const SUPPRESSION_VERSION: u8 = 1;
const COOLDOWN_MS: u64 = 10 * 60 * 1000;
const INTERRUPTION_BUDGET_MAX_ENTRIES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnalysisPolicyMode {
    Smart,
    Auto,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InterventionGates {
    pub(crate) same_dispatch_batch: bool,
    pub(crate) input_empty: bool,
    pub(crate) foreground_idle: bool,
    pub(crate) active_runtime_idle: bool,
    pub(crate) user_has_not_continued: bool,
    pub(crate) user_interactive_origin: bool,
    pub(crate) budget_available: bool,
}

impl InterventionGates {
    pub(crate) fn eligible() -> Self {
        Self {
            same_dispatch_batch: true,
            input_empty: true,
            foreground_idle: true,
            active_runtime_idle: true,
            user_has_not_continued: true,
            user_interactive_origin: true,
            budget_available: true,
        }
    }

    fn all_allow(self) -> bool {
        self.same_dispatch_batch
            && self.input_empty
            && self.foreground_idle
            && self.active_runtime_idle
            && self.user_has_not_continued
            && self.user_interactive_origin
            && self.budget_available
    }
}

pub(crate) fn decide_candidate_intervention(
    candidate: &InsightCandidate,
    mode: AnalysisPolicyMode,
    gates: InterventionGates,
    auto_analyze: bool,
) -> InterventionDecision {
    if mode == AnalysisPolicyMode::Manual || !gates.all_allow() {
        return InterventionDecision::Silent;
    }
    let Some(suggestion) = candidate.suggestion.clone() else {
        return InterventionDecision::Silent;
    };
    let insight = super::model::InlineInsight {
        topic: candidate.topic.clone(),
        entity: candidate.entity.clone(),
        severity: candidate.severity,
    };
    match suggestion {
        PromptSuggestion::AgentPrompt { binding, .. } if auto_analyze => {
            InterventionDecision::AutoAnalyze {
                activity: insight,
                target: binding.target,
            }
        }
        suggestion => InterventionDecision::Suggest {
            insight,
            suggestion,
        },
    }
}

pub(crate) fn failure_suppression_key(
    topic: SuppressionTopic,
    command: &str,
    scope: ExecutionScope,
) -> SuppressionKey {
    let intent = if topic == SuppressionTopic::CommandNotFound {
        CommandIntent::RepairCommand
    } else {
        CommandIntent::AnalyzeFailure
    };
    SuppressionKey {
        version: SUPPRESSION_VERSION,
        topic,
        entity: program_entity(command),
        scope,
        intent,
    }
}

pub(crate) fn memory_pressure_suppression_key(scope: ExecutionScope) -> SuppressionKey {
    SuppressionKey {
        version: SUPPRESSION_VERSION,
        topic: SuppressionTopic::MemoryPressure,
        entity: EntityKey::SystemMemory,
        scope,
        intent: CommandIntent::DiagnoseMemoryPressure,
    }
}

pub(crate) fn process_memory_suppression_key(
    command_basename: &str,
    scope: ExecutionScope,
    root_cause: bool,
) -> SuppressionKey {
    SuppressionKey {
        version: SUPPRESSION_VERSION,
        topic: if root_cause {
            SuppressionTopic::MemoryRootCause
        } else {
            SuppressionTopic::HighMemoryProcess
        },
        entity: process_entity(command_basename),
        scope,
        intent: if root_cause {
            CommandIntent::DiagnoseMemoryRootCause
        } else {
            CommandIntent::DiagnoseProcessMemory
        },
    }
}

fn program_entity(command: &str) -> EntityKey {
    direct_program(command)
        .and_then(safe_basename)
        .map(|program| EntityKey::Program(program.to_string()))
        .unwrap_or(EntityKey::Unknown)
}

fn process_entity(command_basename: &str) -> EntityKey {
    safe_basename(command_basename)
        .map(|program| EntityKey::Process(program.to_string()))
        .unwrap_or(EntityKey::Unknown)
}

fn safe_basename(value: &str) -> Option<&str> {
    let basename = value.rsplit('/').next()?;
    (!basename.is_empty()
        && basename.len() <= 128
        && basename
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte)))
    .then_some(basename)
}

#[derive(Default)]
pub(crate) struct InterruptionBudget {
    visible: HashMap<SuppressionKey, (u64, InsightSeverity)>,
}

impl InterruptionBudget {
    pub(crate) fn is_suppressed(
        &mut self,
        key: &SuppressionKey,
        severity: InsightSeverity,
        now_ms: u64,
    ) -> bool {
        self.remove_expired(now_ms);
        self.visible
            .get(key)
            .is_some_and(|(shown_at_ms, shown_severity)| {
                now_ms.saturating_sub(*shown_at_ms) <= COOLDOWN_MS && severity <= *shown_severity
            })
    }

    pub(crate) fn should_suppress(
        &mut self,
        key: SuppressionKey,
        severity: InsightSeverity,
        now_ms: u64,
    ) -> bool {
        if self.is_suppressed(&key, severity, now_ms) {
            return true;
        }
        self.visible.insert(key, (now_ms, severity));
        while self.visible.len() > INTERRUPTION_BUDGET_MAX_ENTRIES {
            let Some(oldest) = self
                .visible
                .iter()
                .min_by_key(|(_, (shown_at_ms, _))| *shown_at_ms)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.visible.remove(&oldest);
        }
        false
    }

    fn remove_expired(&mut self, now_ms: u64) {
        self.visible
            .retain(|_, (shown_at_ms, _)| now_ms.saturating_sub(*shown_at_ms) <= COOLDOWN_MS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::insight::model::{
        InsightBinding, InsightConfidence, InsightSource, InsightTarget, OutputExcerptStatus,
    };

    fn candidate() -> InsightCandidate {
        let scope = ExecutionScope::local("session-1");
        InsightCandidate {
            source: InsightSource::Top,
            topic: SuppressionTopic::MemoryPressure,
            entity: EntityKey::SystemMemory,
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
            suppression_key: memory_pressure_suppression_key(scope),
        }
    }

    #[test]
    fn common_decision_keeps_memory_auto_as_suggestion() {
        assert!(matches!(
            decide_candidate_intervention(
                &candidate(),
                AnalysisPolicyMode::Auto,
                InterventionGates::eligible(),
                false,
            ),
            InterventionDecision::Suggest { .. }
        ));
    }

    #[test]
    fn common_decision_rejects_manual_and_failed_gates() {
        assert_eq!(
            decide_candidate_intervention(
                &candidate(),
                AnalysisPolicyMode::Manual,
                InterventionGates::eligible(),
                false,
            ),
            InterventionDecision::Silent
        );
        let mut gates = InterventionGates::eligible();
        gates.input_empty = false;
        assert_eq!(
            decide_candidate_intervention(&candidate(), AnalysisPolicyMode::Smart, gates, false),
            InterventionDecision::Silent
        );
    }

    #[test]
    fn suppression_entities_use_safe_basenames_without_raw_command_data() {
        let scope = ExecutionScope::local("session-1");
        let key = failure_suppression_key(
            SuppressionTopic::CommandNotFound,
            "/usr/local/bin/Grpe --color token=secret",
            scope.clone(),
        );
        assert_eq!(key.version, 1);
        assert_eq!(key.entity, EntityKey::Program("Grpe".to_string()));
        assert_eq!(key.intent, CommandIntent::RepairCommand);

        let unsafe_key = failure_suppression_key(
            SuppressionTopic::PermissionDenied,
            "/tmp/命令 --secret value",
            scope,
        );
        assert_eq!(unsafe_key.entity, EntityKey::Unknown);
    }

    #[test]
    fn memory_sources_share_stable_keys_without_pid() {
        let scope = ExecutionScope::local("session-1");
        assert_eq!(
            memory_pressure_suppression_key(scope.clone()),
            memory_pressure_suppression_key(scope.clone())
        );
        assert_eq!(
            process_memory_suppression_key("java", scope.clone(), false),
            process_memory_suppression_key("java", scope, false)
        );
    }

    #[test]
    fn cooldown_suppresses_same_or_lower_severity_but_not_escalation() {
        let key = memory_pressure_suppression_key(ExecutionScope::local("session-1"));
        let mut budget = InterruptionBudget::default();

        assert!(!budget.should_suppress(key.clone(), InsightSeverity::Warning, 1_000));
        assert!(budget.should_suppress(key.clone(), InsightSeverity::Warning, 1_000 + COOLDOWN_MS));
        assert!(!budget.should_suppress(key.clone(), InsightSeverity::Critical, 1_001));
        assert!(!budget.should_suppress(key, InsightSeverity::Warning, 1_001 + COOLDOWN_MS + 1));
    }

    #[test]
    fn interruption_budget_evicts_expired_entries_and_stays_bounded() {
        let scope = ExecutionScope::local("session-1");
        let mut budget = InterruptionBudget::default();
        let expired = process_memory_suppression_key("expired", scope.clone(), false);
        assert!(!budget.should_suppress(expired.clone(), InsightSeverity::Warning, 1));

        for index in 0..300 {
            let key =
                process_memory_suppression_key(&format!("process-{index}"), scope.clone(), false);
            assert!(!budget.should_suppress(
                key,
                InsightSeverity::Warning,
                COOLDOWN_MS + 2 + index
            ));
        }

        assert!(!budget.visible.contains_key(&expired));
        assert!(budget.visible.len() <= 128, "{}", budget.visible.len());
    }
}
