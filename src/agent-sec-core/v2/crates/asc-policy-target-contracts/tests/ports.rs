use asc_policy_target_contracts::{TargetBindingAdapter, TargetDeploymentClient};
use asc_policy_types::binding::PreparedBinding;
use asc_policy_types::target::{
    DeploymentReport, Failure, FailureKind, Observation, PreparedApply, Presence,
    TargetBindingPlan, TargetRef, TranslationOutcome,
};
use serde_json::Value;

const PREPARED: &str = r#"{
  "target": {"route":"pep-a","id":"binding-7","cleanup":[0,255]},
  "format":"test.apply.v1","content":[1,2,255]
}"#;
const PARTIAL: &str = r#"{
  "observations": [
    {"target":{"route":"pep-a","id":"binding-6","cleanup":[6]},"presence":"ABSENT"},
    {"target":{"route":"pep-a","id":"binding-7","cleanup":[0,255]},"presence":"UNKNOWN"}
  ],
  "error":{"kind":"RETRYABLE","code":"TARGET_TIMEOUT"}
}"#;

#[test]
fn complete_shared_artifacts_round_trip_without_losing_partial_evidence() {
    let prepared: PreparedApply = serde_json::from_str(PREPARED).unwrap();
    let report: DeploymentReport = serde_json::from_str(PARTIAL).unwrap();
    assert_eq!(
        serde_json::to_value(&prepared).unwrap(),
        serde_json::from_str::<Value>(PREPARED).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        serde_json::from_str::<Value>(PARTIAL).unwrap()
    );
    assert_eq!(report.observations[0].presence, Presence::Absent);
    assert_eq!(report.observations[1].presence, Presence::Unknown);
    assert_eq!(report.observations[1].target, prepared.target);
    let mut malformed: Value = serde_json::from_str(PREPARED).unwrap();
    malformed["target"]["unexpected"] = true.into();
    assert!(serde_json::from_value::<PreparedApply>(malformed).is_err());
}

#[test]
fn target_identity_and_error_codes_are_pep_neutral() {
    let prepared: PreparedApply = serde_json::from_str(PREPARED).unwrap();
    let mut target = prepared.target.clone();
    target.cleanup = vec![99];
    assert!(target.same_identity(&prepared.target));
    target.route = "pep-b".into();
    assert!(!target.same_identity(&prepared.target));
    for code in ["", "private error body", "NON_ASCII_é", &"X".repeat(97)] {
        assert_eq!(
            Failure::new(FailureKind::Retryable, code),
            Failure {
                kind: FailureKind::Retryable,
                code: "RECONCILE_INTERNAL_ERROR".into(),
            }
        );
    }
    assert_eq!(
        Failure::new(FailureKind::Rejected, "PEP_REJECTED").code,
        "PEP_REJECTED"
    );
}

struct ExampleClient(PreparedApply);

impl TargetDeploymentClient for ExampleClient {
    fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        assert_eq!(plan.format, "test.plan.v1");
        assert_eq!(plan.content, [1, 2, 255]);
        Ok(self.0.clone())
    }
    fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        assert_eq!(prepared, &self.0);
        DeploymentReport {
            observations: vec![Observation {
                target: prepared.target.clone(),
                presence: Presence::Present,
            }],
            error: None,
        }
    }
    fn update(&self, previous: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        assert!(previous.is_empty());
        self.create(prepared)
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        assert_eq!(targets, std::slice::from_ref(&self.0.target));
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

#[test]
fn ports_are_object_safe_without_any_reconciler_or_concrete_pep_dependency() {
    let binding: PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    let translate = |input: &PreparedBinding| {
        assert_eq!(input, &binding);
        Ok(TranslationOutcome::Translated(TargetBindingPlan {
            format: "test.plan.v1".into(),
            content: vec![1, 2, 255],
        }))
    };
    let adapter: &dyn TargetBindingAdapter = &translate;
    let TranslationOutcome::Translated(plan) = adapter.translate(&binding).unwrap() else {
        panic!("fixture must translate");
    };
    let first: PreparedApply = serde_json::from_str(PREPARED).unwrap();
    let mut second = first.clone();
    second.target.route = "pep-b".into();
    second.target.id = binding.binding_id.to_string();
    let clients: Vec<Box<dyn TargetDeploymentClient>> = vec![
        Box::new(ExampleClient(first)),
        Box::new(ExampleClient(second)),
    ];
    for client in clients {
        let prepared = client.prepare_apply(&plan).unwrap();
        assert_eq!(
            client.create(&prepared).observations[0].presence,
            Presence::Present
        );
        assert_eq!(
            client.update(&[], &prepared).observations[0].presence,
            Presence::Present
        );
        assert_eq!(
            client.delete(&[prepared.target]).observations[0].presence,
            Presence::Absent
        );
    }
}
