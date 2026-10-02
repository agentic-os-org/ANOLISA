//! Audit restoration when containment cannot become active.

use agentsight_enforcement_protocol::BindingState;

use super::{
    ContainmentAction, ContainmentCoordinator, ContainmentEnforcerError, ContainmentError,
    ContainmentFailureStage, ContainmentLifecycle, now_ns, sanitize_failure,
};
use crate::enforcement::{TransitionDirection, TransitionKey};

const CLEANUP_RETRY_DELAY_NS: u64 = 1_000_000_000;

impl ContainmentCoordinator {
    pub(super) fn detach_and_fail(
        &self,
        action: &mut ContainmentAction,
        stage: ContainmentFailureStage,
        message: &str,
    ) -> Result<(), ContainmentError> {
        let reason = sanitize_failure(message);
        let claimed_at_ns = action.updated_at_ns;
        let current_time_ns = now_ns();
        let Some(mut cleanup) = self.store.begin_containment_cleanup(
            action,
            ContainmentLifecycle::Pending,
            claimed_at_ns,
            current_time_ns,
            reason.clone(),
        )?
        else {
            return Err(ContainmentError::ClaimLost(action.action_id));
        };
        let cleanup_claim_ns = cleanup.updated_at_ns;
        let reverse_key = TransitionKey {
            action_id: action.action_id,
            direction: TransitionDirection::Reverse,
        };
        let restored = match self.enforcer.resume_transition(&reverse_key) {
            Ok(restored) => Ok(restored),
            Err(ContainmentEnforcerError::MissingTransition(_)) => {
                self.enforcer.begin_reverse_transition(action.action_id)
            }
            Err(error) => Err(error),
        };
        let restored = match restored {
            Ok(restored) => restored,
            Err(error) => {
                return self.persist_rollback_retry(
                    action,
                    cleanup,
                    cleanup_claim_ns,
                    current_time_ns,
                    &format!("{message}; audit restoration failed: {error}"),
                );
            }
        };
        let (binding, readiness_stamp) = restored.into_parts();
        let valid_source = action.source_binding_id.is_some_and(|source_binding_id| {
            binding.request.binding_id == source_binding_id
                && binding.state == BindingState::Enforced
        });
        if !valid_source {
            return self.persist_rollback_retry(
                action,
                cleanup,
                cleanup_claim_ns,
                current_time_ns,
                "reverse transition returned an invalid audit acknowledgement",
            );
        }
        let lease = match self.enforcer.lease_ready(readiness_stamp) {
            Ok(lease) => lease,
            Err(error) => {
                return self.persist_rollback_retry(
                    action,
                    cleanup,
                    cleanup_claim_ns,
                    current_time_ns,
                    &format!("{message}; audit readiness changed: {error}"),
                );
            }
        };
        cleanup.lifecycle_state = ContainmentLifecycle::Failed;
        cleanup.failure_stage = Some(stage);
        cleanup.failure_reason = Some(reason);
        cleanup.next_retry_at_ns = None;
        cleanup.updated_at_ns = now_ns().max(cleanup_claim_ns.saturating_add(1));
        let result =
            self.finish_direct_claim(&cleanup, ContainmentLifecycle::Expiring, cleanup_claim_ns);
        drop(lease);
        result?;
        *action = cleanup;
        Ok(())
    }

    fn persist_rollback_retry(
        &self,
        action: &mut ContainmentAction,
        mut cleanup: ContainmentAction,
        cleanup_claim_ns: u64,
        current_time_ns: u64,
        message: &str,
    ) -> Result<(), ContainmentError> {
        let reason = sanitize_failure(message);
        cleanup.failure_stage = Some(ContainmentFailureStage::Reconcile);
        cleanup.failure_reason = Some(reason.clone());
        cleanup.attempt_count = cleanup.attempt_count.saturating_add(1);
        cleanup.next_retry_at_ns = Some(current_time_ns.saturating_add(CLEANUP_RETRY_DELAY_NS));
        cleanup.updated_at_ns = now_ns().max(cleanup_claim_ns.saturating_add(1));
        self.finish_direct_claim(&cleanup, ContainmentLifecycle::Expiring, cleanup_claim_ns)?;
        *action = cleanup;
        Err(ContainmentError::CleanupRequired {
            action_id: action.action_id,
            binding_id: action.binding_id,
            reason,
        })
    }

    fn finish_direct_claim(
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
