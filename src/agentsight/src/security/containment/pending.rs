//! Shared persistence for retryable Pending containment actions.

use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    ContainmentAction, ContainmentError, ContainmentFailureStage, ContainmentLifecycle,
    SecurityStore, sanitize_failure,
};

const SECOND_NS: u64 = 1_000_000_000;

pub(super) fn now_ns() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    u64::try_from(nanos).unwrap_or(u64::MAX)
}

pub(super) fn record_pending_unavailable(
    store: &SecurityStore,
    action: &mut ContainmentAction,
    claimed_at_ns: u64,
    current_time_ns: u64,
    reason: &str,
) -> Result<String, ContainmentError> {
    let reason = sanitize_failure(reason);
    action.lifecycle_state = ContainmentLifecycle::Pending;
    action.failure_stage = Some(ContainmentFailureStage::Reconcile);
    action.failure_reason = Some(reason.clone());
    action.attempt_count = action.attempt_count.saturating_add(1);
    action.next_retry_at_ns =
        Some(current_time_ns.saturating_add(retry_delay_ns(action.attempt_count)));
    action.updated_at_ns = current_time_ns;
    if store.finish_containment_reconciliation(
        action,
        ContainmentLifecycle::Pending,
        claimed_at_ns,
    )? {
        return Ok(reason);
    }
    Err(ContainmentError::ClaimLost(action.action_id))
}

pub(super) fn record_direct_pending_unavailable(
    store: &SecurityStore,
    mut action: ContainmentAction,
    current_time_ns: u64,
    reason: &str,
) -> Result<ContainmentAction, ContainmentError> {
    let claimed_at_ns = action.updated_at_ns;
    let current_time_ns = current_time_ns.max(claimed_at_ns.saturating_add(1));
    let reason =
        record_pending_unavailable(store, &mut action, claimed_at_ns, current_time_ns, reason)?;
    Err(ContainmentError::Enforcer(reason))
}

pub(super) fn record_attach_failed(
    store: &SecurityStore,
    mut action: ContainmentAction,
    current_time_ns: u64,
    reason: &str,
) -> Result<ContainmentAction, ContainmentError> {
    let reason = sanitize_failure(reason);
    let claimed_at_ns = action.updated_at_ns;
    action.lifecycle_state = ContainmentLifecycle::Failed;
    action.failure_stage = Some(ContainmentFailureStage::Attach);
    action.failure_reason = Some(reason.clone());
    action.next_retry_at_ns = None;
    action.updated_at_ns = current_time_ns.max(claimed_at_ns.saturating_add(1));
    if !store.finish_containment_reconciliation(
        &action,
        ContainmentLifecycle::Pending,
        claimed_at_ns,
    )? {
        return Err(ContainmentError::ClaimLost(action.action_id));
    }
    Err(ContainmentError::Enforcer(reason))
}

pub(super) fn retry_delay_ns(attempt_count: u32) -> u64 {
    let shift = attempt_count.saturating_sub(1).min(4);
    SECOND_NS.saturating_mul(1_u64 << shift)
}
