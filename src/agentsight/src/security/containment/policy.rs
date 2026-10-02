//! Strict binding, target, and acknowledgement checks for containment trust decisions.

use std::collections::HashSet;

use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, Binding, BindingState, CredentialExfiltrationPolicy,
    CredentialPolicySnapshot, DestinationScope, PolicyMode, SecurityEventKind,
};
use uuid::Uuid;

use super::{
    ContainmentAction, ContainmentCandidate, ContainmentError, ContainmentLifecycle,
    ContainmentRequest, MAX_DURATION_SECS, MIN_DURATION_SECS,
};
use crate::enforcement::read_process_start_time;
use crate::security::RiskCaseDetail;

const AGENT_SOURCE: &str = "source AGENT = exec \"**\"";
const CREDENTIAL_SOURCE: &str = "source CREDENTIAL = file \"";
const RULE: &str = "rule agentsight-credential-exfiltration:";
const SINK: &str = " connect endpoint \"*\" if CREDENTIAL";
const TRUSTED: &str = " unless target \"";
const REASON: &str = "  because \"credential-derived data reached an untrusted network target\"";

pub(super) fn validate_process_identity(
    pid: i32,
    expected_start_time: u64,
) -> Result<u64, ContainmentError> {
    match read_process_start_time(pid) {
        Ok(actual) if actual == expected_start_time => Ok(actual),
        Ok(_) | Err(_) => Err(ContainmentError::RootProcessStale(pid)),
    }
}

pub(super) struct ResolvedPolicy {
    pub(super) detail: RiskCaseDetail,
    pub(super) binding: Binding,
    pub(super) source_path: String,
    pub(super) source_policy_snapshot: CredentialPolicySnapshot,
}

pub(super) fn exact_binding(bindings: &[Binding], binding_id: Uuid) -> Result<Option<Binding>, ()> {
    let mut matching = bindings
        .iter()
        .filter(|binding| binding.request.binding_id == binding_id);
    let first = matching.next().cloned();
    if matching.next().is_some() {
        return Err(());
    }
    Ok(first)
}

pub(super) fn resolve_policy(
    detail: RiskCaseDetail,
    bindings: Vec<Binding>,
    source_policy_snapshot: Option<CredentialPolicySnapshot>,
) -> Option<ResolvedPolicy> {
    let source_binding_id = detail.evidence.first()?.identity.binding_id;
    resolve_policy_binding(
        detail,
        bindings,
        source_binding_id,
        source_policy_snapshot,
        false,
    )
}

pub(super) fn resolve_transition_policy(
    detail: RiskCaseDetail,
    bindings: Vec<Binding>,
    source_binding_id: Uuid,
    source_policy_snapshot: Option<CredentialPolicySnapshot>,
) -> Option<ResolvedPolicy> {
    resolve_policy_binding(
        detail,
        bindings,
        source_binding_id,
        source_policy_snapshot,
        true,
    )
}

fn resolve_policy_binding(
    detail: RiskCaseDetail,
    bindings: Vec<Binding>,
    source_binding_id: Uuid,
    source_policy_snapshot: Option<CredentialPolicySnapshot>,
    allow_detached: bool,
) -> Option<ResolvedPolicy> {
    let evidence = detail.evidence.first()?;
    if evidence.identity.binding_id != source_binding_id {
        return None;
    }
    let file_action = match &evidence.kind {
        SecurityEventKind::FileAction(action) => action,
        _ => return None,
    };
    let mut matching = bindings
        .into_iter()
        .filter(|binding| binding.request.binding_id == source_binding_id);
    let binding = matching.next().filter(|_| matching.next().is_none())?;
    let request = &binding.request;
    if !(binding.state == BindingState::Enforced
        || (allow_detached && binding.state == BindingState::Detached))
        || request.agent_id != detail.case.agent_id
        || request.agent_id != evidence.identity.agent_id
        || request.session_id != detail.case.session_id
        || request.session_id != evidence.identity.session_id
        || request.policy_id != detail.case.policy_id
        || request.policy_id != file_action.policy_id
        || request.policy_revision != detail.case.policy_revision.to_string()
        || file_action.policy_revision != detail.case.policy_revision
    {
        return None;
    }
    let compiled = parse_compiled_policy(&request.policy_dsl, CompiledMode::Audit)?;
    let source_policy_snapshot = source_policy_snapshot?;
    let source_policy = source_policy_snapshot.policy().ok()?;
    if source_policy.mode != PolicyMode::Audit
        || source_policy.policy_id != request.policy_id
        || source_policy.revision.to_string() != request.policy_revision
        || source_policy.taint_label != "CREDENTIAL"
        || source_policy.source_patterns
            != compiled
                .source_paths
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .as_slice()
        || source_policy.trusted_endpoints != compiled.trusted_endpoints
    {
        return None;
    }
    Some(ResolvedPolicy {
        detail,
        binding,
        source_path: compiled.source_paths.first().cloned().unwrap_or_default(),
        source_policy_snapshot,
    })
}

pub(super) fn enforce_request(
    context: &ResolvedPolicy,
    binding_id: Uuid,
    root_pid: i32,
    process_start_time: u64,
) -> Option<ApplyCredentialPolicy> {
    let policy = credential_policy(context, PolicyMode::Enforce)?;
    Some(ApplyCredentialPolicy {
        binding_id,
        agent_id: context.binding.request.agent_id.clone(),
        session_id: context.binding.request.session_id.clone(),
        root_pid,
        process_start_time,
        policy,
    })
}

pub(super) fn source_policy_snapshot(context: &ResolvedPolicy) -> CredentialPolicySnapshot {
    context.source_policy_snapshot.clone()
}

fn credential_policy(
    context: &ResolvedPolicy,
    mode: PolicyMode,
) -> Option<CredentialExfiltrationPolicy> {
    let mut policy = context.source_policy_snapshot.policy().ok()?.clone();
    policy.mode = mode;
    policy.validate().ok()?;
    Some(policy)
}

pub(super) fn live_candidates(
    agent_id: &str,
    candidates: Vec<ContainmentCandidate>,
) -> Result<Vec<ContainmentCandidate>, ContainmentError> {
    let mut seen = HashSet::new();
    let mut live = Vec::new();
    for candidate in candidates {
        if candidate.agent_id != agent_id {
            continue;
        }
        if !seen.insert(candidate.root_pid) {
            return Err(ContainmentError::AmbiguousCandidate(candidate.root_pid));
        }
        if read_process_start_time(candidate.root_pid)
            .is_ok_and(|actual| actual == candidate.process_start_time)
        {
            live.push(candidate);
        }
    }
    Ok(live)
}

pub(super) fn select_candidate(
    agent_id: &str,
    root_pid: i32,
    candidates: &[ContainmentCandidate],
) -> Result<ContainmentCandidate, ContainmentError> {
    live_candidates(agent_id, candidates.to_vec())?
        .into_iter()
        .find(|candidate| candidate.root_pid == root_pid)
        .ok_or(ContainmentError::RootProcessStale(root_pid))
}

pub(super) fn acknowledgement_matches(binding: &Binding, expected: &ApplyCredentialPolicy) -> bool {
    let request = &binding.request;
    let expected_source = match expected.policy.source_patterns.as_slice() {
        [source] => source,
        _ => return false,
    };
    if expected.policy.taint_label != "CREDENTIAL"
        || expected.policy.destination_scope != DestinationScope::PublicIpv4
        || expected.policy.mode != PolicyMode::Enforce
        || expected.policy.trusted_endpoints.len() > 1
        || binding.state != BindingState::Enforced
        || request.binding_id != expected.binding_id
        || request.agent_id != expected.agent_id
        || request.session_id != expected.session_id
        || request.root_pid != expected.root_pid
        || request.process_start_time != expected.process_start_time
        || request.policy_id != expected.policy.policy_id
        || request.policy_revision != expected.policy.revision.to_string()
    {
        return false;
    }
    parse_compiled_policy(&request.policy_dsl, CompiledMode::Enforce).is_some_and(|compiled| {
        compiled.source_paths.contains(&expected_source.to_string())
            && compiled.trusted_endpoints == expected.policy.trusted_endpoints
    })
}

pub(super) fn live_lifecycle(lifecycle: ContainmentLifecycle) -> bool {
    matches!(
        lifecycle,
        ContainmentLifecycle::Pending
            | ContainmentLifecycle::Active
            | ContainmentLifecycle::Expiring
    )
}

pub(super) fn existing_action(
    existing: ContainmentAction,
    request: &ContainmentRequest,
    process_start_time: u64,
) -> Result<ContainmentAction, ContainmentError> {
    if existing.root_pid != request.root_pid
        || existing.process_start_time != process_start_time
        || existing.duration_secs != request.duration_secs
    {
        return Err(ContainmentError::IncompatibleAction(existing.action_id));
    }
    match existing.lifecycle_state {
        ContainmentLifecycle::Active => Ok(existing),
        ContainmentLifecycle::Pending => {
            Err(ContainmentError::ContainmentInProgress(existing.action_id))
        }
        ContainmentLifecycle::Expiring => {
            Err(ContainmentError::ContainmentExpiring(existing.action_id))
        }
        ContainmentLifecycle::Expired | ContainmentLifecycle::Failed => {
            Err(ContainmentError::IncompatibleAction(existing.action_id))
        }
    }
}

pub(super) fn validate_duration(duration_secs: Option<u64>) -> Result<(), ContainmentError> {
    if duration_secs.is_some_and(|value| !(MIN_DURATION_SECS..=MAX_DURATION_SECS).contains(&value))
    {
        return Err(ContainmentError::InvalidDuration);
    }
    Ok(())
}

pub(super) fn validate_requested_by(requested_by: &str) -> Result<String, ContainmentError> {
    if requested_by.len() > 128 || requested_by.chars().any(char::is_control) {
        return Err(ContainmentError::InvalidRequestedBy);
    }
    let requested_by = requested_by.trim();
    if requested_by.is_empty() {
        return Err(ContainmentError::InvalidRequestedBy);
    }
    Ok(requested_by.to_string())
}

pub(super) fn sanitize_failure(message: &str) -> String {
    let sanitized: String = message
        .chars()
        .filter(|character| !character.is_control())
        .take(512)
        .collect();
    if sanitized.is_empty() {
        "enforcer operation failed without detail".into()
    } else {
        sanitized
    }
}

#[derive(Clone, Copy)]
enum CompiledMode {
    Audit,
    Enforce,
}

struct CompiledPolicy {
    source_paths: Vec<String>,
    trusted_endpoints: Vec<String>,
}

fn parse_compiled_policy(dsl: &str, mode: CompiledMode) -> Option<CompiledPolicy> {
    let body = dsl.strip_suffix('\n')?;
    let lines: Vec<_> = body.split('\n').collect();
    // Minimum structure: AGENT_SOURCE, at least one source, RULE, action, REASON.
    if lines.len() < 5 || lines[0] != AGENT_SOURCE {
        return None;
    }
    // Collect source lines (all lines between AGENT_SOURCE and RULE).
    let mut source_paths = Vec::new();
    let mut rule_idx = None;
    for (index, line) in lines.iter().enumerate().skip(1) {
        if *line == RULE {
            rule_idx = Some(index);
            break;
        }
        let path = line.strip_prefix(CREDENTIAL_SOURCE)?.strip_suffix('"')?;
        if !valid_source_path(path) {
            return None;
        }
        source_paths.push(path.to_string());
    }
    let rule_idx = rule_idx?;
    if source_paths.is_empty() {
        return None;
    }
    // After RULE: action line, then REASON.
    let action_idx = rule_idx + 1;
    let reason_idx = action_idx + 1;
    if reason_idx >= lines.len() || lines[reason_idx] != REASON {
        return None;
    }
    let action = match mode {
        CompiledMode::Audit => "  notify",
        CompiledMode::Enforce => "  block",
    };
    let mut suffix = lines[action_idx].strip_prefix(action)?.strip_prefix(SINK)?;
    // Strip optional ` expires Ns` tail (from legacy enforce bindings).
    if let Some(pos) = suffix.find(" expires ") {
        suffix = &suffix[..pos];
    }
    let trusted_endpoints = if suffix.is_empty() {
        Vec::new()
    } else {
        let endpoint = suffix.strip_prefix(TRUSTED)?.strip_suffix('"')?;
        if !valid_literal(endpoint) {
            return None;
        }
        vec![endpoint.to_string()]
    };
    Some(CompiledPolicy {
        source_paths,
        trusted_endpoints,
    })
}

fn valid_source_path(value: &str) -> bool {
    if !valid_literal(value) || !value.starts_with('/') || value.contains('\u{fffd}') {
        return false;
    }
    let mut segments = value.split('/');
    segments.next() == Some("")
        && segments.clone().next().is_some()
        && segments.all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn valid_literal(value: &str) -> bool {
    !value.is_empty()
        && value.len() < 127
        && !value
            .chars()
            .any(|character| character == '"' || character == '\\' || character.is_control())
}
