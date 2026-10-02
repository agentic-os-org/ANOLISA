//! Reconciliation decisions over generic aggregate reads and CAS writes.
use crate::model::PendingWrite;
use crate::{
    AttemptOutcome, BindingStateRepository, BindingStateWrite, Deployment, ExpectedBinding,
    PreparedAttempt, Presence, ReconcileRecord, RetryPolicy, StoreError, TargetRef, WriteResult,
};
use asc_foundation_types::ResourceId;
use asc_policy_types::binding::BindingStatus;
use std::sync::Arc;

pub(crate) struct ReconcileState {
    pub repository: Arc<dyn BindingStateRepository>,
}
impl ReconcileState {
    pub fn read(&self, id: &ResourceId) -> Result<Option<ReconcileRecord>, StoreError> {
        self.repository.get_binding_state(id)
    }
    pub fn recover(
        &self,
        record: &ReconcileRecord,
        now: u64,
        policy: RetryPolicy,
        schedule: &mut crate::AttemptSchedule,
    ) -> Result<bool, StoreError> {
        let mut next = record.clone();
        let exhausted = schedule.attempts_started >= policy.max_attempts;
        next.binding.status.phase = if exhausted {
            record.binding.status.phase.fail_reconcile()
        } else {
            record.binding.status.phase.retry_reconcile()
        }
        .map_err(|_| StoreError::Invalid)?;
        next.binding.status.error = Some(crate::Failure::new(
            crate::FailureKind::Retryable,
            "RECONCILE_INTERRUPTED",
        ));
        let next_attempt_at = if exhausted {
            None
        } else {
            Some(now.saturating_add(crate::retry::delay(policy, schedule.attempts_started)))
        };
        let matched = self
            .repository
            .compare_exchange_binding_state(record, &BindingStateWrite::new(next))?
            != WriteResult::Conflict;
        if matched {
            schedule.next_attempt_at = next_attempt_at;
        }
        Ok(matched)
    }
    pub fn claim(
        &self,
        record: &ReconcileRecord,
        now: u64,
        policy: RetryPolicy,
        schedule: &mut crate::AttemptSchedule,
    ) -> Result<Option<ReconcileRecord>, StoreError> {
        let before = schedule.clone();
        let Some(next) = claim(record.clone(), now, policy, schedule)? else {
            return Ok(None);
        };
        let write = BindingStateWrite::new(next.clone());
        match self
            .repository
            .compare_exchange_binding_state(record, &write)?
        {
            WriteResult::Applied | WriteResult::AlreadyApplied => Ok(Some(next)),
            WriteResult::Conflict => {
                *schedule = before;
                Ok(None)
            }
        }
    }
    pub fn register(
        &self,
        expected: &ExpectedBinding,
        prepared: Option<&PreparedAttempt>,
        targets: &[TargetRef],
    ) -> Result<bool, StoreError> {
        for _ in 0..16 {
            let Some(record) = self.read(&expected.id)? else {
                return Ok(false);
            };
            let Some(next) = register(record.clone(), expected, prepared, targets)? else {
                return Ok(false);
            };
            let write = BindingStateWrite::patch(asc_policy_repository::ReconciliationPatch {
                deployments: Some(next.deployments),
                ..Default::default()
            });
            if self
                .repository
                .compare_exchange_binding_state(&record, &write)?
                != WriteResult::Conflict
            {
                return Ok(true);
            }
        }
        Err(StoreError::Contended)
    }

    pub fn finish(
        &self,
        outcome: &AttemptOutcome,
        completion: &mut Option<PendingWrite>,
    ) -> Result<bool, StoreError> {
        // A conflict is recomputed against the latest aggregate. Bound contention
        // work per call; no outcome or write is retained after reconcile returns.
        for _ in 0..16 {
            if completion.is_none() {
                let Some(record) = self.read(&outcome.expected.id)? else {
                    return Ok(false);
                };
                let (next, matched) = finish(record.clone(), outcome)?;
                *completion = Some(PendingWrite {
                    expected: record,
                    write: if matched && outcome.next_status == BindingStatus::Deleted {
                        BindingStateWrite::delete()
                    } else if matched {
                        BindingStateWrite::new(next)
                    } else {
                        BindingStateWrite::patch(asc_policy_repository::ReconciliationPatch {
                            deployments: Some(next.deployments),
                            ..Default::default()
                        })
                    },
                    matched,
                });
            }
            let pending = completion.as_ref().ok_or(StoreError::Invalid)?;
            match self
                .repository
                .compare_exchange_binding_state(&pending.expected, &pending.write)?
            {
                WriteResult::Applied => {
                    let matched = pending.matched;
                    *completion = None;
                    return Ok(matched);
                }
                // Recheck the latest intent on the next call; never replay a
                // removed Absent target or overwrite a newer PAP write.
                WriteResult::AlreadyApplied => {
                    let removed = pending.write.next.is_none();
                    *completion = None;
                    return Ok(removed);
                }
                WriteResult::Conflict => *completion = None,
            }
        }
        Err(StoreError::Contended)
    }
}

fn claim(
    mut record: ReconcileRecord,
    now: u64,
    policy: RetryPolicy,
    schedule: &mut crate::AttemptSchedule,
) -> Result<Option<ReconcileRecord>, StoreError> {
    crate::retry::validate(policy)?;
    if schedule.next_attempt_at.is_some_and(|at| at > now) {
        return Ok(None);
    }
    let Ok(running) = record.binding.status.phase.start_reconcile() else {
        return Ok(None);
    };
    crate::retry::validate(policy)?;
    if schedule.attempts_started >= policy.max_attempts {
        return Ok(None);
    }
    record.binding.status.phase = running;
    record.binding.status.error = None;
    schedule.attempts_started += 1;
    schedule.next_attempt_at = None;
    Ok(Some(record))
}
fn register(
    mut record: ReconcileRecord,
    expected: &ExpectedBinding,
    prepared: Option<&PreparedAttempt>,
    targets: &[TargetRef],
) -> Result<Option<ReconcileRecord>, StoreError> {
    if !expected.matches(&record.binding) {
        return Ok(None);
    }
    if !expected.status.is_reconciling() {
        return Err(StoreError::Invalid);
    }
    if let Some(saved) = prepared
        && (expected.status != BindingStatus::Applying || saved.revision != expected.revision)
    {
        return Err(StoreError::Invalid);
    }
    for (index, target) in targets.iter().enumerate() {
        if targets[..index].iter().any(|t| t.same_identity(target)) {
            return Err(StoreError::Invalid);
        }
        let new_target = prepared.is_some_and(|p| p.prepared.target == *target);
        if let Some(deployment) = record
            .deployments
            .iter_mut()
            .find(|d| d.target.same_identity(target))
        {
            if deployment.target != *target
                && (!new_target || deployment.revision == expected.revision)
            {
                return Err(StoreError::Invalid);
            }
            deployment.presence = Presence::Unknown;
            if new_target {
                deployment.target = target.clone();
                deployment.revision = expected.revision;
            }
        } else if new_target {
            record.deployments.push(Deployment {
                target: target.clone(),
                revision: expected.revision,
                presence: Presence::Unknown,
                last_confirmed: None,
            });
        } else {
            return Err(StoreError::Invalid);
        }
    }
    if prepared.is_some_and(|p| !targets.contains(&p.prepared.target)) {
        return Err(StoreError::Invalid);
    }
    record
        .deployments
        .sort_by(|a, b| (&a.target.route, &a.target.id).cmp(&(&b.target.route, &b.target.id)));
    Ok(Some(record))
}
fn finish(
    mut record: ReconcileRecord,
    outcome: &AttemptOutcome,
) -> Result<(ReconcileRecord, bool), StoreError> {
    if !outcome.expected.status.is_reconciling() {
        return Err(StoreError::Invalid);
    }
    outcome
        .expected
        .status
        .validate_successor(outcome.next_status)
        .map_err(|_| StoreError::Invalid)?;
    let pending = matches!(
        outcome.next_status,
        BindingStatus::PendingApply | BindingStatus::PendingDelete
    );
    let success = matches!(
        outcome.next_status,
        BindingStatus::Ready | BindingStatus::Deleted
    );
    if outcome.next_status == outcome.expected.status
        || pending != outcome.next_attempt_at.is_some()
        || success != outcome.error.is_none()
    {
        return Err(StoreError::Invalid);
    }
    // Validate all evidence before touching the live state. Even a stale
    // lifecycle may only report registered, not newer, target identities.
    for (index, observation) in outcome.observations.iter().enumerate() {
        if outcome.observations[..index]
            .iter()
            .any(|o| o.target.same_identity(&observation.target))
            || !record.deployments.iter().any(|d| {
                d.target == observation.target
                    && d.revision.get() <= outcome.expected.revision.get()
            })
        {
            return Err(StoreError::Invalid);
        }
    }
    for observation in &outcome.observations {
        if observation.presence == Presence::Absent {
            record
                .deployments
                .retain(|d| !d.target.same_identity(&observation.target));
        } else if let Some(deployment) = record
            .deployments
            .iter_mut()
            .find(|d| d.target.same_identity(&observation.target))
        {
            deployment.presence = observation.presence;
            if observation.presence == Presence::Present {
                deployment.last_confirmed = Some(Presence::Present);
            }
        }
    }
    let matches = outcome.expected.matches(&record.binding);
    if matches {
        if outcome.next_status == BindingStatus::Deleted {
            if !record.deployments.is_empty() {
                return Err(StoreError::Invalid);
            }
            // The caller atomically removes the aggregate. There is no Deleted
            // snapshot to persist, but all evidence must still be validated.
            return Ok((record, true));
        }
        if outcome.next_status == BindingStatus::Ready
            && !(record.deployments.len() == 1
                && record.deployments[0].revision == outcome.expected.revision
                && record.deployments[0].presence == Presence::Present)
        {
            return Err(StoreError::Invalid);
        }
        record.binding.status.phase = outcome.next_status;
        record.binding.status.error.clone_from(&outcome.error);
    }
    Ok((record, matches))
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
