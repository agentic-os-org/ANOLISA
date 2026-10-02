//! Coordinator boundary for durable policy ownership replacement.

use agentsight_enforcement_protocol::{ReplaceOutcome, ReplacePolicy};
use uuid::Uuid;

use super::{
    EnforcementClient, EnforcementCoordinator, EnforcementCoordinatorError, EnforcementError,
    EnforcementStore, INGESTION_UNAVAILABLE_MESSAGE, combine_health, unavailable_from_health,
};
use crate::enforcement::{PolicyTransition, TransitionKey, TransitionPhase};

/// Replacement operation used by live and scripted reconciliation clients.
pub(super) trait ReplacementClient {
    fn replace(
        &self,
        request: ReplacePolicy,
        required_subscription_id: Uuid,
    ) -> Result<ReplaceOutcome, EnforcementError>;
}

impl ReplacementClient for EnforcementClient {
    fn replace(
        &self,
        request: ReplacePolicy,
        required_subscription_id: Uuid,
    ) -> Result<ReplaceOutcome, EnforcementError> {
        EnforcementClient::replace(self, request, required_subscription_id)
    }
}

impl EnforcementCoordinator {
    /// Persists a new transition before asking the enforcer to replace policy ownership.
    ///
    /// # Errors
    ///
    /// Returns when ingestion is unavailable, persistence fails, transport is
    /// lost, or the enforcer cannot prove one exact owner.
    pub fn begin_transition(
        &self,
        key: TransitionKey,
        request: ReplacePolicy,
    ) -> Result<PolicyTransition, EnforcementCoordinatorError> {
        let _lifecycle = self.lifecycle();
        let transition = self
            .store
            .begin_transition(&PolicyTransition::pending(key, request))?;
        self.execute_live_transition(transition)
    }

    /// Resumes the exact persisted transition without accepting new desired state.
    ///
    /// # Errors
    ///
    /// Returns a missing-transition, persistence, transport, or ownership error.
    pub fn resume_transition(
        &self,
        key: &TransitionKey,
    ) -> Result<PolicyTransition, EnforcementCoordinatorError> {
        let _lifecycle = self.lifecycle();
        let transition =
            self.store
                .transition(key)?
                .ok_or(super::EnforcementStoreError::MissingTransition(
                    key.action_id,
                ))?;
        self.execute_live_transition(transition)
    }

    /// Restores the source audit policy recorded by a completed forward transition.
    ///
    /// # Errors
    ///
    /// Returns when the forward transition is missing or incomplete, ingestion
    /// is unavailable, persistence fails, or runtime ownership cannot be proved.
    pub fn begin_reverse_transition(
        &self,
        action_id: Uuid,
    ) -> Result<PolicyTransition, EnforcementCoordinatorError> {
        let _lifecycle = self.lifecycle();
        let forward_key = TransitionKey {
            action_id,
            direction: crate::enforcement::TransitionDirection::Forward,
        };
        let forward = self
            .store
            .transition(&forward_key)?
            .ok_or(super::EnforcementStoreError::MissingTransition(action_id))?;
        if forward.phase != TransitionPhase::Completed {
            return Err(super::EnforcementStoreError::TransitionConflict(action_id).into());
        }
        let expected = forward.acknowledgement.ok_or_else(|| {
            super::EnforcementStoreError::InvalidTransitionState {
                field: "acknowledgement",
                value: "missing completed forward acknowledgement".into(),
            }
        })?;
        let reverse = PolicyTransition::pending(
            TransitionKey {
                action_id,
                direction: crate::enforcement::TransitionDirection::Reverse,
            },
            forward.request.reverse(expected),
        );
        let transition = self.store.begin_transition(&reverse)?;
        self.execute_live_transition(transition)
    }

    fn execute_live_transition(
        &self,
        transition: PolicyTransition,
    ) -> Result<PolicyTransition, EnforcementCoordinatorError> {
        if is_terminal(&transition) {
            return Ok(transition);
        }
        let lease = self
            .ingestion_readiness
            .lease()
            .ok_or(EnforcementCoordinatorError::IngestionUnavailable)?;
        let health = combine_health(self.client.health()?, &self.ingestion_readiness);
        if !health.ready {
            return Err(unavailable_from_health(health));
        }
        let outcome = match self
            .client
            .replace(transition.request.clone(), lease.subscription_id)
        {
            Ok(outcome) => outcome,
            Err(EnforcementError::Remote { code, message })
                if code == "required_subscription_unavailable" =>
            {
                log::error!("required violation subscription unavailable: {message}");
                self.ingestion_readiness
                    .invalidate_lease(&lease, INGESTION_UNAVAILABLE_MESSAGE.into());
                return Err(EnforcementCoordinatorError::EnforcementUnavailable(
                    INGESTION_UNAVAILABLE_MESSAGE.into(),
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let post_health = combine_health(self.client.health()?, &self.ingestion_readiness);
        if !post_health.ready {
            self.store
                .mark_transition_indeterminate(&transition.key, outcome.failure_code_or_kernel())?;
            return Err(unavailable_from_health(post_health));
        }
        let key = transition.key.clone();
        let committed = self.ingestion_readiness.commit_if_current(&lease, || {
            persist_outcome(&self.store, &key, outcome.clone())
        })?;
        if !committed {
            self.store
                .mark_transition_indeterminate(&transition.key, outcome.failure_code_or_kernel())?;
            return Err(EnforcementCoordinatorError::TransitionUnavailable);
        }
        transition_result(&self.store, &transition.key)
    }
}

#[cfg(test)]
pub(super) fn execute_transition<C: ReplacementClient + ?Sized>(
    client: &C,
    store: &EnforcementStore,
    transition: PolicyTransition,
    required_subscription_id: Uuid,
) -> Result<PolicyTransition, EnforcementCoordinatorError> {
    execute_transition_fenced(
        client,
        store,
        transition,
        required_subscription_id,
        &mut |store, key, outcome| {
            persist_outcome(store, key, outcome)?;
            Ok(true)
        },
    )
}

pub(super) fn execute_transition_fenced<C, F>(
    client: &C,
    store: &EnforcementStore,
    transition: PolicyTransition,
    required_subscription_id: Uuid,
    persist_if_current: &mut F,
) -> Result<PolicyTransition, EnforcementCoordinatorError>
where
    C: ReplacementClient + ?Sized,
    F: FnMut(
        &EnforcementStore,
        &TransitionKey,
        ReplaceOutcome,
    ) -> Result<bool, EnforcementCoordinatorError>,
{
    if is_terminal(&transition) {
        return Ok(transition);
    }
    let key = transition.key.clone();
    let outcome = client.replace(transition.request, required_subscription_id)?;
    let failure_code = outcome.failure_code_or_kernel();
    if !persist_if_current(store, &key, outcome)? {
        store.mark_transition_indeterminate(&key, failure_code)?;
        return Err(EnforcementCoordinatorError::TransitionUnavailable);
    }
    transition_result(store, &key)
}

pub(super) fn persist_outcome(
    store: &EnforcementStore,
    key: &TransitionKey,
    outcome: ReplaceOutcome,
) -> Result<(), EnforcementCoordinatorError> {
    match outcome {
        ReplaceOutcome::Applied(binding) => store.complete_transition(key, &binding)?,
        ReplaceOutcome::SourceRetained { binding, code } => {
            store.retain_transition(key, &binding, code)?;
        }
        ReplaceOutcome::SourceRestored { binding, code } => {
            if key.direction == crate::enforcement::TransitionDirection::Forward {
                store.restore_transition(key, &binding, code)?;
            } else {
                store.retain_transition(key, &binding, code)?;
            }
        }
        ReplaceOutcome::Conflict { code } | ReplaceOutcome::Indeterminate { code } => {
            store.mark_transition_indeterminate(key, code)?;
            return Err(EnforcementCoordinatorError::TransitionUnavailable);
        }
    }
    Ok(())
}

fn transition_result(
    store: &EnforcementStore,
    key: &TransitionKey,
) -> Result<PolicyTransition, EnforcementCoordinatorError> {
    let transition =
        store
            .transition(key)?
            .ok_or(super::EnforcementStoreError::MissingTransition(
                key.action_id,
            ))?;
    if is_terminal(&transition) {
        Ok(transition)
    } else {
        Err(EnforcementCoordinatorError::TransitionUnavailable)
    }
}

fn is_terminal(transition: &PolicyTransition) -> bool {
    transition.phase == TransitionPhase::Completed
        || (transition.key.direction == crate::enforcement::TransitionDirection::Forward
            && transition.phase == TransitionPhase::SourceRestored)
}

trait OutcomeFailureCode {
    fn failure_code_or_kernel(&self) -> agentsight_enforcement_protocol::ReplaceFailureCode;
}

impl OutcomeFailureCode for ReplaceOutcome {
    fn failure_code_or_kernel(&self) -> agentsight_enforcement_protocol::ReplaceFailureCode {
        match self {
            ReplaceOutcome::SourceRetained { code, .. }
            | ReplaceOutcome::SourceRestored { code, .. }
            | ReplaceOutcome::Conflict { code }
            | ReplaceOutcome::Indeterminate { code } => *code,
            ReplaceOutcome::Applied(_) => {
                agentsight_enforcement_protocol::ReplaceFailureCode::KernelFailure
            }
        }
    }
}
