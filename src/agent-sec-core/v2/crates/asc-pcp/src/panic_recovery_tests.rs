//! Panic injection across claim, target I/O and completion transaction boundaries.
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::*;
use asc_foundation_types::ResourceId;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_types::binding::{BindingStatus, PreparedBinding};
use asc_policy_types::target::{AdapterFault, TargetBindingPlan, TranslationOutcome};

use crate::test_store as store;
use store::{TestAdmission, TestStore};

const POLICY: RetryPolicy = RetryPolicy {
    max_attempts: 3,
    base_delay_ms: 100,
    max_delay_ms: 150,
};

fn initial() -> ReconcileRecord {
    ReconcileRecord {
        binding: asc_policy_types::binding::BindingView {
            spec: serde_json::from_str(include_str!(
                "../../asc-policy-types/tests/fixtures/prepared-binding.json"
            ))
            .unwrap(),
            status: (BindingStatus::PendingApply).into(),
        },
        deployments: vec![],
    }
}

fn prepared() -> PreparedApply {
    PreparedApply {
        target: TargetRef {
            route: "test".into(),
            id: "opaque".into(),
            cleanup: vec![1],
        },
        format: "request.v1".into(),
        content: vec![2],
    }
}

struct Rig {
    repo: ProcessLocalPapRepository,
    panic_at: &'static str,
    fired: AtomicBool,
    fail_finish: AtomicBool,
    supersede: bool,
    trace: Mutex<Vec<&'static str>>,
}

impl Rig {
    fn point(&self, point: &'static str) {
        self.trace.lock().unwrap().push(point);
        assert!(
            point != self.panic_at || self.fired.swap(true, Ordering::SeqCst),
            "private injected panic payload"
        );
    }

    fn core(self: &Arc<Self>) -> BindingReconciler {
        let adapter = |_binding: &PreparedBinding| -> Result<TranslationOutcome, AdapterFault> {
            Ok(TranslationOutcome::Translated(TargetBindingPlan {
                format: "plan.v1".into(),
                content: vec![3],
            }))
        };
        BindingReconciler::new(
            self.clone(),
            Arc::new(adapter),
            BTreeMap::from([("test".into(), {
                let client = self.clone();
                Arc::new(move || Ok(client.clone() as Arc<dyn TargetDeploymentClient>))
                    as Arc<dyn crate::TargetDeploymentClientFactory>
            })]),
            "test".into(),
            self.clone(),
            POLICY,
        )
        .unwrap()
    }
}

impl Clock for Rig {
    fn now_ms(&self) -> u64 {
        0
    }
}
impl TargetDeploymentClient for Rig {
    fn prepare_apply(&self, _: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        self.point("prepare");
        Ok(prepared())
    }
    fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        if self.supersede {
            let current = self
                .repo
                .read(&initial().binding.spec.binding_id)
                .unwrap()
                .unwrap();
            let mut desired = current.binding.clone();
            desired.status = BindingStatus::PendingDelete.into();
            assert!(
                self.repo
                    .compare_exchange_reconcile_intent(
                        &ExpectedBinding::from_binding(&current.binding),
                        &desired
                    )
                    .unwrap()
            );
        }
        self.point("create");
        DeploymentReport {
            observations: vec![Observation {
                target: prepared.target.clone(),
                presence: Presence::Present,
            }],
            error: None,
        }
    }
    fn update(&self, _: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        self.create(prepared)
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        self.point("delete");
        DeploymentReport {
            observations: targets
                .iter()
                .map(|target| Observation {
                    target: target.clone(),
                    presence: Presence::Absent,
                })
                .collect(),
            error: None,
        }
    }
}
impl BindingStateRepository for Rig {
    fn get_binding_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError> {
        self.repo.get_binding_state(id)
    }
    fn compare_exchange_binding_state(
        &self,
        expected: &BindingStateSnapshot,
        write: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError> {
        let phase = store::write_phase(expected, write);
        if phase == "claim" {
            self.point("before_claim");
        }
        if phase == "finish" {
            self.point("before_finish");
            if self.fail_finish.load(Ordering::SeqCst) {
                return Err(StoreError::Unavailable);
            }
        }
        let result = self.repo.compare_exchange_binding_state(expected, write)?;
        if self.panic_at == "fail_after_delete"
            && write.next.is_none()
            && !self.fired.swap(true, Ordering::SeqCst)
        {
            return Err(StoreError::Unavailable);
        }
        self.point(match phase {
            "claim" => "after_claim",
            "register" => "register",
            _ => "after_finish",
        });
        Ok(result)
    }
}

fn rig(
    record: ReconcileRecord,
    panic_at: &'static str,
    fail_finish: bool,
    supersede: bool,
) -> Arc<Rig> {
    Arc::new(Rig {
        repo: ProcessLocalPapRepository::with_binding_states(vec![record]).unwrap(),
        panic_at,
        fired: AtomicBool::new(false),
        fail_finish: AtomicBool::new(fail_finish),
        supersede,
        trace: Mutex::new(vec![]),
    })
}

fn expected_failure(mut record: ReconcileRecord, registered: bool) -> ReconcileRecord {
    record.binding.status.phase = record
        .binding
        .status
        .start_reconcile()
        .unwrap()
        .fail_reconcile()
        .unwrap();
    record.binding.status.error = Some(Failure::new(
        FailureKind::Rejected,
        "RECONCILE_WORKER_PANICKED",
    ));
    if registered {
        record.deployments = vec![Deployment {
            target: prepared().target,
            revision: record.binding.spec.binding_revision,
            presence: Presence::Unknown,
            last_confirmed: None,
        }];
    }
    record
}

#[test]
fn panics_fail_claimed_apply_without_losing_cleanup() {
    let mut schedule = AttemptSchedule::default();
    for point in ["after_claim", "prepare", "register", "create"] {
        let input = initial();
        let id = input.binding.spec.binding_id.clone();
        let rig = rig(input.clone(), point, false, false);
        let core = rig.core();
        assert!(catch_unwind(AssertUnwindSafe(|| core.reconcile(&id, &mut schedule))).is_err());
        assert_eq!(
            rig.read(&id).unwrap().unwrap(),
            expected_failure(input, matches!(point, "register" | "create"))
        );
        assert_eq!(
            core.reconcile(&id, &mut schedule).unwrap(),
            Disposition::Skipped
        );
        let mut expected_trace = vec!["before_claim", "after_claim"];
        if point != "after_claim" {
            expected_trace.push("prepare");
        }
        if matches!(point, "register" | "create") {
            expected_trace.push("register");
        }
        if point == "create" {
            expected_trace.push("create");
        }
        expected_trace.extend(["before_finish", "after_finish"]);
        assert_eq!(*rig.trace.lock().unwrap(), expected_trace);
    }
}

#[test]
fn panic_before_claim_does_not_fail_unclaimed_intent() {
    let mut schedule = AttemptSchedule::default();
    let input = initial();
    let id = input.binding.spec.binding_id.clone();
    let rig = rig(input.clone(), "before_claim", false, false);
    assert!(
        catch_unwind(AssertUnwindSafe(|| rig
            .core()
            .reconcile(&id, &mut schedule)))
        .is_err()
    );
    assert_eq!(rig.read(&id).unwrap(), Some(input));
    assert_eq!(
        rig.core().reconcile(&id, &mut schedule).unwrap(),
        Disposition::Completed
    );
}

#[test]
fn panic_without_committed_result_recovers_budget_and_schedules_fresh_attempt() {
    let mut schedule = AttemptSchedule::default();
    let input = initial();
    let id = input.binding.spec.binding_id.clone();
    let rig = rig(input.clone(), "create", true, false);
    assert!(
        catch_unwind(AssertUnwindSafe(|| rig
            .core()
            .reconcile(&id, &mut schedule)))
        .is_err()
    );
    let mut running = expected_failure(input.clone(), true);
    running.binding.status.phase = BindingStatus::Applying;
    running.binding.status.error = None;
    assert_eq!(rig.read(&id).unwrap(), Some(running));
    assert_eq!(
        rig.core().reconcile(&id, &mut schedule),
        Err(StoreError::Unavailable)
    );
    rig.fail_finish.store(false, Ordering::SeqCst);
    assert_eq!(
        rig.core().reconcile(&id, &mut schedule).unwrap(),
        Disposition::Skipped
    );
    let recovered = rig.read(&id).unwrap().unwrap();
    assert_eq!(recovered.binding.status.phase, BindingStatus::PendingApply);
    assert_eq!(schedule.attempts_started, 1);
    assert!(schedule.next_attempt_at.is_some());
    assert_eq!(
        recovered.deployments,
        expected_failure(input, true).deployments
    );
    assert_eq!(
        rig.trace
            .lock()
            .unwrap()
            .iter()
            .filter(|&&p| p == "create")
            .count(),
        1
    );
}

#[test]
fn panic_during_completion_preserves_success_and_never_repeats_create() {
    let mut schedule = AttemptSchedule::default();
    for point in ["before_finish", "after_finish"] {
        let input = initial();
        let id = input.binding.spec.binding_id.clone();
        let rig = rig(input.clone(), point, false, false);
        assert!(
            catch_unwind(AssertUnwindSafe(|| rig
                .core()
                .reconcile(&id, &mut schedule)))
            .is_err()
        );
        let mut expected = expected_failure(input, true);
        expected.binding.status.phase = BindingStatus::Ready;
        expected.binding.status.error = None;
        expected.deployments[0].presence = Presence::Present;
        expected.deployments[0].last_confirmed = Some(Presence::Present);
        assert_eq!(rig.read(&id).unwrap(), Some(expected));
        assert_eq!(
            rig.core().reconcile(&id, &mut schedule).unwrap(),
            Disposition::Skipped
        );
        assert_eq!(
            rig.trace
                .lock()
                .unwrap()
                .iter()
                .filter(|&&p| p == "create")
                .count(),
            1
        );
    }
}

#[test]
fn panic_does_not_overwrite_new_delete_and_next_call_can_clean_up() {
    let mut schedule = AttemptSchedule::default();
    let input = initial();
    let id = input.binding.spec.binding_id.clone();
    let rig = rig(input.clone(), "create", false, true);
    assert!(
        catch_unwind(AssertUnwindSafe(|| rig
            .core()
            .reconcile(&id, &mut schedule)))
        .is_err()
    );
    let mut expected = expected_failure(input, true);
    expected.binding.status = BindingStatus::PendingDelete.into();

    assert_eq!(rig.read(&id).unwrap(), Some(expected.clone()));
    assert_eq!(
        rig.core().reconcile(&id, &mut schedule).unwrap(),
        Disposition::Completed
    );
    assert_eq!(rig.read(&id).unwrap(), None);
}

#[test]
fn delete_panic_preserves_unknown_target_and_marks_delete_failed() {
    let mut schedule = AttemptSchedule::default();
    let mut input = initial();
    input.binding.status.phase = BindingStatus::PendingDelete;
    input.deployments = vec![Deployment {
        target: prepared().target,
        revision: input.binding.spec.binding_revision,
        presence: Presence::Unknown,
        last_confirmed: Some(Presence::Present),
    }];
    let id = input.binding.spec.binding_id.clone();
    let rig = rig(input.clone(), "delete", false, false);
    assert!(
        catch_unwind(AssertUnwindSafe(|| rig
            .core()
            .reconcile(&id, &mut schedule)))
        .is_err()
    );
    assert_eq!(
        rig.read(&id).unwrap().unwrap(),
        expected_failure(input, false)
    );
    assert_eq!(
        rig.core().reconcile(&id, &mut schedule).unwrap(),
        Disposition::Skipped
    );
}

#[test]
fn delete_completion_panic_replays_receipt_after_target_removal() {
    let mut schedule = AttemptSchedule::default();
    let mut input = initial();
    input.binding.status.phase = BindingStatus::PendingDelete;
    input.deployments = vec![Deployment {
        target: prepared().target,
        revision: input.binding.spec.binding_revision,
        presence: Presence::Unknown,
        last_confirmed: Some(Presence::Present),
    }];
    let id = input.binding.spec.binding_id.clone();
    let rig = rig(input.clone(), "after_finish", false, false);
    assert!(
        catch_unwind(AssertUnwindSafe(|| rig
            .core()
            .reconcile(&id, &mut schedule)))
        .is_err()
    );
    assert_eq!(rig.read(&id).unwrap(), None);
    assert_eq!(
        rig.core().reconcile(&id, &mut schedule).unwrap(),
        Disposition::Skipped
    );
    assert_eq!(
        rig.trace
            .lock()
            .unwrap()
            .iter()
            .filter(|&&p| p == "delete")
            .count(),
        1
    );
}

#[test]
fn unknown_ids_skip_without_client_calls() {
    let mut schedule = AttemptSchedule::default();
    let rig = rig(initial(), "never", false, false);
    let core = rig.core();
    for index in 0..1000 {
        let id = ResourceId::new(format!("missing-{index}")).unwrap();
        assert_eq!(
            core.reconcile(&id, &mut schedule).unwrap(),
            Disposition::Skipped
        );
    }
    assert!(rig.trace.lock().unwrap().is_empty());
}

#[test]
fn delete_commit_response_failure_replays_absence_without_repeating_client_io() {
    let mut schedule = AttemptSchedule::default();
    let mut input = initial();
    input.binding.status.phase = BindingStatus::PendingDelete;
    input.deployments = vec![Deployment {
        target: prepared().target,
        revision: input.binding.spec.binding_revision,
        presence: Presence::Unknown,
        last_confirmed: None,
    }];
    let id = input.binding.spec.binding_id.clone();
    let rig = rig(input, "fail_after_delete", false, false);
    assert_eq!(
        rig.core().reconcile(&id, &mut schedule),
        Err(StoreError::Unavailable)
    );
    assert_eq!(rig.read(&id).unwrap(), None);
    assert_eq!(
        rig.core().reconcile(&id, &mut schedule).unwrap(),
        Disposition::Skipped
    );
    assert_eq!(
        rig.trace
            .lock()
            .unwrap()
            .iter()
            .filter(|&&p| p == "delete")
            .count(),
        1
    );
}
