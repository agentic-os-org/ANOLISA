//! Client initialization is attempt-local and uses the normal retry bookkeeping.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_pcp::*;
use asc_policy_types::binding::{BindingStatus, BindingView, PreparedBinding};
use asc_policy_types::target::{TargetBindingPlan, TranslationOutcome};

#[derive(Default)]
struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Session(u64);
impl TargetDeploymentClient for Session {
    fn prepare_apply(&self, _: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        Ok(PreparedApply {
            target: target("new"),
            format: "test.v1".into(),
            content: self.0.to_le_bytes().to_vec(),
        })
    }
    fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        // Opening another Client between preparation and I/O would break this.
        assert_eq!(prepared.content, self.0.to_le_bytes());
        DeploymentReport {
            observations: vec![Observation {
                target: prepared.target.clone(),
                presence: Presence::Present,
            }],
            error: None,
        }
    }
    fn update(&self, previous: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        assert_eq!(previous, &[target("old")]);
        let mut report = self.create(prepared);
        report
            .observations
            .extend(self.delete(previous).observations);
        report
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        assert_eq!(targets, &[target("old")]);
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

fn target(id: &str) -> TargetRef {
    TargetRef {
        route: "test".into(),
        id: id.into(),
        cleanup: vec![1],
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn initialization_failure_retries_apply_update_and_delete_without_losing_target_responsibility() {
    for (status, has_previous) in [
        (BindingStatus::PendingApply, false),
        (BindingStatus::PendingApply, true),
        (BindingStatus::PendingDelete, true),
    ] {
        let spec: PreparedBinding = serde_json::from_str(include_str!(
            "../../asc-policy-types/tests/fixtures/prepared-binding.json"
        ))
        .unwrap();
        let mut schedule = AttemptSchedule::default();
        let id = spec.binding_id.clone();
        let previous = Deployment {
            target: target("old"),
            revision: spec.binding_revision,
            presence: Presence::Present,
            last_confirmed: Some(Presence::Present),
        };
        let initial = ReconcileRecord {
            binding: BindingView {
                spec: spec.clone(),
                status: status.into(),
            },
            deployments: if has_previous { vec![previous] } else { vec![] },
        };
        let repo = Arc::new(
            ProcessLocalPapRepository::with_binding_states(vec![initial.clone()]).unwrap(),
        );
        let opens = Arc::new(AtomicU64::new(0));
        let factory: Arc<dyn TargetDeploymentClientFactory> = {
            let opens = opens.clone();
            Arc::new(move || {
                let attempt = opens.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt == 1 {
                    Err(Failure::new(
                        FailureKind::Retryable,
                        "TEST_CREDENTIAL_UNAVAILABLE",
                    ))
                } else {
                    Ok(Arc::new(Session(attempt)) as Arc<dyn TargetDeploymentClient>)
                }
            })
        };
        let clock = Arc::new(TestClock::default());
        let retry = RetryPolicy {
            max_attempts: 3,
            base_delay_ms: 100,
            max_delay_ms: 200,
        };
        let core = BindingReconciler::new(
            repo.clone(),
            Arc::new(|_: &PreparedBinding| {
                Ok(TranslationOutcome::Translated(TargetBindingPlan {
                    format: "test.v1".into(),
                    content: vec![],
                }))
            }),
            BTreeMap::from([("test".into(), factory)]),
            "test".into(),
            clock.clone(),
            retry,
        )
        .unwrap();
        assert_eq!(
            opens.load(Ordering::SeqCst),
            0,
            "registration must not initialize a Client"
        );
        assert_eq!(
            core.reconcile(&id, &mut schedule).unwrap(),
            Disposition::RetryAt { at: 100 }
        );
        let mut expected = initial;
        expected.binding.status.error = Some(Failure::new(
            FailureKind::Retryable,
            "TEST_CREDENTIAL_UNAVAILABLE",
        ));
        if status == BindingStatus::PendingDelete {
            expected.deployments[0].presence = Presence::Unknown;
        }
        assert_eq!(repo.get_binding_state(&id).unwrap(), Some(expected));
        assert_eq!(
            core.reconcile(&id, &mut schedule).unwrap(),
            Disposition::Skipped
        );
        assert_eq!(
            opens.load(Ordering::SeqCst),
            1,
            "not-due calls must not initialize a Client"
        );
        clock.0.store(100, Ordering::SeqCst);
        assert_eq!(
            core.reconcile(&id, &mut schedule).unwrap(),
            Disposition::Completed
        );
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        let expected = (status != BindingStatus::PendingDelete).then_some(ReconcileRecord {
            binding: {
                let mut binding = BindingView {
                    spec: spec.clone(),
                    status: (BindingStatus::Ready).into(),
                };
                binding.status.error = None;
                binding
            },

            deployments: vec![Deployment {
                target: target("new"),
                revision: spec.binding_revision,
                presence: Presence::Present,
                last_confirmed: Some(Presence::Present),
            }],
        });
        assert_eq!(repo.get_binding_state(&id).unwrap(), expected);
    }
}
