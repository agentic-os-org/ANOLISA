use super::*;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_types::binding::BindingView;

#[test]
fn invalid_outcome_transitions_return_invalid_without_producing_a_write() {
    struct TestClock;
    impl Clock for TestClock {
        fn now_ms(&self) -> u64 {
            0
        }
    }
    let core = BindingReconciler::new(
        Arc::new(ProcessLocalPapRepository::default()),
        Arc::new(|_: &asc_policy_types::binding::PreparedBinding| {
            panic!("outcome validation must not call the adapter")
        }),
        BTreeMap::from([(
            "test".into(),
            Arc::new(|| -> Result<Arc<dyn TargetDeploymentClient>, Failure> {
                panic!("outcome validation must not open a client")
            }) as Arc<dyn TargetDeploymentClientFactory>,
        )]),
        "test".into(),
        Arc::new(TestClock),
        RetryPolicy {
            max_attempts: 3,
            base_delay_ms: 100,
            max_delay_ms: 200,
        },
    )
    .unwrap();
    for status in [
        BindingStatus::PendingApply,
        BindingStatus::Ready,
        BindingStatus::ApplyFailed,
        BindingStatus::PendingDelete,
        BindingStatus::DeleteFailed,
        BindingStatus::Deleted,
    ] {
        let record = ReconcileRecord {
            binding: BindingView {
                spec: serde_json::from_str(include_str!(
                    "../../asc-policy-types/tests/fixtures/prepared-binding.json"
                ))
                .unwrap(),
                status: status.into(),
            },
            deployments: vec![],
        };
        for error in [
            None,
            Some(Failure::new(FailureKind::Retryable, "TEST_RETRY")),
            Some(Failure::new(FailureKind::Rejected, "TEST_REJECTED")),
        ] {
            assert!(matches!(
                core.outcome(
                    &record,
                    DeploymentReport {
                        observations: vec![],
                        error
                    },
                    &crate::AttemptSchedule::default()
                ),
                Err(StoreError::Invalid)
            ));
        }
    }
}
