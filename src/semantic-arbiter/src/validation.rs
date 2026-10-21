use std::collections::HashSet;

use serde::Serialize;

use crate::contract::{
    Binding, Budget, Candidate, Evidence, EvidenceFact, EvidenceGap, Scenario, Task,
};
use crate::outcome::{
    CandidateControl, ObligationAction, Outcome, Receipt, ReceiptReason, ReceiptState,
};
use crate::protocol::{
    input_digest, CheckRequest, CheckResponse, ContractError, CHECK_API_VERSION,
    MAX_EVIDENCE_ITEMS, MAX_MESSAGE_BYTES,
};

fn require(value: bool, error: ContractError) -> Result<(), ContractError> {
    if value {
        Ok(())
    } else {
        Err(error)
    }
}

pub(crate) fn id(value: &str) -> Result<(), ContractError> {
    require(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-:@".contains(&b)),
        ContractError::InvalidInput,
    )
}

fn ids(values: &[String], nonempty: bool) -> Result<(), ContractError> {
    require(
        values.len() <= MAX_EVIDENCE_ITEMS && (!nonempty || !values.is_empty()),
        ContractError::InvalidInput,
    )?;
    let mut seen = HashSet::new();
    for value in values {
        id(value)?;
        require(seen.insert(value), ContractError::InvalidInput)?;
    }
    Ok(())
}

fn binding(value: &Binding) -> Result<(), ContractError> {
    for field in [
        &value.session_id,
        &value.instance_id,
        &value.workspace_id,
        &value.task_version,
        &value.config_revision,
        &value.candidate_id,
        &value.candidate_revision,
    ] {
        id(field)?;
    }
    Ok(())
}

pub(crate) fn bounded(value: &impl Serialize) -> Result<(), ContractError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ContractError::InvalidInput)?;
    require(
        bytes.len() <= MAX_MESSAGE_BYTES,
        ContractError::MessageTooLarge,
    )
}

fn version(value: &str) -> Result<(), ContractError> {
    require(
        value == CHECK_API_VERSION,
        ContractError::UnsupportedVersion,
    )
}

pub(crate) struct Input<'a> {
    pub request_id: &'a str,
    pub binding: &'a Binding,
    pub budget: &'a Budget,
    pub scenarios: &'a [Scenario],
    pub task: Option<&'a Task>,
    pub candidate: &'a Candidate,
    pub evidence: &'a [Evidence],
    pub gaps: &'a [EvidenceGap],
}

impl<'a> From<&'a CheckRequest> for Input<'a> {
    fn from(value: &'a CheckRequest) -> Self {
        Self {
            request_id: &value.request_id,
            binding: &value.binding,
            budget: &value.budget,
            scenarios: &value.scenarios,
            task: value.task.as_ref(),
            candidate: &value.candidate,
            evidence: &value.evidence,
            gaps: &value.gaps,
        }
    }
}

pub(crate) fn request(value: &CheckRequest) -> Result<(), ContractError> {
    version(&value.api_version)?;
    bounded(value)?;
    context(&Input::from(value), value.check_at_ms)
}

pub(crate) fn context(value: &Input<'_>, now_ms: u64) -> Result<(), ContractError> {
    id(value.request_id)?;
    binding(value.binding)?;
    id(&value.candidate.resource_id)?;
    require(
        !value.scenarios.is_empty()
            && value.scenarios.len() <= 2
            && !(value.scenarios.len() == 2 && value.scenarios[0] == value.scenarios[1]),
        ContractError::InvalidInput,
    )?;
    require(
        value.budget.deadline_ms > value.budget.opened_at_ms
            && now_ms >= value.budget.opened_at_ms
            && value.budget.max_evidence_items <= MAX_EVIDENCE_ITEMS
            && value.budget.max_evidence_bytes <= MAX_MESSAGE_BYTES
            && value.evidence.len() <= MAX_EVIDENCE_ITEMS
            && value.gaps.len() <= MAX_EVIDENCE_ITEMS,
        ContractError::InvalidInput,
    )?;
    if let Some(task) = &value.task {
        id(&task.version)?;
        id(&task.session_id)?;
        require(
            task.version == value.binding.task_version
                && task.session_id == value.binding.session_id,
            ContractError::BindingMismatch,
        )?;
        require(
            !task.text.trim().is_empty() && task.text.len() <= 16_384,
            ContractError::InvalidInput,
        )?;
        ids(&task.allowed_resource_ids, false)?;
        ids(&task.preserve_resource_ids, false)?;
    }
    let mut seen = HashSet::new();
    for evidence in value.evidence {
        id(&evidence.id)?;
        id(&evidence.source_id)?;
        id(&evidence.source_revision)?;
        require(seen.insert(&evidence.id), ContractError::InvalidInput)?;
        require(
            &evidence.binding == value.binding,
            ContractError::BindingMismatch,
        )?;
        require(
            evidence.captured_at_ms < evidence.valid_until_ms,
            ContractError::InvalidInput,
        )?;
        match &evidence.fact {
            EvidenceFact::Rebuild { resource_id, .. } => id(resource_id)?,
        }
    }
    let mut gaps = HashSet::new();
    for gap in value.gaps {
        id(&gap.source_id)?;
        require(
            gaps.insert((&gap.source_id, gap.scenario as u8)),
            ContractError::InvalidInput,
        )?;
    }
    Ok(())
}

fn refs(values: &[String], evidence: &[Evidence]) -> Result<(), ContractError> {
    ids(values, false)?;
    require(
        values
            .iter()
            .all(|id| evidence.iter().any(|item| &item.id == id)),
        ContractError::InvalidResponse,
    )
}

pub(crate) fn outcome(value: &Outcome, request: &CheckRequest) -> Result<(), ContractError> {
    outcome_for_input(value, &Input::from(request))
}

pub(crate) fn outcome_for_input(value: &Outcome, request: &Input<'_>) -> Result<(), ContractError> {
    bounded(value)?;
    refs(&value.evidence_refs, request.evidence)?;
    require(
        value.control.subject.len() <= 3 && value.obligations.len() <= MAX_EVIDENCE_ITEMS,
        ContractError::InvalidResponse,
    )?;
    let mut directive_ids = HashSet::new();
    directive_ids.insert("candidate-approval");
    let mut subjects = HashSet::new();
    for item in &value.control.subject {
        id(&item.id)?;
        require(
            directive_ids.insert(&item.id),
            ContractError::InvalidResponse,
        )?;
        require(
            item.instance_id == request.binding.instance_id,
            ContractError::BindingMismatch,
        )?;
        require(
            subjects.insert(item.action as u8),
            ContractError::InvalidResponse,
        )?;
        ids(&item.scope_ids, true)?;
    }
    let mut audits = 0;
    for item in &value.obligations {
        id(&item.id)?;
        require(
            directive_ids.insert(&item.id),
            ContractError::InvalidResponse,
        )?;
        match &item.action {
            ObligationAction::Audit {} => audits += 1,
            ObligationAction::Snapshot { workspace_id }
            | ObligationAction::Rollback { workspace_id, .. }
            | ObligationAction::Cleanup { workspace_id, .. } => {
                require(
                    workspace_id == &request.binding.workspace_id,
                    ContractError::BindingMismatch,
                )?;
            }
            ObligationAction::Notify { recipient_id } => id(recipient_id)?,
            ObligationAction::Diagnose { diagnostic_id } => id(diagnostic_id)?,
            ObligationAction::RetrieveSkill { query_id }
            | ObligationAction::RetrieveMemory { query_id } => id(query_id)?,
            ObligationAction::InjectContext { material_ids } => ids(material_ids, true)?,
        }
        match &item.action {
            ObligationAction::Rollback { checkpoint_id, .. } => id(checkpoint_id)?,
            ObligationAction::Cleanup { checkpoint_ids, .. } => ids(checkpoint_ids, true)?,
            _ => {}
        }
    }
    let needs_audit = value.control.candidate != CandidateControl::Nop
        || !value.control.subject.is_empty()
        || !value.obligations.is_empty()
        || !value.evidence_refs.is_empty()
        || request
            .gaps
            .iter()
            .any(|gap| request.scenarios.contains(&gap.scenario));
    require(
        audits == usize::from(needs_audit),
        ContractError::InvalidResponse,
    )
}

pub(crate) fn response(
    value: &CheckResponse,
    original: &CheckRequest,
) -> Result<(), ContractError> {
    request(original)?;
    version(&value.api_version)?;
    bounded(value)?;
    require(
        value.request_id == original.request_id
            && value.binding == original.binding
            && value.input_digest == input_digest(original)?
            && value.mode == original.mode,
        ContractError::BindingMismatch,
    )?;
    require(
        value.answers.len() == original.scenarios.len(),
        ContractError::InvalidResponse,
    )?;
    for answer in &value.answers {
        require(
            answer.confidence_ppm <= 1_000_000,
            ContractError::InvalidResponse,
        )?;
        require(
            original.scenarios.contains(&answer.unit),
            ContractError::InvalidResponse,
        )?;
        refs(&answer.evidence_refs, &original.evidence)?;
    }
    outcome(&value.outcome, original)?;
    let expected = crate::check::check(original)?;
    require(
        value.answers == expected.answers,
        ContractError::InvalidResponse,
    )?;
    require(
        value.outcome == expected.outcome,
        ContractError::InvalidResponse,
    )
}

pub(crate) fn receipt(
    value: &Receipt,
    original: &CheckRequest,
    original_outcome: &Outcome,
) -> Result<(), ContractError> {
    request(original)?;
    outcome(original_outcome, original)?;
    version(&value.api_version)?;
    bounded(value)?;
    require(
        value.request_id == original.request_id && value.binding == original.binding,
        ContractError::BindingMismatch,
    )?;
    require(
        original_outcome
            .directives(&original.binding)
            .iter()
            .any(|item| item.id() == value.directive_id),
        ContractError::BindingMismatch,
    )?;
    require(
        matches!(
            (value.state, value.reason),
            (ReceiptState::Applied, ReceiptReason::Confirmed)
                | (ReceiptState::Rejected, ReceiptReason::Denied)
                | (ReceiptState::Unknown, ReceiptReason::Unconfirmed)
        ),
        ContractError::InvalidResponse,
    )
}
