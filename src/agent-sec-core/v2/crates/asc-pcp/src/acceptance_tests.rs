//! Frozen complete records plus scripted port input/output and ordered traces.
#![allow(clippy::too_many_lines)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::*;
use asc_foundation_types::ResourceId;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_types::binding::{BindingView, PreparedBinding};
use asc_policy_types::target::{
    AdapterFault, TargetBindingPlan, TranslationOutcome, TranslationRejection,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[path = "../tests/support/concurrency.rs"]
mod concurrency;
#[path = "../tests/support/fixtures.rs"]
mod fixtures;
use crate::test_store as store;
use fixtures::expand;
use store::{TestAdmission, TestStore};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Suite {
    cases: Vec<FixtureCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureCase {
    case_id: String,
    variant: String,
    initial: Option<ReconcileRecord>,
    initial_schedule: Option<FixtureSchedule>,
    id: ResourceId,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureSchedule {
    attempts_started: u32,
    next_attempt_at: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Step {
    now: u64,
    calls: Vec<Call>,
    faults: Vec<String>,
    admission: Option<Admission>,
    expected: Option<ReconcileRecord>,
    result: Value,
    trace: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Call {
    operation: String,
    input: Value,
    output: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Admission {
    at: String,
    desired: Vec<BindingView>,
}

#[derive(Default)]
struct Script {
    calls: VecDeque<Call>,
    faults: VecDeque<String>,
    admission: Option<Admission>,
    trace: Vec<String>,
}

struct Harness {
    repository: ProcessLocalPapRepository,
    script: Mutex<Script>,
    now: AtomicU64,
    id: ResourceId,
}

impl Harness {
    fn point(&self, point: &str) -> Result<(), StoreError> {
        let mut script = self.script.lock().unwrap();
        if script.admission.as_ref().is_some_and(|a| a.at == point) {
            let admission = script.admission.take().unwrap();
            for desired in admission.desired {
                let before = self.repository.read(&self.id)?.unwrap();
                assert!(self.repository.compare_exchange_reconcile_intent(
                    &ExpectedBinding::from_binding(&before.binding),
                    &desired,
                )?);
                script.trace.push("admit".into());
            }
        }
        script.trace.push(point.into());
        if script.faults.front().is_some_and(|f| f == point) {
            script.faults.pop_front();
            script.trace.push(format!("{point}:error"));
            return Err(StoreError::Unavailable);
        }
        Ok(())
    }

    fn call(&self, operation: &str, input: Value) -> Value {
        self.point(operation).unwrap();
        let call = self
            .script
            .lock()
            .unwrap()
            .calls
            .pop_front()
            .expect("unexpected dependency call");
        assert_eq!(call.operation, operation);
        assert_eq!(call.input, input, "{operation} input differs");
        if matches!(operation, "create" | "update" | "delete") {
            let record = self.repository.read(&self.id).unwrap().unwrap();
            let targets: Vec<TargetRef> = if operation == "delete" {
                serde_json::from_value(input).unwrap()
            } else {
                let prepared: PreparedApply =
                    serde_json::from_value(input["prepared"].clone()).unwrap();
                let mut targets: Vec<TargetRef> =
                    serde_json::from_value(input["previous"].clone()).unwrap();
                targets.push(prepared.target);
                targets
            };
            for target in targets {
                assert!(
                    record
                        .deployments
                        .iter()
                        .any(|d| d.target == target && d.presence == Presence::Unknown),
                    "identity must be committed UNKNOWN before I/O"
                );
            }
        }
        self.script
            .lock()
            .unwrap()
            .trace
            .push(format!("{operation}:return"));
        call.output
    }
}

impl Clock for Harness {
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

impl TargetBindingAdapter for Harness {
    fn translate(&self, binding: &PreparedBinding) -> Result<TranslationOutcome, AdapterFault> {
        let value = self.call("adapter", json!(binding));
        match value["kind"].as_str().unwrap() {
            "translated" => Ok(TranslationOutcome::Translated(
                serde_json::from_value(value["plan"].clone()).unwrap(),
            )),
            "rejected" => Ok(TranslationOutcome::Rejected(TranslationRejection {
                code: value["code"].as_str().unwrap().into(),
            })),
            "fault" => Err(AdapterFault {
                code: value["code"].as_str().unwrap().into(),
            }),
            _ => panic!("unknown adapter response"),
        }
    }
}

impl TargetDeploymentClient for Harness {
    fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        serde_json::from_value(self.call("prepare", json!(plan))).unwrap()
    }
    fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        serde_json::from_value(self.call("create", json!({"previous": [], "prepared": prepared})))
            .unwrap()
    }
    fn update(&self, previous: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        serde_json::from_value(self.call(
            "update",
            json!({"previous": previous, "prepared": prepared}),
        ))
        .unwrap()
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        serde_json::from_value(self.call("delete", json!(targets))).unwrap()
    }
}

impl BindingStateRepository for Harness {
    fn get_binding_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError> {
        self.point("read")?;
        self.repository.get_binding_state(id)
    }
    fn compare_exchange_binding_state(
        &self,
        expected: &BindingStateSnapshot,
        write: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError> {
        let phase = store::write_phase(expected, write);
        self.point(phase)?;
        let result = self
            .repository
            .compare_exchange_binding_state(expected, write)?;
        self.script.lock().unwrap().trace.push(format!(
            "{phase}:{}",
            if result == WriteResult::Conflict {
                "conflict"
            } else {
                "matched"
            }
        ));
        Ok(result)
    }
}

#[test]
fn complete_serialized_core_cases() {
    let objects: BTreeMap<String, Value> = serde_json::from_str(include_str!(
        "../../../fixtures/reconciliation/objects.json"
    ))
    .unwrap();
    let raw: Value = serde_json::from_str(include_str!(
        "../../../fixtures/reconciliation/core-cases.json"
    ))
    .unwrap();
    let suite: Suite = serde_json::from_value(expand(raw, &objects)).unwrap();
    let required_list: Vec<String> = serde_json::from_str(include_str!(
        "../../../fixtures/reconciliation/required-variants.json"
    ))
    .unwrap();
    let required: BTreeSet<_> = required_list.iter().cloned().collect();
    assert_eq!(
        required.len(),
        required_list.len(),
        "duplicate manifest variant"
    );
    let categories: BTreeSet<_> = required
        .iter()
        .map(|key| key.split('/').next().unwrap().to_owned())
        .collect();
    assert_eq!(
        categories,
        (1..=22).map(|n| format!("REC-CORE-{n:03}")).collect()
    );
    let mut actual = BTreeSet::new();
    for case in suite.cases {
        let key = format!("{}/{}", case.case_id, case.variant);
        assert!(actual.insert(key.clone()), "duplicate fixture {key}");
        assert!(!case.steps.is_empty(), "empty fixture {key}");
        let mut schedule = AttemptSchedule::default();
        if let Some(initial) = &case.initial {
            schedule.observe(&initial.binding);
        }
        if let Some(progress) = case.initial_schedule {
            schedule.attempts_started = progress.attempts_started;
            schedule.next_attempt_at = progress.next_attempt_at;
        }
        let harness = Arc::new(Harness {
            repository: ProcessLocalPapRepository::with_binding_states(
                case.initial.into_iter().collect(),
            )
            .unwrap(),
            script: Mutex::new(Script::default()),
            now: AtomicU64::new(0),
            id: case.id.clone(),
        });
        let client: Arc<dyn TargetDeploymentClient> = harness.clone();
        let reconciler = BindingReconciler::new(
            harness.clone(),
            harness.clone(),
            BTreeMap::from([(
                "test".into(),
                Arc::new(move || Ok(client.clone()))
                    as Arc<dyn crate::TargetDeploymentClientFactory>,
            )]),
            "test".into(),
            harness.clone(),
            RetryPolicy {
                max_attempts: 3,
                base_delay_ms: 100,
                max_delay_ms: 150,
            },
        )
        .unwrap();
        for (index, step) in case.steps.into_iter().enumerate() {
            harness.now.store(step.now, Ordering::SeqCst);
            *harness.script.lock().unwrap() = Script {
                calls: step.calls.into(),
                faults: step.faults.into(),
                admission: step.admission,
                trace: vec![],
            };
            let result = reconciler.reconcile(&case.id, &mut schedule);
            let result = match result {
                Ok(disposition) => json!({"Ok": disposition}),
                Err(error) => json!({"Err": error.to_string()}),
            };
            assert_eq!(result, step.result, "{key} step {index} disposition");
            assert_eq!(
                harness.repository.read(&case.id).unwrap(),
                step.expected,
                "{key} step {index} full record"
            );
            let script = harness.script.lock().unwrap();
            assert!(
                script.calls.is_empty(),
                "{key}: unconsumed calls {:?}",
                script.calls
            );
            assert!(script.faults.is_empty(), "{key}: unconsumed faults");
            assert!(script.admission.is_none(), "{key}: unconsumed admission");
            assert_eq!(script.trace, step.trace, "{key} step {index} ordered trace");
            eprintln!("PASS {key} step {index}");
        }
    }
    for key in concurrency::run(&objects) {
        assert!(
            actual.insert(key.clone()),
            "duplicate threaded/serial variant {key}"
        );
    }
    assert_eq!(
        actual, required,
        "missing or unexpected acceptance variants"
    );
}

#[test]
fn actual_agentsight_adapter_uses_the_core_port_without_interface_changes() {
    use asc_policy_adapter_agentsight::{
        AGENTSIGHT_BINDING_PLAN_FORMAT, AgentSightAdapter, AgentSightBindingPlan,
    };

    let objects: BTreeMap<String, Value> = serde_json::from_str(include_str!(
        "../../../fixtures/reconciliation/objects.json"
    ))
    .unwrap();
    let raw = serde_json::from_str(include_str!(
        "../../../fixtures/reconciliation/core-cases.json"
    ))
    .unwrap();
    let mut suite: Suite = serde_json::from_value(expand(raw, &objects)).unwrap();
    let case = suite.cases.remove(0);
    let mut step = case.steps.into_iter().next().unwrap();
    let golden: AgentSightBindingPlan = serde_json::from_str(include_str!(
        "../../../fixtures/adapters/agentsight/prevent-file-deletion/agentsight-binding-plan.json"
    ))
    .unwrap();
    let plan = TargetBindingPlan {
        format: AGENTSIGHT_BINDING_PLAN_FORMAT.into(),
        content: serde_json::to_vec(&golden).unwrap(),
    };
    step.calls[0].output = json!({"kind": "translated", "plan": plan});
    step.calls[1].input = json!(plan);
    let harness = Arc::new(Harness {
        repository: ProcessLocalPapRepository::with_binding_states(
            case.initial.into_iter().collect(),
        )
        .unwrap(),
        script: Mutex::new(Script {
            calls: step.calls.into(),
            ..Script::default()
        }),
        now: AtomicU64::new(0),
        id: case.id.clone(),
    });
    let adapter_harness = harness.clone();
    let adapter = move |binding: &PreparedBinding| {
        let expected = TargetBindingAdapter::translate(adapter_harness.as_ref(), binding)?;
        let actual = AgentSightAdapter.translate(binding)?;
        assert_eq!(
            actual, expected,
            "actual Adapter output differs from frozen golden"
        );
        Ok(actual)
    };
    let reconciler = BindingReconciler::new(
        harness.clone(),
        Arc::new(adapter),
        BTreeMap::from([("test".into(), {
            let client = harness.clone();
            Arc::new(move || Ok(client.clone() as Arc<dyn TargetDeploymentClient>))
                as Arc<dyn crate::TargetDeploymentClientFactory>
        })]),
        "test".into(),
        harness.clone(),
        RetryPolicy {
            max_attempts: 3,
            base_delay_ms: 100,
            max_delay_ms: 150,
        },
    )
    .unwrap();
    assert_eq!(
        reconciler
            .reconcile(&case.id, &mut crate::AttemptSchedule::default())
            .unwrap(),
        Disposition::Completed
    );
    assert_eq!(harness.repository.read(&case.id).unwrap(), step.expected);
    let script = harness.script.lock().unwrap();
    assert!(script.calls.is_empty());
    assert_eq!(script.trace, step.trace);
}
