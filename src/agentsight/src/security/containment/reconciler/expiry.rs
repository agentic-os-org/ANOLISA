//! Reverse transition recovery for expiring containment actions.

use agentsight_enforcement_protocol::BindingState;

use super::super::pending::retry_delay_ns;
use super::super::{
    ContainmentAction, ContainmentEnforcerError, ContainmentError, ContainmentFailureStage,
    ContainmentLifecycle, sanitize_failure,
};
use super::Reconciler;
use crate::enforcement::{TransitionDirection, TransitionKey};
use crate::security::StampedBinding;

const RESTORE_MAX_RETRIES: u32 = 5;

impl Reconciler<'_> {
    pub(super) fn expire_pending_binding(
        &self,
        action: ContainmentAction,
        claimed_at_ns: u64,
        current_time_ns: u64,
    ) -> Result<(), ContainmentError> {
        let Some(cleanup) = self.store.begin_containment_cleanup(
            &action,
            ContainmentLifecycle::Pending,
            claimed_at_ns,
            current_time_ns,
            "expired containment requires audit restoration".into(),
        )?
        else {
            return Err(ContainmentError::ClaimLost(action.action_id));
        };
        self.reconcile_detach(cleanup, current_time_ns)
    }

    pub(super) fn reconcile_detach(
        &self,
        mut action: ContainmentAction,
        current_time_ns: u64,
    ) -> Result<(), ContainmentError> {
        let claimed_at_ns = action.updated_at_ns;
        let restored = match self.reverse_transition(action.action_id) {
            Ok(restored) => restored,
            Err(error) => {
                let reason = sanitize_failure(&error.to_string());
                self.record_detach_failure(
                    &mut action,
                    ContainmentLifecycle::Expiring,
                    claimed_at_ns,
                    current_time_ns,
                    reason.clone(),
                )?;
                return Err(ContainmentError::Enforcer(reason));
            }
        };
        let (binding, readiness_stamp) = restored.into_parts();
        let valid_source = action.source_binding_id.is_some_and(|source_binding_id| {
            binding.request.binding_id == source_binding_id
                && binding.state == BindingState::Enforced
        });
        if !valid_source {
            let reason =
                String::from("reverse transition returned an invalid audit acknowledgement");
            self.record_detach_failure(
                &mut action,
                ContainmentLifecycle::Expiring,
                claimed_at_ns,
                current_time_ns,
                reason.clone(),
            )?;
            return Err(ContainmentError::Enforcer(reason));
        }
        let lease = match self.enforcer.lease_ready(readiness_stamp) {
            Ok(lease) => lease,
            Err(error) => {
                let reason = sanitize_failure(&error.to_string());
                self.record_detach_failure(
                    &mut action,
                    ContainmentLifecycle::Expiring,
                    claimed_at_ns,
                    current_time_ns,
                    reason.clone(),
                )?;
                return Err(ContainmentError::Enforcer(reason));
            }
        };
        action.lifecycle_state = ContainmentLifecycle::Expired;
        action.failure_stage = None;
        action.failure_reason = None;
        action.next_retry_at_ns = None;
        let result = self.finish_claimed(&action, ContainmentLifecycle::Expiring, claimed_at_ns);
        drop(lease);
        result
    }

    pub(super) fn restore_failed_pending(
        &self,
        action: ContainmentAction,
        claimed_at_ns: u64,
        current_time_ns: u64,
        reason: String,
    ) -> Result<(), ContainmentError> {
        let Some(mut cleanup) = self.store.begin_containment_cleanup(
            &action,
            ContainmentLifecycle::Pending,
            claimed_at_ns,
            current_time_ns,
            reason.clone(),
        )?
        else {
            return Err(ContainmentError::ClaimLost(action.action_id));
        };
        let cleanup_claim_ns = cleanup.updated_at_ns;
        let restored = match self.reverse_transition(action.action_id) {
            Ok(restored) => restored,
            Err(error) => {
                let restore_reason =
                    sanitize_failure(&format!("{reason}; audit restoration failed: {error}"));
                self.record_detach_failure(
                    &mut cleanup,
                    ContainmentLifecycle::Expiring,
                    cleanup_claim_ns,
                    current_time_ns,
                    restore_reason.clone(),
                )?;
                return Err(ContainmentError::CleanupRequired {
                    action_id: cleanup.action_id,
                    binding_id: cleanup.binding_id,
                    reason: restore_reason,
                });
            }
        };
        let (binding, readiness_stamp) = restored.into_parts();
        let valid_source = action.source_binding_id.is_some_and(|source_binding_id| {
            binding.request.binding_id == source_binding_id
                && binding.state == BindingState::Enforced
        });
        if !valid_source {
            let restore_reason =
                String::from("reverse transition returned an invalid audit acknowledgement");
            self.record_detach_failure(
                &mut cleanup,
                ContainmentLifecycle::Expiring,
                cleanup_claim_ns,
                current_time_ns,
                restore_reason.clone(),
            )?;
            return Err(ContainmentError::CleanupRequired {
                action_id: cleanup.action_id,
                binding_id: cleanup.binding_id,
                reason: restore_reason,
            });
        }
        let lease = match self.enforcer.lease_ready(readiness_stamp) {
            Ok(lease) => lease,
            Err(error) => {
                let restore_reason =
                    sanitize_failure(&format!("{reason}; audit readiness changed: {error}"));
                self.record_detach_failure(
                    &mut cleanup,
                    ContainmentLifecycle::Expiring,
                    cleanup_claim_ns,
                    current_time_ns,
                    restore_reason.clone(),
                )?;
                return Err(ContainmentError::CleanupRequired {
                    action_id: cleanup.action_id,
                    binding_id: cleanup.binding_id,
                    reason: restore_reason,
                });
            }
        };
        cleanup.lifecycle_state = ContainmentLifecycle::Failed;
        cleanup.failure_stage = Some(ContainmentFailureStage::Reconcile);
        cleanup.failure_reason = Some(reason.clone());
        cleanup.next_retry_at_ns = None;
        let result =
            self.finish_claimed(&cleanup, ContainmentLifecycle::Expiring, cleanup_claim_ns);
        drop(lease);
        result?;
        Err(ContainmentError::RecoveryFailed {
            action_id: cleanup.action_id,
            reason,
        })
    }

    fn reverse_transition(
        &self,
        action_id: uuid::Uuid,
    ) -> Result<StampedBinding, ContainmentEnforcerError> {
        let reverse_key = TransitionKey {
            action_id,
            direction: TransitionDirection::Reverse,
        };
        match self.enforcer.resume_transition(&reverse_key) {
            Ok(restored) => Ok(restored),
            Err(ContainmentEnforcerError::MissingTransition(_)) => {
                self.enforcer.begin_reverse_transition(action_id)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn record_detach_failure(
        &self,
        action: &mut ContainmentAction,
        claimed_lifecycle: ContainmentLifecycle,
        claimed_at_ns: u64,
        current_time_ns: u64,
        reason: String,
    ) -> Result<(), ContainmentError> {
        action.attempt_count = action.attempt_count.saturating_add(1);
        action.failure_stage = Some(ContainmentFailureStage::Reconcile);
        action.failure_reason = Some(reason);
        if action.attempt_count >= RESTORE_MAX_RETRIES {
            action.lifecycle_state = ContainmentLifecycle::Failed;
            action.next_retry_at_ns = None;
        } else {
            action.lifecycle_state = ContainmentLifecycle::Expiring;
            action.next_retry_at_ns =
                Some(current_time_ns.saturating_add(retry_delay_ns(action.attempt_count)));
        }
        self.finish_claimed(action, claimed_lifecycle, claimed_at_ns)
    }
}
