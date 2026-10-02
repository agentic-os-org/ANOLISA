use std::collections::BTreeSet;

use crate::types::{
    BuiltinFactRecord, BuiltinFindingFacts, CommandBlock, HighMemoryProcessFacts, HookFinding,
    HookProvenance, MemoryPressureFacts, MetricsConfidence,
};

use super::correlation::{InsightCorrelationState, MemoryPressureFact};
use super::model::{
    EntityKey, InsightBinding, InsightCandidate, InsightConfidence, InsightEvidence,
    InsightSeverity, InsightSource, InsightTarget, OutputExcerptStatus, PromptSuggestion,
};
use super::policy::{memory_pressure_suppression_key, process_memory_suppression_key};
use super::scope::{direct_program, resolve_execution_scope};

const MEMORY_PRESSURE_HOOK: &str = "memory-pressure";
const HIGH_MEMORY_PROCESS_HOOK: &str = "high-memory-process";

pub(crate) struct MemoryAggregateView<'a> {
    pub(crate) provenance: &'a HookProvenance,
    pub(crate) primary: &'a HookFinding,
    pub(crate) related: &'a [HookFinding],
    pub(crate) builtin_facts: &'a [BuiltinFactRecord],
}

impl<'a> MemoryAggregateView<'a> {
    pub(crate) fn new(
        provenance: &'a HookProvenance,
        primary: &'a HookFinding,
        related: &'a [HookFinding],
    ) -> Self {
        Self {
            provenance,
            primary,
            related,
            builtin_facts: &[],
        }
    }

    pub(crate) fn new_with_facts(
        provenance: &'a HookProvenance,
        primary: &'a HookFinding,
        related: &'a [HookFinding],
        builtin_facts: &'a [BuiltinFactRecord],
    ) -> Self {
        Self {
            provenance,
            primary,
            related,
            builtin_facts,
        }
    }
}

pub(crate) enum MemoryInsightOutcome {
    NotClaimed,
    Claimed(Option<Box<InsightCandidate>>),
    ClaimedError(&'static str),
}

pub(crate) fn adapt_memory_aggregate(
    block: &CommandBlock,
    aggregate: MemoryAggregateView<'_>,
    correlation: &mut InsightCorrelationState,
) -> MemoryInsightOutcome {
    if !claims_memory_aggregate(aggregate.provenance) {
        return MemoryInsightOutcome::NotClaimed;
    }
    let HookProvenance::Builtin {
        producer_registration_ids,
    } = aggregate.provenance
    else {
        unreachable!();
    };

    let facts = match validate_memory_facts(producer_registration_ids, aggregate.builtin_facts) {
        Ok(facts) => facts,
        Err(reason) => return MemoryInsightOutcome::ClaimedError(reason),
    };

    let scope = resolve_execution_scope(&block.session_id, &block.command);
    if !scope.allows_correlation() {
        return MemoryInsightOutcome::Claimed(None);
    }
    let source = match direct_program(&block.command) {
        Some("free") => InsightSource::Free,
        Some("top") => InsightSource::Top,
        Some("ps") => InsightSource::Ps,
        _ => return MemoryInsightOutcome::ClaimedError("unsupported-memory-command"),
    };

    let findings = std::iter::once(aggregate.primary).chain(aggregate.related.iter());
    let mut payload_ids = BTreeSet::new();
    for finding in findings {
        if !payload_ids.insert(finding.hook_id.as_str()) {
            return MemoryInsightOutcome::ClaimedError("duplicate-memory-finding");
        }
        match finding.hook_id.as_str() {
            MEMORY_PRESSURE_HOOK | HIGH_MEMORY_PROCESS_HOOK => {}
            _ => return MemoryInsightOutcome::ClaimedError("invalid-memory-finding"),
        }
    }
    if payload_ids.len() != producer_registration_ids.len()
        || !payload_ids
            .iter()
            .all(|id| producer_registration_ids.contains(*id))
    {
        return MemoryInsightOutcome::ClaimedError("memory-producer-payload-mismatch");
    }

    let pressure = facts.pressure.and_then(|facts| {
        (facts.confidence == MetricsConfidence::High)
            .then(|| pressure_severity(facts.available_ratio))
            .flatten()
    });
    if let Some(severity) = pressure {
        correlation.record(MemoryPressureFact {
            scope: scope.clone(),
            ended_at_ms: block.ended_at_ms,
            severity,
            confidence: InsightConfidence::High,
            source_command_block_id: provider_safe_identifier(&block.id),
            provider_safe_fact: format!(
                "memory_pressure severity={severity:?} ended_at_ms={}",
                block.ended_at_ms
            ),
        });
    }

    let process_facts = facts
        .process
        .filter(|facts| facts.confidence == MetricsConfidence::High);
    let process_severity = process_facts.and_then(process_severity);
    let recent_pressure = process_facts.is_some()
        && correlation.has_recent_memory_pressure(&scope, block.ended_at_ms);
    let process_visible = process_severity.is_some_and(|severity| {
        severity != InsightSeverity::Candidate || pressure.is_some() || recent_pressure
    });
    if pressure.is_none() && !process_visible {
        return MemoryInsightOutcome::Claimed(None);
    }

    let process = process_visible.then_some(process_facts).flatten();
    let root_cause = pressure.is_some() && process.is_some();
    let mut severity = pressure
        .into_iter()
        .chain(process_severity.filter(|_| process_visible))
        .max()
        .unwrap_or(InsightSeverity::Candidate);
    if let (Some(pressure), Some(process)) = (pressure, process) {
        let top_mem_pct = process.processes[0].mem_pct;
        if pressure == InsightSeverity::Critical && top_mem_pct >= 35.0 {
            severity = InsightSeverity::Critical;
        } else if top_mem_pct >= 20.0 {
            severity = severity.max(InsightSeverity::Warning);
        }
    }
    let confidence = InsightConfidence::High;
    let process_name = process.map(|facts| facts.processes[0].command_basename.as_str());
    let (topic, entity, suppression_key) = if let Some(process_name) = process_name {
        let key = process_memory_suppression_key(process_name, scope.clone(), root_cause);
        (key.topic.clone(), key.entity.clone(), key)
    } else {
        let key = memory_pressure_suppression_key(scope.clone());
        (key.topic.clone(), EntityKey::SystemMemory, key)
    };
    let evidence = vec![InsightEvidence {
        key: "command_block_id".to_string(),
        value: block.id.clone(),
    }];
    let target = InsightTarget {
        insight_id: format!("memory-{}", block.id),
        source_session_id: block.session_id.clone(),
        source_command_block_id: block.id.clone(),
        scope: scope.clone(),
        evidence_handle: block
            .output
            .terminal_output_ref
            .as_ref()
            .map(|_| crate::evidence::terminal_output_id(&block.session_id, &block.id)),
        evidence_status: memory_output_status(block),
        severity,
        confidence,
        evidence: evidence.clone(),
        created_at_ms: block.ended_at_ms,
    };
    MemoryInsightOutcome::Claimed(Some(Box::new(InsightCandidate {
        source,
        topic,
        entity,
        severity,
        confidence,
        evidence,
        suggestion: Some(PromptSuggestion::AgentPrompt {
            binding: Box::new(InsightBinding {
                suggestion_id: format!("memory-suggestion-{}", block.id),
                target,
            }),
        }),
        scope,
        suppression_key,
    })))
}

fn memory_output_status(block: &CommandBlock) -> OutputExcerptStatus {
    match crate::evidence::evidence_capture_status_for_block(block) {
        crate::evidence::EvidenceCaptureStatus::Available => OutputExcerptStatus::Available,
        crate::evidence::EvidenceCaptureStatus::Truncated => OutputExcerptStatus::Truncated,
        crate::evidence::EvidenceCaptureStatus::Expired => OutputExcerptStatus::Expired,
        crate::evidence::EvidenceCaptureStatus::Unavailable => OutputExcerptStatus::Unavailable,
        crate::evidence::EvidenceCaptureStatus::ReadFailed => OutputExcerptStatus::ReadFailed,
    }
}

struct ValidatedMemoryFacts<'a> {
    pressure: Option<&'a MemoryPressureFacts>,
    process: Option<&'a HighMemoryProcessFacts>,
}

fn validate_memory_facts<'a>(
    producer_registration_ids: &BTreeSet<String>,
    records: &'a [BuiltinFactRecord],
) -> Result<ValidatedMemoryFacts<'a>, &'static str> {
    if records.is_empty() {
        return Err("missing-memory-facts");
    }
    let mut pressure = None;
    let mut process = None;
    let mut fact_producers = BTreeSet::new();
    for record in records {
        if !fact_producers.insert(record.producer_registration_id.as_str()) {
            return Err("duplicate-memory-facts");
        }
        if !producer_registration_ids.contains(&record.producer_registration_id) {
            return Err("memory-producer-facts-mismatch");
        }
        match (&record.producer_registration_id[..], &record.facts) {
            (MEMORY_PRESSURE_HOOK, BuiltinFindingFacts::MemoryPressure(facts)) => {
                validate_pressure_facts(facts)?;
                pressure = Some(facts);
            }
            (HIGH_MEMORY_PROCESS_HOOK, BuiltinFindingFacts::HighMemoryProcesses(facts)) => {
                validate_process_facts(facts)?;
                process = Some(facts);
            }
            _ => return Err("memory-producer-facts-mismatch"),
        }
    }
    if fact_producers.len() != producer_registration_ids.len() {
        return Err("missing-memory-facts");
    }
    Ok(ValidatedMemoryFacts { pressure, process })
}

fn validate_pressure_facts(facts: &MemoryPressureFacts) -> Result<(), &'static str> {
    if !valid_ratio(facts.available_ratio)
        || facts.swap_ratio.is_some_and(|ratio| !valid_ratio(ratio))
    {
        return Err("invalid-memory-pressure-facts");
    }
    Ok(())
}

fn validate_process_facts(facts: &HighMemoryProcessFacts) -> Result<(), &'static str> {
    if facts.processes.is_empty() || facts.processes.len() > 3 {
        return Err("invalid-memory-process-facts");
    }
    let mut previous_mem_pct = f64::INFINITY;
    for process in &facts.processes {
        if process.pid.is_empty()
            || !process.pid.bytes().all(|byte| byte.is_ascii_digit())
            || !safe_process_basename(&process.command_basename)
            || !process.mem_pct.is_finite()
            || !(0.0..=100.0).contains(&process.mem_pct)
            || process.mem_pct > previous_mem_pct
        {
            return Err("invalid-memory-process-facts");
        }
        previous_mem_pct = process.mem_pct;
    }
    Ok(())
}

fn valid_ratio(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn safe_process_basename(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
}

fn pressure_severity(available_ratio: f64) -> Option<InsightSeverity> {
    if available_ratio <= 0.05 {
        Some(InsightSeverity::Critical)
    } else if available_ratio <= 0.10 {
        Some(InsightSeverity::Warning)
    } else {
        None
    }
}

fn process_severity(facts: &HighMemoryProcessFacts) -> Option<InsightSeverity> {
    let mem_pct = facts.processes.first()?.mem_pct;
    if mem_pct >= 50.0 {
        Some(InsightSeverity::Critical)
    } else if mem_pct >= 30.0 {
        Some(InsightSeverity::Warning)
    } else if mem_pct >= 20.0 {
        Some(InsightSeverity::Candidate)
    } else {
        None
    }
}

pub(crate) fn claims_memory_aggregate(provenance: &HookProvenance) -> bool {
    let HookProvenance::Builtin {
        producer_registration_ids,
    } = provenance
    else {
        return false;
    };
    !producer_registration_ids.is_empty()
        && producer_registration_ids
            .iter()
            .all(|id| matches!(id.as_str(), MEMORY_PRESSURE_HOOK | HIGH_MEMORY_PROCESS_HOOK))
}

fn provider_safe_identifier(value: &str) -> String {
    value
        .chars()
        .take(128)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "memory/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "memory/typed_facts_tests.rs"]
mod typed_facts_tests;
