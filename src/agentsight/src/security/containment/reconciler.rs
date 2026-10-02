//! Bounded restart recovery and expiry handling for containment actions.

mod expiry;

use agentsight_enforcement_protocol::{
    BindingState, ReplacePolicy, ReplacementPolicy, ReplacementSource,
};

use crate::enforcement::{TransitionDirection, TransitionKey};
use crate::security::store::DueContainmentAction;

use super::pending::record_pending_unavailable;
use super::{
    ContainmentAction, ContainmentActivationResult, ContainmentCoordinator, ContainmentEnforcer,
    ContainmentEnforcerError, ContainmentError, ContainmentFailureStage, ContainmentLifecycle,
    RiskCaseStatus, SecurityStore, SecurityStoreError, acknowledgement_matches, enforce_request,
    exact_binding, resolve_transition_policy, sanitize_failure, source_policy_snapshot,
    validate_process_identity,
};

const DUE_BATCH_LIMIT: usize = 100;

impl ContainmentCoordinator {
    /// Reconciles at most one bounded batch of due persisted actions.
    ///
    /// # Errors
    ///
    /// Returns the first typed store, enforcer, or recovery failure after
    /// continuing to process the rest of the fetched batch.
    pub fn reconcile_once(&self, current_time_ns: u64) -> Result<(), ContainmentError> {
        reconcile_batch(&self.store, self.enforcer.as_ref(), current_time_ns)
    }

    pub(super) fn reconcile_action(
        &self,
        action_id: uuid::Uuid,
        current_time_ns: u64,
    ) -> Result<(), ContainmentError> {
        Reconciler {
            store: &self.store,
            enforcer: self.enforcer.as_ref(),
        }
        .reconcile_action(action_id, current_time_ns)
    }
}

pub(super) fn reconcile_batch(
    store: &SecurityStore,
    enforcer: &dyn ContainmentEnforcer,
    current_time_ns: u64,
) -> Result<(), ContainmentError> {
    Reconciler { store, enforcer }.reconcile_once(current_time_ns)
}

struct Reconciler<'a> {
    store: &'a SecurityStore,
    enforcer: &'a dyn ContainmentEnforcer,
}

impl Reconciler<'_> {
    fn reconcile_action(
        &self,
        action_id: uuid::Uuid,
        current_time_ns: u64,
    ) -> Result<(), ContainmentError> {
        let action = self.store.containment_action(action_id)?.ok_or_else(|| {
            SecurityStoreError::InvalidData(format!(
                "containment action {action_id} disappeared during targeted reconciliation"
            ))
        })?;
        let Some(claimed) = self
            .store
            .claim_containment_reconciliation(&action, current_time_ns)?
        else {
            return Err(ContainmentError::ClaimLost(action_id));
        };
        self.reconcile_claimed(claimed, current_time_ns)
    }

    fn reconcile_once(&self, current_time_ns: u64) -> Result<(), ContainmentError> {
        let candidates = self
            .store
            .due_containment_candidates(current_time_ns, DUE_BATCH_LIMIT)?;
        let mut first_error = None;
        let mut corrupt_count = 0;
        for candidate in candidates {
            let action = match candidate {
                DueContainmentAction::Valid(action) => *action,
                DueContainmentAction::Corrupt { action_key, reason } => {
                    corrupt_count += 1;
                    if let Err(error) = self.store.quarantine_containment_action(
                        &action_key,
                        &reason,
                        current_time_ns,
                    ) && first_error.is_none()
                    {
                        first_error = Some(error.into());
                    }
                    continue;
                }
            };
            let result = self
                .store
                .claim_containment_reconciliation(&action, current_time_ns)
                .map_err(ContainmentError::from)
                .and_then(|claimed| match claimed {
                    Some(claimed) => self.reconcile_claimed(claimed, current_time_ns),
                    None => Ok(()),
                });
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        if corrupt_count > 0 {
            return Err(ContainmentError::CorruptActions {
                count: corrupt_count,
            });
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn reconcile_claimed(
        &self,
        action: ContainmentAction,
        current_time_ns: u64,
    ) -> Result<(), ContainmentError> {
        match action.lifecycle_state {
            ContainmentLifecycle::Pending => self.reconcile_pending(action, current_time_ns),
            ContainmentLifecycle::Expiring => self.reconcile_detach(action, current_time_ns),
            ContainmentLifecycle::Active
            | ContainmentLifecycle::Expired
            | ContainmentLifecycle::Failed => Err(SecurityStoreError::InvalidData(format!(
                "containment action {} has invalid claimed lifecycle {:?}",
                action.action_id, action.lifecycle_state
            ))
            .into()),
        }
    }

    fn reconcile_pending(
        &self,
        mut action: ContainmentAction,
        current_time_ns: u64,
    ) -> Result<(), ContainmentError> {
        let claimed_at_ns = action.updated_at_ns;
        let snapshot = match self.enforcer.bindings() {
            Ok(snapshot) => snapshot,
            Err(ContainmentEnforcerError::Unavailable(message)) => {
                return self.record_pending_unavailable(
                    &mut action,
                    claimed_at_ns,
                    current_time_ns,
                    message,
                );
            }
            Err(ContainmentEnforcerError::Rejected(message)) => {
                return self.fail_pending(action, claimed_at_ns, &message, false, current_time_ns);
            }
            Err(ContainmentEnforcerError::MissingTransition(_)) => {
                return self.fail_pending(
                    action,
                    claimed_at_ns,
                    "enforcement binding snapshot unexpectedly reported a missing transition",
                    false,
                    current_time_ns,
                );
            }
        };
        let (bindings, _) = snapshot.into_parts();
        let exact = exact_binding(&bindings, action.binding_id);
        if exact.is_err() {
            return self.fail_pending(
                action,
                claimed_at_ns,
                "enforcer returned duplicate containment binding identities",
                true,
                current_time_ns,
            );
        }
        let exact = exact.ok().flatten();
        if action
            .expires_at_ns
            .is_some_and(|expires_at_ns| expires_at_ns <= current_time_ns)
        {
            if exact.is_some() {
                return self.expire_pending_binding(action, claimed_at_ns, current_time_ns);
            }
            action.lifecycle_state = ContainmentLifecycle::Expired;
            action.failure_stage = None;
            action.failure_reason = None;
            action.next_retry_at_ns = None;
            return self.finish_claimed(&action, ContainmentLifecycle::Pending, claimed_at_ns);
        }
        let Some(source_binding_id) = action.source_binding_id else {
            return self.fail_pending(
                action,
                claimed_at_ns,
                "legacy containment action lacks exact source audit binding provenance",
                exact.is_some(),
                current_time_ns,
            );
        };
        let detail = self.store.case_detail(action.case_id)?;
        if !matches!(
            detail.case.status,
            RiskCaseStatus::Open | RiskCaseStatus::Confirmed
        ) {
            return self.fail_pending(
                action,
                claimed_at_ns,
                "source case is no longer eligible for containment recovery",
                exact.is_some(),
                current_time_ns,
            );
        }
        let stored_policy_snapshot =
            match self.enforcer.credential_policy_snapshot(source_binding_id) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    return self.record_pending_unavailable(
                        &mut action,
                        claimed_at_ns,
                        current_time_ns,
                        error.to_string(),
                    );
                }
            };
        let Some(context) =
            resolve_transition_policy(detail, bindings, source_binding_id, stored_policy_snapshot)
        else {
            return self.fail_pending(
                action,
                claimed_at_ns,
                "original audit binding provenance is unavailable",
                exact.is_some(),
                current_time_ns,
            );
        };
        if context.detail.case.agent_id != action.agent_id
            || context.source_path != action.source_path
        {
            return self.fail_pending(
                action,
                claimed_at_ns,
                "persisted containment identity does not match source provenance",
                exact.is_some(),
                current_time_ns,
            );
        }
        let Some(request) = enforce_request(
            &context,
            action.binding_id,
            action.root_pid,
            action.process_start_time,
        ) else {
            return self.fail_pending(
                action,
                claimed_at_ns,
                "source policy cannot be reconstructed exactly",
                exact.is_some(),
                current_time_ns,
            );
        };

        let key = TransitionKey {
            action_id: action.action_id,
            direction: TransitionDirection::Forward,
        };
        let stamped = match self.enforcer.resume_transition(&key) {
            Ok(stamped) => Ok(stamped),
            Err(ContainmentEnforcerError::MissingTransition(_)) => {
                if context.binding.state != BindingState::Enforced {
                    return self.fail_pending(
                        action,
                        claimed_at_ns,
                        "missing transition cannot be rebuilt from a detached audit binding",
                        exact.is_some(),
                        current_time_ns,
                    );
                }
                if validate_process_identity(action.root_pid, action.process_start_time).is_err() {
                    return self.fail_pending(
                        action,
                        claimed_at_ns,
                        "persisted containment process identity is stale",
                        false,
                        current_time_ns,
                    );
                }
                self.enforcer.begin_transition(
                    key,
                    ReplacePolicy {
                        expected: context.binding.clone(),
                        source: ReplacementSource::Credential(source_policy_snapshot(&context)),
                        replacement: ReplacementPolicy::Credential(request.clone()),
                    },
                )
            }
            Err(error) => Err(error),
        };
        let stamped = match stamped {
            Ok(stamped) => stamped,
            Err(ContainmentEnforcerError::Unavailable(message)) => {
                return self.record_pending_unavailable(
                    &mut action,
                    claimed_at_ns,
                    current_time_ns,
                    message,
                );
            }
            Err(ContainmentEnforcerError::Rejected(message)) => {
                return self.fail_pending(action, claimed_at_ns, &message, false, current_time_ns);
            }
            Err(ContainmentEnforcerError::MissingTransition(_)) => {
                return self.record_pending_unavailable(
                    &mut action,
                    claimed_at_ns,
                    current_time_ns,
                    "persisted policy transition disappeared".into(),
                );
            }
        };
        let (acknowledgement, activation_stamp) = stamped.into_parts();
        if !acknowledgement_matches(&acknowledgement, &request) {
            return self.fail_pending(
                action,
                claimed_at_ns,
                "enforcer returned an invalid recovery acknowledgement",
                true,
                current_time_ns,
            );
        }
        let lease = match self.enforcer.lease_ready(activation_stamp) {
            Ok(lease) => lease,
            Err(error) => {
                return self.record_pending_unavailable(
                    &mut action,
                    claimed_at_ns,
                    current_time_ns,
                    error.to_string(),
                );
            }
        };
        let activation = self.store.activate_containment_action(
            action.action_id,
            claimed_at_ns,
            current_time_ns.max(claimed_at_ns.saturating_add(1)),
        );
        drop(lease);
        match activation {
            Ok(ContainmentActivationResult::Activated) => Ok(()),
            Ok(ContainmentActivationResult::CaseIneligible(_)) => self.fail_pending(
                action,
                claimed_at_ns,
                "source case changed eligibility during containment recovery",
                true,
                current_time_ns,
            ),
            Ok(ContainmentActivationResult::LostClaim) => {
                Err(ContainmentError::ClaimLost(action.action_id))
            }
            Err(error) => {
                let reason = format!("transactional recovery activation failed: {error}");
                self.fail_pending(action, claimed_at_ns, &reason, true, current_time_ns)
            }
        }
    }

    fn record_pending_unavailable(
        &self,
        action: &mut ContainmentAction,
        claimed_at_ns: u64,
        current_time_ns: u64,
        reason: String,
    ) -> Result<(), ContainmentError> {
        let reason = record_pending_unavailable(
            self.store,
            action,
            claimed_at_ns,
            current_time_ns,
            &reason,
        )?;
        Err(ContainmentError::Enforcer(reason))
    }

    fn fail_pending(
        &self,
        mut action: ContainmentAction,
        claimed_at_ns: u64,
        reason: &str,
        cleanup_binding: bool,
        current_time_ns: u64,
    ) -> Result<(), ContainmentError> {
        let reason = sanitize_failure(reason);
        if cleanup_binding {
            return self.restore_failed_pending(action, claimed_at_ns, current_time_ns, reason);
        }
        action.lifecycle_state = ContainmentLifecycle::Failed;
        action.failure_stage = Some(ContainmentFailureStage::Reconcile);
        action.failure_reason = Some(reason.clone());
        action.next_retry_at_ns = None;
        self.finish_claimed(&action, ContainmentLifecycle::Pending, claimed_at_ns)?;
        Err(ContainmentError::RecoveryFailed {
            action_id: action.action_id,
            reason,
        })
    }

    fn finish_claimed(
        &self,
        action: &ContainmentAction,
        claimed_lifecycle: ContainmentLifecycle,
        claimed_at_ns: u64,
    ) -> Result<(), ContainmentError> {
        if self
            .store
            .finish_containment_reconciliation(action, claimed_lifecycle, claimed_at_ns)?
        {
            return Ok(());
        }
        Err(ContainmentError::ClaimLost(action.action_id))
    }
}
