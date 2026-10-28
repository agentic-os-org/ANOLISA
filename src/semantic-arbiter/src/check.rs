//! Deterministic preservation and task-scope checking, with no model, filesystem or network access.

use crate::contract::{
    AnswerValue, EvidenceFact, Provenance, Reason, RebuildResult, Scenario, UnitAnswer,
};
use crate::outcome::{blocked_reason, compose, CandidateControl};
use crate::protocol::{
    input_digest, validate_request, CheckRequest, CheckResponse, ContractError, CHECK_API_VERSION,
};

/// Check a frozen synthetic request; a valid block is a successful judgment.
pub fn check(request: &CheckRequest) -> Result<CheckResponse, ContractError> {
    validate_request(request)?;
    let answers = evaluate_answers(request)?;
    let controls = answers.iter().map(|answer| {
        if blocked_reason(answer.reason) {
            CandidateControl::Block
        } else {
            CandidateControl::Allow
        }
    });
    let outcome = compose(&answers, &request.binding, controls);
    Ok(CheckResponse {
        api_version: CHECK_API_VERSION.into(),
        request_id: request.request_id.clone(),
        mode: request.mode,
        binding: request.binding.clone(),
        input_digest: input_digest(request)?,
        answers,
        outcome,
    })
}

pub(crate) fn evaluate_answers(request: &CheckRequest) -> Result<Vec<UnitAnswer>, ContractError> {
    reference_answers(
        &crate::validation::Input::from(request),
        request.check_at_ms,
    )
}

pub(crate) fn reference_answers(
    request: &crate::validation::Input<'_>,
    now_ms: u64,
) -> Result<Vec<UnitAnswer>, ContractError> {
    let evidence_bytes = serde_json::to_vec(&request.evidence)
        .map_err(|_| ContractError::InvalidInput)?
        .len();
    let failure = if now_ms >= request.budget.deadline_ms {
        Some(Reason::BudgetExceeded)
    } else if request.evidence.len() > request.budget.max_evidence_items
        || evidence_bytes > request.budget.max_evidence_bytes
    {
        Some(Reason::EvidenceBudgetExceeded)
    } else {
        None
    };
    Ok([Scenario::Preservation, Scenario::TaskScope]
        .into_iter()
        .filter(|unit| request.scenarios.contains(unit))
        .map(|unit| {
            let (reason, evidence_refs) = match failure {
                Some(reason) => (reason, vec![]),
                None => evaluate_unit(request, now_ms, unit),
            };
            let value = match reason {
                Reason::Allowed => AnswerValue::Allow,
                Reason::Preserved => AnswerValue::Preserve,
                Reason::NeedsSnapshot => AnswerValue::NeedsSnapshot,
                Reason::OutsideScope => AnswerValue::OutsideScope,
                _ => AnswerValue::Unknown,
            };
            UnitAnswer {
                unit,
                value,
                confidence_ppm: if value == AnswerValue::Unknown {
                    0
                } else {
                    1_000_000
                },
                reason,
                evidence_refs,
            }
        })
        .collect())
}

fn evaluate_unit(
    request: &crate::validation::Input<'_>,
    now_ms: u64,
    unit: Scenario,
) -> (Reason, Vec<String>) {
    let Some(task) = &request.task else {
        return (Reason::MissingTask, vec![]);
    };
    let resource = &request.candidate.resource_id;
    if task.preserve_resource_ids.contains(resource) {
        return (Reason::Preserved, vec![]);
    }
    // An omitted allowance declares no ceiling, so scope is left to the semantic
    // units instead of denying every resource an operator did not enumerate. A
    // declared allowance keeps excluding each resource it omits.
    if !task.allowed_resource_ids.is_empty() && !task.allowed_resource_ids.contains(resource) {
        return (Reason::OutsideScope, vec![]);
    }
    if request.gaps.iter().any(|gap| gap.scenario == unit) {
        return (Reason::EvidenceGap, vec![]);
    }
    if unit == Scenario::TaskScope {
        return (Reason::Allowed, vec![]);
    }
    let matching: Vec<_> = request
        .evidence
        .iter()
        .filter(|evidence| match &evidence.fact {
            EvidenceFact::Rebuild { resource_id, .. } => resource_id == resource,
        })
        .collect();
    if matching.is_empty() {
        return (Reason::MissingEvidence, vec![]);
    }
    let qualified: Vec<_> = matching
        .into_iter()
        .filter(|evidence| evidence.provenance == Provenance::LocalVerifier)
        .collect();
    if qualified.is_empty() {
        return (Reason::UntrustedEvidence, vec![]);
    }
    let mut refs: Vec<_> = qualified.iter().map(|e| e.id.clone()).collect();
    refs.sort();
    if qualified
        .iter()
        .any(|evidence| evidence.captured_at_ms > now_ms || evidence.valid_until_ms <= now_ms)
    {
        return (Reason::StaleEvidence, refs);
    }
    let results: Vec<_> = qualified
        .iter()
        .map(|evidence| match evidence.fact {
            EvidenceFact::Rebuild { result, .. } => result,
        })
        .collect();
    if results.contains(&RebuildResult::Identical) && results.contains(&RebuildResult::Unique) {
        (Reason::ConflictingEvidence, refs)
    } else if results.contains(&RebuildResult::Unknown) {
        (Reason::Undetermined, refs)
    } else if results.contains(&RebuildResult::Unique) {
        (Reason::NeedsSnapshot, refs)
    } else {
        (Reason::Allowed, refs)
    }
}
