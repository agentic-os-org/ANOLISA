use std::collections::{HashMap, VecDeque};

use super::model::{ExecutionScope, InsightConfidence, InsightSeverity};

const CORRELATION_TTL_MS: u64 = 10 * 60 * 1000;
const MAX_FACTS_PER_SESSION: usize = 32;
const MAX_RELATED_FACTS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryPressureFact {
    pub(crate) scope: ExecutionScope,
    pub(crate) ended_at_ms: u64,
    pub(crate) severity: InsightSeverity,
    pub(crate) confidence: InsightConfidence,
    pub(crate) source_command_block_id: String,
    pub(crate) provider_safe_fact: String,
}

#[derive(Default)]
pub(crate) struct InsightCorrelationState {
    facts_by_session: HashMap<String, VecDeque<MemoryPressureFact>>,
}

impl InsightCorrelationState {
    pub(crate) fn record(&mut self, fact: MemoryPressureFact) {
        if !fact.scope.allows_correlation()
            || fact.confidence != InsightConfidence::High
            || fact.severity < InsightSeverity::Warning
        {
            return;
        }
        let facts = self
            .facts_by_session
            .entry(fact.scope.session_id.clone())
            .or_default();
        prune_expired(facts, fact.ended_at_ms);
        facts.push_back(fact);
        while facts.len() > MAX_FACTS_PER_SESSION {
            facts.pop_front();
        }
    }

    pub(crate) fn has_recent_memory_pressure(
        &mut self,
        scope: &ExecutionScope,
        target_ended_at_ms: u64,
    ) -> bool {
        if !scope.allows_correlation() {
            return false;
        }
        let Some(facts) = self.facts_by_session.get_mut(&scope.session_id) else {
            return false;
        };
        prune_expired(facts, target_ended_at_ms);
        facts.iter().any(|fact| {
            fact.scope == *scope
                && fact.ended_at_ms <= target_ended_at_ms
                && target_ended_at_ms - fact.ended_at_ms <= CORRELATION_TTL_MS
        })
    }

    pub(crate) fn recent_memory_pressure_facts(
        &mut self,
        scope: &ExecutionScope,
        target_ended_at_ms: u64,
        target_command_block_id: &str,
    ) -> Vec<String> {
        if !scope.allows_correlation() {
            return Vec::new();
        }
        let Some(facts) = self.facts_by_session.get_mut(&scope.session_id) else {
            return Vec::new();
        };
        prune_expired(facts, target_ended_at_ms);
        let mut related = facts
            .iter()
            .rev()
            .filter(|fact| {
                fact.scope == *scope
                    && fact.source_command_block_id != target_command_block_id
                    && fact.ended_at_ms <= target_ended_at_ms
                    && target_ended_at_ms - fact.ended_at_ms <= CORRELATION_TTL_MS
            })
            .take(MAX_RELATED_FACTS)
            .map(|fact| {
                format!(
                    "source_command_block_id={}; {}",
                    fact.source_command_block_id, fact.provider_safe_fact
                )
            })
            .collect::<Vec<_>>();
        related.reverse();
        related
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.facts_by_session.values().map(VecDeque::len).sum()
    }

    #[cfg(test)]
    fn first_ended_at_ms(&self, session_id: &str) -> Option<u64> {
        self.facts_by_session
            .get(session_id)
            .and_then(|facts| facts.front())
            .map(|fact| fact.ended_at_ms)
    }
}

fn prune_expired(facts: &mut VecDeque<MemoryPressureFact>, target_ended_at_ms: u64) {
    facts.retain(|fact| {
        fact.ended_at_ms > target_ended_at_ms
            || target_ended_at_ms - fact.ended_at_ms <= CORRELATION_TTL_MS
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(
        session: &str,
        ended_at_ms: u64,
        severity: InsightSeverity,
        confidence: InsightConfidence,
    ) -> MemoryPressureFact {
        MemoryPressureFact {
            scope: ExecutionScope::local(session),
            ended_at_ms,
            severity,
            confidence,
            source_command_block_id: format!("cmd-{ended_at_ms}"),
            provider_safe_fact: format!("memory pressure at {ended_at_ms}"),
        }
    }

    #[test]
    fn only_local_high_confidence_pressure_is_recorded() {
        let mut state = InsightCorrelationState::default();
        state.record(fact(
            "session-1",
            100,
            InsightSeverity::Candidate,
            InsightConfidence::High,
        ));
        state.record(fact(
            "session-1",
            101,
            InsightSeverity::Warning,
            InsightConfidence::Medium,
        ));
        state.record(MemoryPressureFact {
            scope: ExecutionScope::unknown("session-1"),
            ended_at_ms: 102,
            severity: InsightSeverity::Critical,
            confidence: InsightConfidence::High,
            source_command_block_id: "cmd-102".to_string(),
            provider_safe_fact: "memory pressure at 102".to_string(),
        });
        assert_eq!(state.len(), 0);

        state.record(fact(
            "session-1",
            103,
            InsightSeverity::Warning,
            InsightConfidence::High,
        ));
        assert_eq!(state.len(), 1);
    }

    #[test]
    fn ttl_and_scope_boundaries_are_exact() {
        let mut state = InsightCorrelationState::default();
        state.record(fact(
            "session-1",
            1_000,
            InsightSeverity::Warning,
            InsightConfidence::High,
        ));
        assert!(state.has_recent_memory_pressure(
            &ExecutionScope::local("session-1"),
            1_000 + CORRELATION_TTL_MS
        ));
        assert!(!state.has_recent_memory_pressure(
            &ExecutionScope::local("session-2"),
            1_000 + CORRELATION_TTL_MS
        ));
        assert!(!state.has_recent_memory_pressure(
            &ExecutionScope::local("session-1"),
            1_000 + CORRELATION_TTL_MS + 1
        ));
    }

    #[test]
    fn capacity_evicts_oldest_fact_after_expiry_cleanup() {
        let mut state = InsightCorrelationState::default();
        for offset in 0..=MAX_FACTS_PER_SESSION {
            state.record(fact(
                "session-1",
                10_000 + offset as u64,
                InsightSeverity::Warning,
                InsightConfidence::High,
            ));
        }
        assert_eq!(state.len(), MAX_FACTS_PER_SESSION);
        assert_eq!(state.first_ended_at_ms("session-1"), Some(10_001));
    }

    #[test]
    fn recent_facts_return_latest_three_in_stable_chronological_order() {
        let mut state = InsightCorrelationState::default();
        for offset in 0..4 {
            state.record(MemoryPressureFact {
                scope: ExecutionScope::local("session-1"),
                ended_at_ms: 1_000 + offset,
                severity: InsightSeverity::Warning,
                confidence: InsightConfidence::High,
                source_command_block_id: format!("cmd-{offset}"),
                provider_safe_fact: format!("memory pressure fact {offset}"),
            });
        }

        assert_eq!(
            state.recent_memory_pressure_facts(
                &ExecutionScope::local("session-1"),
                1_003,
                "target-command"
            ),
            vec![
                "source_command_block_id=cmd-1; memory pressure fact 1".to_string(),
                "source_command_block_id=cmd-2; memory pressure fact 2".to_string(),
                "source_command_block_id=cmd-3; memory pressure fact 3".to_string(),
            ]
        );
        assert!(state
            .recent_memory_pressure_facts(
                &ExecutionScope::local("session-2"),
                1_003,
                "target-command"
            )
            .is_empty());
    }

    #[test]
    fn target_command_fact_is_not_returned_as_related_history() {
        let mut state = InsightCorrelationState::default();
        state.record(fact(
            "session-1",
            1_000,
            InsightSeverity::Warning,
            InsightConfidence::High,
        ));

        assert!(state
            .recent_memory_pressure_facts(&ExecutionScope::local("session-1"), 1_000, "cmd-1000")
            .is_empty());
    }
}
