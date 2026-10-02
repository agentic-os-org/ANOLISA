use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;

use asc_foundation_types::ResourceId;
use asc_policy_types::binding::BindingStatus;
use asc_policy_types::target::TranslationOutcome;

use crate::{
    AttemptOutcome, BindingStateRepository, Clock, DeploymentReport, Disposition, ExecutionSlot,
    ExpectedBinding, Failure, FailureKind, Observation, PreparedAttempt, Presence, ReconcileRecord,
    RetryPolicy, StoreError, TargetBindingAdapter, TargetDeploymentClient,
    TargetDeploymentClientFactory, TargetRef,
};

/// Pure orchestration over repository, Adapter and Client ports. The caller
/// serializes attempts for each Binding, including across core instances.
pub struct BindingReconciler {
    state: crate::state::ReconcileState,
    adapter: Arc<dyn TargetBindingAdapter>,
    client_factories: BTreeMap<String, Arc<dyn TargetDeploymentClientFactory>>,
    apply_route: String,
    clock: Arc<dyn Clock>,
    retry: RetryPolicy,
}

impl BindingReconciler {
    /// Routes are stable configuration references, never endpoint credentials.
    /// The caller must serialize all calls for a Binding in the same store.
    /// Temporary results never survive a call.
    /// # Errors
    /// Rejects invalid retry configuration or a missing Apply Client factory.
    pub fn new(
        repository: Arc<dyn BindingStateRepository>,
        adapter: Arc<dyn TargetBindingAdapter>,
        client_factories: BTreeMap<String, Arc<dyn TargetDeploymentClientFactory>>,
        apply_route: String,
        clock: Arc<dyn Clock>,
        retry: RetryPolicy,
    ) -> Result<Self, StoreError> {
        crate::retry::validate(retry)?;
        if !client_factories.contains_key(&apply_route) {
            return Err(StoreError::Invalid);
        }
        Ok(Self {
            state: crate::state::ReconcileState { repository },
            adapter,
            client_factories,
            apply_route,
            clock,
            retry,
        })
    }

    /// Shares the exact clock used to compute retry deadlines with the scheduler.
    pub fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    /// Reloads current state and executes at most one due attempt from scratch.
    /// The caller retains `schedule` between calls and discards it on process exit.
    /// The returned retry deadline is for the caller's timer; it is not stored.
    ///
    /// This synchronous call is intentionally not cancellation-safe by dropping
    /// an async wrapper: callers must retain/join their blocking task. No task is
    /// spawned or detached here. The caller must retain exclusive scheduling of
    /// this Binding until the call returns or finishes unwinding, including panic
    /// bookkeeping. In production, one Runtime `WorkQueue` per store owns this rule.
    /// # Errors
    /// Storage failure or exhausted CAS contention never implies remote failure
    /// or success. Temporary outcomes
    /// are discarded on return; the next call recovers from repository facts.
    /// # Panics
    /// Resumes a port panic after attempting terminal bookkeeping.
    /// The caller must catch the attempt panic, inspect committed state and finish
    /// scheduling this ID before continuing other work. Unwinding never proves remote absence.
    pub fn reconcile(
        &self,
        id: &ResourceId,
        schedule: &mut crate::AttemptSchedule,
    ) -> Result<Disposition, StoreError> {
        let mut slot = ExecutionSlot::default();
        // Keep call-local completion data available for panic bookkeeping.
        // The caller retains the Running entry throughout this unwind boundary.
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.reconcile_attempt(id, &mut slot, schedule)
        }));
        match result {
            Ok(result) => result,
            Err(payload) => {
                // Preserve an actual completion over the provisional panic
                // failure within this call. If storage fails too, discard the
                // temporary result on exit and recover from registered facts.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    if let Some(outcome) = slot.pending.clone() {
                        self.commit(&outcome, &mut slot.completion)?;
                        slot.pending = None;
                    }
                    Ok::<_, StoreError>(())
                }));
                // The owner catches this attempt failure without stopping other Bindings.
                resume_unwind(payload)
            }
        }
    }

    fn reconcile_attempt(
        &self,
        id: &ResourceId,
        slot: &mut ExecutionSlot,
        schedule: &mut crate::AttemptSchedule,
    ) -> Result<Disposition, StoreError> {
        let Some(mut record) = self.state.read(id)? else {
            return Ok(Disposition::Skipped);
        };
        schedule.observe(&record.binding);
        // Caller serialization guarantees any earlier invocation has exited.
        // Recovery preserves budget and cannot overwrite a newer CRUD intent.
        if record.binding.status.phase.is_reconciling() {
            if !self
                .state
                .recover(&record, self.clock.now_ms(), self.retry, schedule)?
            {
                return Ok(Disposition::Superseded);
            }
            let Some(latest) = self.state.read(id)? else {
                return Ok(Disposition::Skipped);
            };
            record = latest;
        }
        if !matches!(
            record.binding.status.phase,
            BindingStatus::PendingApply | BindingStatus::PendingDelete
        ) || schedule
            .next_attempt_at
            .is_some_and(|at| at > self.clock.now_ms())
        {
            return Ok(Disposition::Skipped);
        }
        let mut expected = ExpectedBinding::from_binding(&record.binding);
        expected.status = expected
            .status
            .start_reconcile()
            .map_err(|_| StoreError::Invalid)?;
        // Install before claim: a repository wrapper may panic after committing
        // the claim but before returning it. CAS prevents failing unclaimed work.
        slot.pending = Some(AttemptOutcome {
            next_status: expected
                .status
                .fail_reconcile()
                .map_err(|_| StoreError::Invalid)?,
            expected,
            observations: vec![],
            next_attempt_at: None,
            error: Some(Failure::new(
                FailureKind::Rejected,
                "RECONCILE_WORKER_PANICKED",
            )),
        });
        let claimed = self
            .state
            .claim(&record, self.clock.now_ms(), self.retry, schedule);
        let claimed = match claimed {
            Ok(Some(claimed)) => claimed,
            other => {
                slot.pending = None;
                return other.map(|_| Disposition::Superseded);
            }
        };
        let Some(report) = self.execute(&claimed)? else {
            slot.pending = None;
            return Ok(Disposition::Superseded);
        };
        slot.pending = Some(self.outcome(&claimed, report, schedule)?);
        schedule.next_attempt_at = slot.pending.as_ref().and_then(|o| o.next_attempt_at);
        let disposition = self.commit(
            slot.pending.as_ref().ok_or(StoreError::Invalid)?,
            &mut slot.completion,
        )?;
        slot.pending = None;
        Ok(disposition)
    }

    fn execute(&self, record: &ReconcileRecord) -> Result<Option<DeploymentReport>, StoreError> {
        if record.binding.status.phase == BindingStatus::Deleting {
            return self.delete(record);
        }
        let (saved, client) = match self.prepare(record) {
            Ok(saved) => saved,
            Err(error) => {
                return Ok(Some(DeploymentReport {
                    observations: vec![],
                    error: Some(error),
                }));
            }
        };
        let target = &saved.prepared.target;
        let previous: Vec<_> = record
            .deployments
            .iter()
            .filter(|d| !d.target.same_identity(target))
            .map(|d| d.target.clone())
            .collect();
        // Cross-route migration needs an explicit multi-PEP contract. Never
        // reinterpret old cleanup bytes with the newly configured Client.
        if previous.iter().any(|old| old.route != target.route) {
            return Ok(Some(DeploymentReport {
                observations: vec![],
                error: Some(Failure::new(
                    FailureKind::Rejected,
                    "RECONCILE_TARGET_UNAVAILABLE",
                )),
            }));
        }
        let mut touched = previous.clone();
        touched.push(target.clone());
        if !self.state.register(
            &ExpectedBinding::from_binding(&record.binding),
            Some(&saved),
            &touched,
        )? {
            return Ok(None);
        }
        let report = if saved.is_update {
            client.update(&previous, &saved.prepared)
        } else {
            client.create(&saved.prepared)
        };
        let required: Vec<_> = touched
            .iter()
            .map(|t| Observation {
                target: t.clone(),
                presence: if t.same_identity(target) {
                    Presence::Present
                } else {
                    Presence::Absent
                },
            })
            .collect();
        Ok(Some(validate_report(report, &required)))
    }

    fn prepare(
        &self,
        record: &ReconcileRecord,
    ) -> Result<(PreparedAttempt, Arc<dyn TargetDeploymentClient>), Failure> {
        let plan = match self.adapter.translate(&record.binding.spec) {
            Ok(TranslationOutcome::Translated(plan)) => plan,
            Ok(TranslationOutcome::Rejected(rejection)) => {
                return Err(Failure::new(FailureKind::Rejected, &rejection.code));
            }
            Err(fault) => return Err(retryable(&fault.code)),
        };
        let client = self
            .client_factories
            .get(&self.apply_route)
            .ok_or_else(|| retryable("RECONCILE_TARGET_UNAVAILABLE"))?
            .open()
            .map_err(|error| Failure::new(error.kind, &error.code))?;
        let prepared = client
            .prepare_apply(&plan)
            .map_err(|error| Failure::new(error.kind, &error.code))?;
        if prepared.target.route != self.apply_route
            || prepared.target.id.is_empty()
            || prepared.format.is_empty()
        {
            return Err(Failure::new(
                FailureKind::Rejected,
                "RECONCILE_INVALID_PREPARED",
            ));
        }
        if record.deployments.iter().any(|d| {
            d.revision == record.binding.spec.binding_revision
                && d.target.same_identity(&prepared.target)
                && d.target != prepared.target
        }) {
            return Err(Failure::new(
                FailureKind::Rejected,
                "RECONCILE_TARGET_IDENTITY_CHANGED",
            ));
        }
        Ok((
            PreparedAttempt {
                revision: record.binding.spec.binding_revision,
                is_update: !record.deployments.is_empty(),
                prepared,
            },
            client,
        ))
    }

    fn delete(&self, record: &ReconcileRecord) -> Result<Option<DeploymentReport>, StoreError> {
        let targets: Vec<_> = record
            .deployments
            .iter()
            .map(|d| d.target.clone())
            .collect();
        if !self.state.register(
            &ExpectedBinding::from_binding(&record.binding),
            None,
            &targets,
        )? {
            return Ok(None);
        }
        let mut groups: BTreeMap<&str, Vec<TargetRef>> = BTreeMap::new();
        for target in &targets {
            groups
                .entry(&target.route)
                .or_default()
                .push(target.clone());
        }
        let mut aggregate = DeploymentReport {
            observations: vec![],
            error: None,
        };
        for (route, targets) in groups {
            let report = self.client_factories.get(route).map_or_else(
                || failed("RECONCILE_TARGET_UNAVAILABLE"),
                |factory| match factory.open() {
                    Ok(client) => client.delete(&targets),
                    Err(error) => DeploymentReport {
                        observations: vec![],
                        error: Some(error),
                    },
                },
            );
            let required: Vec<_> = targets
                .into_iter()
                .map(|target| Observation {
                    target,
                    presence: Presence::Absent,
                })
                .collect();
            let report = validate_report(report, &required);
            aggregate.observations.extend(report.observations);
            if let Some(error) = report.error
                && (aggregate.error.is_none() || error.kind == FailureKind::Rejected)
            {
                aggregate.error = Some(error);
            }
        }
        Ok(Some(aggregate))
    }

    fn outcome(
        &self,
        record: &ReconcileRecord,
        report: DeploymentReport,
        schedule: &crate::AttemptSchedule,
    ) -> Result<AttemptOutcome, StoreError> {
        let status = record.binding.status.phase;
        let policy = self.retry;
        let (next_status, next_attempt_at) = match &report.error {
            None => (status.complete_reconcile(), None),
            Some(error)
                if error.kind == FailureKind::Retryable
                    && schedule.attempts_started < policy.max_attempts =>
            {
                (
                    status.retry_reconcile(),
                    Some(
                        self.clock
                            .now_ms()
                            .saturating_add(crate::retry::delay(policy, schedule.attempts_started)),
                    ),
                )
            }
            Some(_) => (status.fail_reconcile(), None),
        };
        let next_status = next_status.map_err(|_| StoreError::Invalid)?;
        Ok(AttemptOutcome {
            expected: ExpectedBinding::from_binding(&record.binding),
            observations: report.observations,
            next_status,
            next_attempt_at,
            error: report.error,
        })
    }

    fn commit(
        &self,
        outcome: &AttemptOutcome,
        completion: &mut Option<crate::model::PendingWrite>,
    ) -> Result<Disposition, StoreError> {
        if !self.state.finish(outcome, completion)? {
            return Ok(Disposition::Superseded);
        }
        if let Some(at) = outcome.next_attempt_at {
            Ok(Disposition::RetryAt { at })
        } else if let Some(error) = &outcome.error {
            Ok(Disposition::Failed {
                error: error.clone(),
            })
        } else {
            Ok(Disposition::Completed)
        }
    }
}

fn retryable(code: &str) -> Failure {
    Failure::new(FailureKind::Retryable, code)
}

fn failed(code: &str) -> DeploymentReport {
    DeploymentReport {
        observations: vec![],
        error: Some(retryable(code)),
    }
}

fn validate_report(mut report: DeploymentReport, required: &[Observation]) -> DeploymentReport {
    for (index, observed) in report.observations.iter().enumerate() {
        if !required.iter().any(|r| r.target == observed.target)
            || report.observations[..index]
                .iter()
                .any(|o| o.target.same_identity(&observed.target))
        {
            // An invalid report provides no trustworthy clearance evidence.
            return failed("RECONCILE_INVALID_REPORT");
        }
    }
    report.error = report.error.map(|e| Failure::new(e.kind, &e.code));
    if report.error.is_none() && required.iter().any(|r| !report.observations.contains(r)) {
        report.error = Some(retryable("RECONCILE_UNCONFIRMED"));
    }
    report
}

#[cfg(test)]
#[path = "reconciler_tests.rs"]
mod tests;
