use std::sync::Arc;

use asc_pap::PapRepository;

use super::ReconcileState;
use crate::*;
use asc_policy_types::binding::{BindingStatus, BindingView};

use crate::test_store::TestAdmission;

fn initial() -> ReconcileRecord {
    ReconcileRecord {
        binding: BindingView {
            spec: serde_json::from_str(include_str!(
                "../../asc-policy-types/tests/fixtures/prepared-binding.json"
            ))
            .unwrap(),
            status: (BindingStatus::PendingApply).into(),
        },
        deployments: vec![],
    }
}

fn policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 3,
        base_delay_ms: 100,
        max_delay_ms: 150,
    }
}

fn saved(record: &ReconcileRecord) -> PreparedAttempt {
    PreparedAttempt {
        revision: record.binding.spec.binding_revision,
        is_update: false,
        prepared: PreparedApply {
            target: TargetRef {
                route: "test".into(),
                id: "opaque".into(),
                cleanup: vec![255],
            },
            format: "test.request.v1".into(),
            content: vec![0, 255],
        },
    }
}

fn claimed() -> (TestRepository, ReconcileRecord, PreparedAttempt) {
    let record = initial();
    let repository = TestRepository::with_binding_states(vec![record.clone()]).unwrap();
    let record = repository
        .claim(&ExpectedBinding::from_binding(&record.binding), 0, policy())
        .unwrap()
        .unwrap();
    let saved = saved(&record);
    assert!(
        repository
            .register(
                &ExpectedBinding::from_binding(&record.binding),
                Some(&saved),
                std::slice::from_ref(&saved.prepared.target)
            )
            .unwrap()
    );
    (repository, record, saved)
}

fn outcome(record: &ReconcileRecord, observations: Vec<Observation>) -> AttemptOutcome {
    AttemptOutcome {
        expected: ExpectedBinding::from_binding(&record.binding),
        observations,
        next_status: BindingStatus::Ready,
        next_attempt_at: None,
        error: None,
    }
}

#[test]
fn bounded_cas_contention_is_distinct_from_storage_failure_and_preserves_state() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Conflicts {
        inner: Arc<asc_pap_repository_memory::ProcessLocalPapRepository>,
        calls: AtomicUsize,
    }
    impl BindingStateRepository for Conflicts {
        fn get_binding_state(
            &self,
            id: &asc_foundation_types::ResourceId,
        ) -> Result<Option<BindingStateSnapshot>, StoreError> {
            self.inner.get_binding_state(id)
        }
        fn compare_exchange_binding_state(
            &self,
            expected: &BindingStateSnapshot,
            write: &BindingStateWrite,
        ) -> Result<WriteResult, StoreError> {
            // Exercise the retry bound deterministically; this does not claim
            // that ordinary PAP requests can produce this many conflicts.
            if self.calls.fetch_add(1, Ordering::SeqCst) < 16 {
                Ok(WriteResult::Conflict)
            } else {
                self.inner.compare_exchange_binding_state(expected, write)
            }
        }
    }
    for registration in [true, false] {
        let (repo, record, saved) = claimed();
        let before = repo.read(&record.binding.spec.binding_id).unwrap();
        let wrapped = Arc::new(Conflicts {
            inner: repo.inner.clone(),
            calls: AtomicUsize::new(0),
        });
        let state = ReconcileState {
            repository: wrapped.clone(),
        };
        let mut completion = None;
        let result = outcome(
            &record,
            vec![Observation {
                target: saved.prepared.target.clone(),
                presence: Presence::Present,
            }],
        );
        let mut attempt = || {
            if registration {
                state.register(
                    &result.expected,
                    Some(&saved),
                    std::slice::from_ref(&saved.prepared.target),
                )
            } else {
                state.finish(&result, &mut completion)
            }
        };
        assert_eq!(attempt(), Err(StoreError::Contended));
        assert_eq!(wrapped.calls.load(Ordering::SeqCst), 16);
        assert_eq!(repo.read(&record.binding.spec.binding_id).unwrap(), before);
        assert_eq!(attempt(), Ok(true));
        assert_eq!(wrapped.calls.load(Ordering::SeqCst), 17);
        assert!(completion.is_none());
        let mut expected = before.unwrap();
        if !registration {
            expected.binding.status.phase = BindingStatus::Ready;
            expected.deployments[0].presence = Presence::Present;
            expected.deployments[0].last_confirmed = Some(Presence::Present);
        }
        assert_eq!(
            repo.read(&record.binding.spec.binding_id).unwrap(),
            Some(expected)
        );
    }
}

#[test]
fn invalid_partial_report_rolls_back_all_observations_and_lifecycle() {
    let (repository, record, saved) = claimed();
    let id = &record.binding.spec.binding_id;
    let before = repository.read(id).unwrap();
    let mut foreign = saved.prepared.target.clone();
    foreign.id = "never-registered".into();
    let result = outcome(
        &record,
        vec![
            Observation {
                target: saved.prepared.target,
                presence: Presence::Absent,
            },
            Observation {
                target: foreign,
                presence: Presence::Absent,
            },
        ],
    );
    assert_eq!(repository.finish(&result), Err(StoreError::Invalid));
    assert_eq!(repository.read(id).unwrap(), before);
}

#[test]
fn registration_failure_does_not_partially_save_prepared_or_targets() {
    let record = initial();
    let repository = TestRepository::with_binding_states(vec![record.clone()]).unwrap();
    let record = repository
        .claim(&ExpectedBinding::from_binding(&record.binding), 0, policy())
        .unwrap()
        .unwrap();
    let saved = saved(&record);
    let mut foreign = saved.prepared.target.clone();
    foreign.id = "unregistered-old".into();
    let before = repository.read(&record.binding.spec.binding_id).unwrap();
    assert_eq!(
        repository.register(
            &ExpectedBinding::from_binding(&record.binding),
            Some(&saved),
            &[saved.prepared.target.clone(), foreign]
        ),
        Err(StoreError::Invalid)
    );
    assert_eq!(
        repository.read(&record.binding.spec.binding_id).unwrap(),
        before
    );
}

#[test]
fn no_confirmation_cannot_complete_ready_or_deleted() {
    let (repository, record, _) = claimed();
    let before = repository.read(&record.binding.spec.binding_id).unwrap();
    assert_eq!(
        repository.finish(&outcome(&record, vec![])),
        Err(StoreError::Invalid)
    );
    assert_eq!(
        repository.read(&record.binding.spec.binding_id).unwrap(),
        before
    );
    let mut deletion = record.binding.clone();
    deletion.status = BindingStatus::PendingDelete.into();
    assert!(
        repository
            .compare_exchange_reconcile_intent(
                &ExpectedBinding::from_binding(&record.binding),
                &deletion
            )
            .unwrap()
    );
    let deleting = repository
        .claim(&ExpectedBinding::from_binding(&deletion), 0, policy())
        .unwrap()
        .unwrap();
    let mut result = outcome(&deleting, vec![]);
    result.next_status = BindingStatus::Deleted;
    let before = repository.read(&record.binding.spec.binding_id).unwrap();
    assert_eq!(repository.finish(&result), Err(StoreError::Invalid));
    assert_eq!(
        repository.read(&record.binding.spec.binding_id).unwrap(),
        before
    );
}

#[test]
fn due_time_and_attempts_are_caller_owned_while_claim_clears_error() {
    let mut record = initial();
    record.binding.status.error = Some(Failure::new(FailureKind::Retryable, "RETRY"));
    let repository = TestRepository::with_binding_states(vec![record.clone()]).unwrap();
    repository.schedule.lock().unwrap().next_attempt_at = Some(100);
    let expected = ExpectedBinding::from_binding(&record.binding);
    let other_policy = RetryPolicy {
        max_attempts: 10,
        base_delay_ms: 200,
        max_delay_ms: 400,
    };
    assert_eq!(repository.claim(&expected, 99, other_policy).unwrap(), None);
    assert_eq!(repository.read(&expected.id).unwrap(), Some(record));
    let claimed = repository
        .claim(&expected, 100, other_policy)
        .unwrap()
        .unwrap();
    assert_eq!(repository.schedule.lock().unwrap().attempts_started, 1);
    assert_eq!(claimed.binding.status.error, None);
    assert_eq!(repository.schedule.lock().unwrap().next_attempt_at, None);
    assert_eq!(
        repository.claim(&expected, 100, other_policy).unwrap(),
        None
    );
    assert_eq!(repository.read(&expected.id).unwrap(), Some(claimed));
}

#[test]
fn repeated_admitted_intent_preserves_budget_and_new_revision_keeps_targets() {
    let (repository, record, saved) = claimed();
    let id = &record.binding.spec.binding_id;
    assert!(
        repository
            .finish(&outcome(
                &record,
                vec![Observation {
                    target: saved.prepared.target,
                    presence: Presence::Present
                }]
            ))
            .unwrap()
    );
    let ready = repository.read(id).unwrap().unwrap();
    let mut next = ready.binding.clone();
    next.status = BindingStatus::PendingApply.into();
    next.spec.binding_revision = next.spec.binding_revision.checked_next().unwrap();
    // Existing PAP repository writes and worker transactions share one store.
    next.spec.scope.revision = next.spec.scope.revision.checked_next().unwrap();
    repository
        .update_binding(Some(&ready.binding), &next)
        .unwrap();
    let pending = repository.read(id).unwrap().unwrap();
    assert_eq!(pending.deployments, ready.deployments);
    assert_eq!(pending.binding.status.error, None);
    assert_eq!(repository.get_binding(id).unwrap(), next);
    assert!(
        repository
            .compare_exchange_reconcile_intent(&ExpectedBinding::from_binding(&next), &next)
            .unwrap()
    );
    assert_eq!(repository.read(id).unwrap(), Some(pending));
}

#[test]
fn retry_noop_does_not_reset_attempts_and_bad_cas_does_not_mutate() {
    let (repository, record, _) = claimed();
    let mut retry = outcome(&record, vec![]);
    retry.next_status = BindingStatus::PendingApply;
    retry.next_attempt_at = Some(100);
    retry.error = Some(Failure::new(FailureKind::Retryable, "UNKNOWN"));
    repository.finish(&retry).unwrap();
    let pending = repository
        .read(&record.binding.spec.binding_id)
        .unwrap()
        .unwrap();
    let expected = ExpectedBinding::from_binding(&pending.binding);
    assert!(
        repository
            .compare_exchange_reconcile_intent(&expected, &pending.binding)
            .unwrap()
    );
    assert_eq!(
        repository.read(&expected.id).unwrap(),
        Some(pending.clone())
    );
    let mut wrong = expected.clone();
    wrong.revision = wrong.revision.checked_next().unwrap();
    assert_eq!(repository.claim(&wrong, 100, policy()).unwrap(), None);
    wrong = expected.clone();
    wrong.status = BindingStatus::PendingDelete;
    assert!(
        !repository
            .compare_exchange_reconcile_intent(&wrong, &pending.binding)
            .unwrap()
    );
    assert_eq!(repository.read(&expected.id).unwrap(), Some(pending));
}

#[test]
fn bounded_codes_and_backoff_reject_invalid_configuration() {
    assert_eq!(
        Failure::new(FailureKind::Rejected, "secret body\n").code,
        "RECONCILE_INTERNAL_ERROR"
    );
    assert_eq!(crate::retry::delay(policy(), 1), 100);
    assert_eq!(crate::retry::delay(policy(), 2), 150);
    assert_eq!(crate::retry::delay(policy(), u32::MAX), 150);
    let invalid = RetryPolicy {
        max_attempts: 0,
        ..policy()
    };
    assert_eq!(crate::retry::validate(invalid), Err(StoreError::Invalid));
}

struct TestRepository {
    inner: Arc<asc_pap_repository_memory::ProcessLocalPapRepository>,
    state: ReconcileState,
    schedule: std::sync::Mutex<AttemptSchedule>,
    completion: std::sync::Mutex<Option<crate::model::PendingWrite>>,
}
impl std::ops::Deref for TestRepository {
    type Target = asc_pap_repository_memory::ProcessLocalPapRepository;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl TestRepository {
    fn with_binding_states(records: Vec<ReconcileRecord>) -> Result<Self, StoreError> {
        let inner = Arc::new(
            asc_pap_repository_memory::ProcessLocalPapRepository::with_binding_states(records)?,
        );
        Ok(Self {
            state: ReconcileState {
                repository: inner.clone(),
            },
            inner,
            completion: std::sync::Mutex::new(None),
            schedule: std::sync::Mutex::new(AttemptSchedule::default()),
        })
    }
    fn read(
        &self,
        id: &asc_foundation_types::ResourceId,
    ) -> Result<Option<ReconcileRecord>, StoreError> {
        self.state.read(id)
    }
    fn claim(
        &self,
        expected: &ExpectedBinding,
        now: u64,
        policy: RetryPolicy,
    ) -> Result<Option<ReconcileRecord>, StoreError> {
        let Some(record) = self.read(&expected.id)? else {
            return Ok(None);
        };
        if !expected.matches(&record.binding) {
            return Ok(None);
        }
        self.state
            .claim(&record, now, policy, &mut self.schedule.lock().unwrap())
    }
    fn register(
        &self,
        expected: &ExpectedBinding,
        prepared: Option<&PreparedAttempt>,
        targets: &[TargetRef],
    ) -> Result<bool, StoreError> {
        self.state.register(expected, prepared, targets)
    }
    fn finish(&self, outcome: &AttemptOutcome) -> Result<bool, StoreError> {
        self.state
            .finish(outcome, &mut self.completion.lock().unwrap())
    }
}

#[test]
fn deployment_only_registration_preserves_concurrent_status_error_without_conflict() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ConcurrentWrite {
        inner: Arc<asc_pap_repository_memory::ProcessLocalPapRepository>,
        calls: AtomicUsize,
    }
    impl BindingStateRepository for ConcurrentWrite {
        fn get_binding_state(
            &self,
            id: &asc_foundation_types::ResourceId,
        ) -> Result<Option<BindingStateSnapshot>, StoreError> {
            self.inner.get_binding_state(id)
        }
        fn compare_exchange_binding_state(
            &self,
            expected: &BindingStateSnapshot,
            write: &BindingStateWrite,
        ) -> Result<WriteResult, StoreError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let mut concurrent = expected.clone();
                concurrent.binding.status.error = Some(Failure::new(
                    FailureKind::Retryable,
                    "CONCURRENT_DIAGNOSTIC",
                ));
                assert_eq!(
                    self.inner.compare_exchange_binding_state(
                        expected,
                        &BindingStateWrite::new(concurrent)
                    )?,
                    WriteResult::Applied
                );
            }
            self.inner.compare_exchange_binding_state(expected, write)
        }
    }
    let (repo, record, saved) = claimed();
    let wrapped = Arc::new(ConcurrentWrite {
        inner: repo.inner.clone(),
        calls: AtomicUsize::new(0),
    });
    let state = ReconcileState {
        repository: wrapped.clone(),
    };
    assert!(
        state
            .register(
                &ExpectedBinding::from_binding(&record.binding),
                Some(&saved),
                std::slice::from_ref(&saved.prepared.target)
            )
            .unwrap()
    );
    let current = state
        .read(&record.binding.spec.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        current.binding.status.error,
        Some(Failure::new(
            FailureKind::Retryable,
            "CONCURRENT_DIAGNOSTIC"
        ))
    );
    assert_eq!(current.deployments[0].target, saved.prepared.target);
    assert_eq!(current.deployments[0].presence, Presence::Unknown);
    assert_eq!(wrapped.calls.load(Ordering::SeqCst), 1);
}
