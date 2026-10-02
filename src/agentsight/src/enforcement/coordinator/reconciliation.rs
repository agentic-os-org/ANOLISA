//! Desired-state reconciliation after an enforcer reconnects.

use std::collections::{HashMap, HashSet};

use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, ApplyPolicy, Binding, BindingState, ReplaceFailureCode, ReplaceOutcome,
    ReplacementSource,
};
use uuid::Uuid;

#[cfg(test)]
use super::transition::persist_outcome;
use super::transition::{ReplacementClient, execute_transition_fenced};
use super::{
    EnforcementClient, EnforcementCoordinatorError, EnforcementError, EnforcementStore,
    EnforcementStoreError,
};
use crate::enforcement::TransitionKey;

const MAX_OPERATOR_MESSAGE_CHARS: usize = 512;
const REJECTION_FALLBACK: &str = "enforcer rejected desired binding without operator-safe detail";

/// Operations used to reconcile one persisted desired-state generation.
pub(super) trait DesiredStateClient: ReplacementClient {
    /// Lists bindings currently acknowledged by the enforcer.
    fn bindings(&self) -> Result<Vec<Binding>, EnforcementError>;

    /// Applies one missing desired binding.
    fn apply(
        &self,
        request: ApplyPolicy,
        required_subscription_id: Uuid,
    ) -> Result<Binding, EnforcementError>;

    /// Applies one structured credential intent through the privileged adapter.
    fn apply_credential_policy(
        &self,
        request: ApplyCredentialPolicy,
        required_subscription_id: Uuid,
    ) -> Result<Binding, EnforcementError>;

    /// Detaches one actual binding that is not desired.
    fn detach(&self, binding_id: Uuid) -> Result<(), EnforcementError>;
}

impl DesiredStateClient for EnforcementClient {
    fn bindings(&self) -> Result<Vec<Binding>, EnforcementError> {
        EnforcementClient::bindings(self)
    }

    fn apply(
        &self,
        request: ApplyPolicy,
        required_subscription_id: Uuid,
    ) -> Result<Binding, EnforcementError> {
        EnforcementClient::apply(self, request, required_subscription_id)
    }

    fn apply_credential_policy(
        &self,
        request: ApplyCredentialPolicy,
        required_subscription_id: Uuid,
    ) -> Result<Binding, EnforcementError> {
        EnforcementClient::apply_credential_policy(self, request, required_subscription_id)
    }

    fn detach(&self, binding_id: Uuid) -> Result<(), EnforcementError> {
        EnforcementClient::detach(self, binding_id)
    }
}

/// Reconciles persisted desired bindings against one backend snapshot.
///
/// A typed remote apply rejection is terminal for only that binding. Transport,
/// protocol, response-correlation, and persistence errors abort the generation.
///
/// # Errors
///
/// Returns an enforcer or persistence error that invalidates the generation.
#[cfg(test)]
pub(super) fn reconcile_desired_state<C: DesiredStateClient + ?Sized>(
    client: &C,
    store: &EnforcementStore,
    required_subscription_id: Uuid,
) -> Result<(), EnforcementCoordinatorError> {
    reconcile_desired_state_fenced(
        client,
        store,
        required_subscription_id,
        |store, key, outcome| {
            persist_outcome(store, key, outcome)?;
            Ok(true)
        },
    )
}

pub(super) fn reconcile_desired_state_fenced<C, F>(
    client: &C,
    store: &EnforcementStore,
    required_subscription_id: Uuid,
    mut persist_if_current: F,
) -> Result<(), EnforcementCoordinatorError>
where
    C: DesiredStateClient + ?Sized,
    F: FnMut(
        &EnforcementStore,
        &TransitionKey,
        ReplaceOutcome,
    ) -> Result<bool, EnforcementCoordinatorError>,
{
    let mut reconciled_credential_ids = HashSet::new();
    for transition in store.pending_transitions()? {
        if let Some(binding_id) = reconcile_transition_fenced(
            client,
            store,
            transition,
            required_subscription_id,
            &mut persist_if_current,
        )? {
            reconciled_credential_ids.insert(binding_id);
        }
    }

    let credential_intents = store.credential_policy_intents()?;
    let credential_ids: HashSet<_> = credential_intents
        .iter()
        .map(|intent| intent.request.binding_id)
        .collect();
    let desired = store.bindings()?;
    let actual = client.bindings()?;
    let desired_by_id: HashMap<_, _> = desired
        .iter()
        .map(|binding| (binding.request.binding_id, binding))
        .collect();
    let active_credential_by_id: HashMap<_, _> = credential_intents
        .iter()
        .filter(|intent| {
            matches!(
                intent.state,
                BindingState::Pending | BindingState::Enforced | BindingState::Degraded
            )
        })
        .map(|intent| (intent.request.binding_id, intent))
        .collect();
    let mut retained_actual = HashMap::new();

    for binding in actual {
        let binding_id = binding.request.binding_id;
        let matches_generic_desired = desired_by_id.get(&binding_id).is_some_and(|desired| {
            is_active_desired(desired.state) && desired.request == binding.request
        });
        let matches_credential_desired =
            active_credential_by_id
                .get(&binding_id)
                .is_some_and(|intent| {
                    super::super::store::credential_binding_matches_request(
                        &intent.request,
                        &binding,
                    )
                });
        if matches_generic_desired || matches_credential_desired {
            retained_actual.insert(binding_id, binding);
        } else {
            client.detach(binding_id)?;
        }
    }

    for intent in active_credential_by_id
        .values()
        .filter(|intent| !reconciled_credential_ids.contains(&intent.request.binding_id))
    {
        persist_reconciled_credential_apply(
            store,
            &intent.request,
            client.apply_credential_policy(intent.request.clone(), required_subscription_id),
        )?;
    }

    for mut binding in desired {
        let binding_id = binding.request.binding_id;
        if is_active_desired(binding.state) {
            if credential_ids.contains(&binding_id) {
                continue;
            }
            match retained_actual.remove(&binding_id) {
                Some(actual) => store.upsert_binding(&actual)?,
                None => persist_reconciled_apply(
                    store,
                    binding.request.clone(),
                    client.apply(binding.request.clone(), required_subscription_id),
                )?,
            }
        } else if binding.state == BindingState::Detaching {
            binding.state = BindingState::Detached;
            binding.message = None;
            binding.domain_id = None;
            store.upsert_binding(&binding)?;
        }
    }
    Ok(())
}

fn reconcile_transition_fenced<C, F>(
    client: &C,
    store: &EnforcementStore,
    transition: crate::enforcement::PolicyTransition,
    required_subscription_id: Uuid,
    persist_if_current: &mut F,
) -> Result<Option<Uuid>, EnforcementCoordinatorError>
where
    C: DesiredStateClient + ?Sized,
    F: FnMut(
        &EnforcementStore,
        &TransitionKey,
        ReplaceOutcome,
    ) -> Result<bool, EnforcementCoordinatorError>,
{
    if transition.phase == crate::enforcement::TransitionPhase::Indeterminate {
        return Err(EnforcementCoordinatorError::TransitionUnavailable);
    }

    let actual = client.bindings()?;
    if let Some(target) = actual
        .iter()
        .find(|binding| transition.request.validate_acknowledgement(binding).is_ok())
    {
        if !persist_if_current(
            store,
            &transition.key,
            ReplaceOutcome::Applied(target.clone()),
        )? {
            store.mark_transition_indeterminate(
                &transition.key,
                ReplaceFailureCode::KernelFailure,
            )?;
            return Err(EnforcementCoordinatorError::TransitionUnavailable);
        }
        return Ok(None);
    }
    if actual.iter().any(|binding| {
        (binding.request.binding_id == transition.request.expected.request.binding_id
            && binding.request != transition.request.expected.request)
            || binding.request.binding_id == transition.request.replacement.binding_id()
    }) {
        store
            .mark_transition_indeterminate(&transition.key, ReplaceFailureCode::BindingConflict)?;
        return Err(EnforcementCoordinatorError::TransitionUnavailable);
    }

    let recovered_credential_id = match &transition.request.source {
        ReplacementSource::Credential(_) => Some(transition.request.expected.request.binding_id),
        ReplacementSource::Generic => None,
    };
    let source = match &transition.request.source {
        ReplacementSource::Generic => actual
            .iter()
            .find(|binding| {
                binding.state == BindingState::Enforced
                    && binding.domain_id.is_some()
                    && binding.request == transition.request.expected.request
            })
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| {
                client.apply(
                    transition.request.expected.request.clone(),
                    required_subscription_id,
                )
            }),
        ReplacementSource::Credential(snapshot) => {
            let request = credential_source_request(&transition, snapshot)?;
            client.apply_credential_policy(request, required_subscription_id)
        }
    }
    .map_err(|error| transition_source_error(store, &transition.key, error))?;

    if !source_matches_transition(&transition, &source) {
        store
            .mark_transition_indeterminate(&transition.key, ReplaceFailureCode::BindingConflict)?;
        return Err(EnforcementCoordinatorError::TransitionUnavailable);
    }
    let transition = store.reestablish_transition_source(&transition.key, &source)?;
    match execute_transition_fenced(
        client,
        store,
        transition.clone(),
        required_subscription_id,
        persist_if_current,
    ) {
        Ok(_) => Ok(recovered_credential_id),
        Err(EnforcementCoordinatorError::TransitionUnavailable) => {
            let retained = store
                .transition(&transition.key)?
                .is_some_and(|transition| proven_source_retained(&transition));
            if retained {
                Ok(recovered_credential_id)
            } else {
                Err(EnforcementCoordinatorError::TransitionUnavailable)
            }
        }
        Err(error) => Err(error),
    }
}

fn credential_source_request(
    transition: &crate::enforcement::PolicyTransition,
    snapshot: &agentsight_enforcement_protocol::CredentialPolicySnapshot,
) -> Result<ApplyCredentialPolicy, EnforcementCoordinatorError> {
    let expected = &transition.request.expected.request;
    let policy = snapshot.policy().map_err(|error| {
        EnforcementStoreError::InvalidCredentialPolicySnapshot {
            binding_id: expected.binding_id,
            reason: error.to_string(),
        }
    })?;
    Ok(ApplyCredentialPolicy {
        binding_id: expected.binding_id,
        agent_id: expected.agent_id.clone(),
        session_id: expected.session_id.clone(),
        root_pid: expected.root_pid,
        process_start_time: expected.process_start_time,
        policy: policy.clone(),
    })
}

fn source_matches_transition(
    transition: &crate::enforcement::PolicyTransition,
    source: &Binding,
) -> bool {
    match &transition.request.source {
        ReplacementSource::Generic => {
            source.state == BindingState::Enforced
                && source.domain_id.is_some()
                && source.request == transition.request.expected.request
        }
        ReplacementSource::Credential(snapshot) => snapshot.policy().is_ok_and(|policy| {
            let request = ApplyCredentialPolicy {
                binding_id: transition.request.expected.request.binding_id,
                agent_id: transition.request.expected.request.agent_id.clone(),
                session_id: transition.request.expected.request.session_id.clone(),
                root_pid: transition.request.expected.request.root_pid,
                process_start_time: transition.request.expected.request.process_start_time,
                policy: policy.clone(),
            };
            super::super::store::credential_binding_matches_request(&request, source)
        }),
    }
}

fn proven_source_retained(transition: &crate::enforcement::PolicyTransition) -> bool {
    transition.phase == crate::enforcement::TransitionPhase::Pending
        && transition.failure_code.is_some()
        && transition.acknowledgement.as_ref().is_some_and(|source| {
            source.state == BindingState::Enforced
                && source.request == transition.request.expected.request
                && source.domain_id == transition.request.expected.domain_id
        })
}

fn transition_source_error(
    store: &EnforcementStore,
    key: &TransitionKey,
    error: EnforcementError,
) -> EnforcementCoordinatorError {
    if let EnforcementError::Remote { code, .. } = &error
        && let Some(code) = replace_failure_code(code)
    {
        if let Err(store_error) = store.mark_transition_indeterminate(key, code) {
            return store_error.into();
        }
        return EnforcementCoordinatorError::TransitionUnavailable;
    }
    error.into()
}

fn replace_failure_code(code: &str) -> Option<ReplaceFailureCode> {
    match code {
        "binding_conflict" | "missing_binding" => Some(ReplaceFailureCode::BindingConflict),
        "stale_process" => Some(ReplaceFailureCode::StaleProcess),
        "compile_failure" => Some(ReplaceFailureCode::CompileFailure),
        "kernel_failure" => Some(ReplaceFailureCode::KernelFailure),
        _ => None,
    }
}

fn persist_reconciled_apply(
    store: &EnforcementStore,
    request: ApplyPolicy,
    result: Result<Binding, EnforcementError>,
) -> Result<(), EnforcementCoordinatorError> {
    match result {
        Ok(acknowledged) => store.upsert_binding(&acknowledged)?,
        Err(EnforcementError::Remote { code, message }) if is_binding_rejection(&code) => {
            store.upsert_binding(&remote_failure_binding(request, &code, &message))?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn persist_reconciled_credential_apply(
    store: &EnforcementStore,
    request: &ApplyCredentialPolicy,
    result: Result<Binding, EnforcementError>,
) -> Result<(), EnforcementCoordinatorError> {
    match result {
        Ok(acknowledged) => {
            if !super::super::store::credential_binding_matches_request(request, &acknowledged) {
                return Err(EnforcementStoreError::InvalidCredentialPolicySnapshot {
                    binding_id: request.binding_id,
                    reason: "compiled acknowledgement does not match structured intent".into(),
                }
                .into());
            }
            store.upsert_credential_binding(&acknowledged, &request.policy)?;
        }
        Err(EnforcementError::Remote { code, message }) if is_binding_rejection(&code) => {
            store.mark_credential_policy_intent_failed(
                request.binding_id,
                &remote_rejection_message(&code, &message),
            )?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Returns whether a remote code is a terminal desired-binding rejection.
pub(super) fn is_binding_rejection(code: &str) -> bool {
    matches!(
        code,
        "binding_conflict"
            | "missing_binding"
            | "stale_process"
            | "compile_failure"
            | "kernel_failure"
    )
}

/// Builds the bounded operator-safe failed state for a typed remote rejection.
pub(super) fn remote_failure_binding(request: ApplyPolicy, code: &str, message: &str) -> Binding {
    Binding {
        request,
        state: BindingState::Failed,
        message: Some(remote_rejection_message(code, message)),
        domain_id: None,
    }
}

/// Maps a remote rejection to bounded operator-safe persisted text.
pub(super) fn remote_rejection_message(code: &str, _message: &str) -> String {
    let operator_message = match code {
        "binding_conflict" => "enforcer rejected desired binding: binding conflict",
        "missing_binding" => "enforcer rejected desired binding: binding is missing",
        "stale_process" => "enforcer rejected desired binding: process identity is stale",
        "compile_failure" => "enforcer rejected desired binding: policy compilation failed",
        "kernel_failure" => "enforcer rejected desired binding: kernel attachment failed",
        _ => REJECTION_FALLBACK,
    };
    sanitize_operator_message(operator_message)
}

fn sanitize_operator_message(message: &str) -> String {
    let sanitized: String = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(MAX_OPERATOR_MESSAGE_CHARS)
        .collect();
    let sanitized = sanitized.trim();
    if sanitized.is_empty() {
        REJECTION_FALLBACK.into()
    } else {
        sanitized.into()
    }
}

fn is_active_desired(state: BindingState) -> bool {
    matches!(
        state,
        BindingState::Pending | BindingState::Enforced | BindingState::Degraded
    )
}

#[cfg(test)]
#[path = "reconciliation_tests.rs"]
mod tests;
