//! Composition slice: real PAP, runtime, core and Adapter with a scripted Client.
//! Full CLI/daemon process E2E is intentionally a separate suite.
use asc_daemon::start_policy_reconciliation_with_client;
use asc_pap::{PapRepository, PapService};
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_pcp::{
    DeploymentReport, Failure, Observation, PreparedApply, Presence, TargetDeploymentClient,
    TargetRef,
};
use asc_policy_repository::{BindingStateRepository, Deployment};
use asc_policy_types::binding::{BindingStatus, PreparedBinding};
use asc_policy_types::target::TargetBindingPlan;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Client {
    plans: Mutex<Vec<TargetBindingPlan>>,
    operations: Mutex<Vec<&'static str>>,
}
impl TargetDeploymentClient for Client {
    fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        self.plans.lock().unwrap().push(plan.clone());
        Ok(PreparedApply {
            target: TargetRef {
                route: "agentsight".into(),
                id: "test-target".into(),
                cleanup: vec![1],
            },
            format: "test".into(),
            content: plan.content.clone(),
        })
    }
    fn create(&self, p: &PreparedApply) -> DeploymentReport {
        self.operations.lock().unwrap().push("create");
        DeploymentReport {
            observations: vec![Observation {
                target: p.target.clone(),
                presence: Presence::Present,
            }],
            error: None,
        }
    }
    fn update(&self, _: &[TargetRef], _: &PreparedApply) -> DeploymentReport {
        panic!("unexpected update")
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        self.operations.lock().unwrap().push("delete");
        DeploymentReport {
            observations: targets
                .iter()
                .map(|t| Observation {
                    target: t.clone(),
                    presence: Presence::Absent,
                })
                .collect(),
            error: None,
        }
    }
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn configured_composition_delivers_pap_intent_and_joins_its_workers() {
    let repository = Arc::new(ProcessLocalPapRepository::default());
    let mut spec: PreparedBinding = serde_json::from_str(include_str!(
        "../../../crates/asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    spec.scope.revision = asc_foundation_types::Revision::new(1).unwrap();
    repository.put_policy(&spec.policy).unwrap();
    repository.put_scope(&spec.scope).unwrap();
    let client = Arc::new(Client::default());
    let runtime =
        start_policy_reconciliation_with_client(repository.clone(), client.clone()).unwrap();
    let pap = PapService::new(
        repository.clone(),
        Arc::new(asc_policy_engine::PolicyTemplateCompiler),
    )
    .with_reconcile_enqueuer(runtime.enqueuer());
    let accepted = pap
        .create_binding(
            &spec.policy.policy_id,
            spec.policy.revision,
            &spec.scope.scope_id,
            spec.scope.revision,
        )
        .unwrap();
    let id = &accepted.spec.binding_id;
    wait(|| pap.get_binding(id).unwrap().status == BindingStatus::Ready);
    let actual = repository.get_binding_state(id).unwrap().unwrap();
    assert_eq!(actual.binding.spec, accepted.spec);
    assert_eq!(actual.binding.status.error, None);
    assert_eq!(
        actual.deployments,
        vec![Deployment {
            target: TargetRef {
                route: "agentsight".into(),
                id: "test-target".into(),
                cleanup: vec![1]
            },
            revision: accepted.spec.binding_revision,
            presence: Presence::Present,
            last_confirmed: Some(Presence::Present)
        }]
    );
    let plan = client.plans.lock().unwrap()[0].clone();
    let mut golden: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/adapters/agentsight/prevent-file-deletion/agentsight-binding-plan.json"
    ))
    .unwrap();
    golden["source"]["bindingId"] = serde_json::json!(id);
    golden["source"]["bindingRevision"] = serde_json::json!(1);
    golden["source"]["scopeRevision"] = serde_json::json!(1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&plan.content).unwrap(),
        golden
    );
    pap.delete_binding(id).unwrap();
    wait(|| repository.get_binding_state(id).unwrap().is_none());
    runtime.shutdown().unwrap();
    assert_eq!(*client.operations.lock().unwrap(), vec!["create", "delete"]);
}

#[test]
fn unavailable_reconciliation_only_rejects_binding_writes() {
    let repository = Arc::new(ProcessLocalPapRepository::default());
    let spec: PreparedBinding = serde_json::from_str(include_str!(
        "../../../crates/asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    repository.put_policy(&spec.policy).unwrap();
    let pap = PapService::new(
        repository,
        Arc::new(asc_policy_engine::PolicyTemplateCompiler),
    )
    .with_reconcile_enqueuer(Arc::new(asc_daemon::UnavailableReconciliation));
    assert_eq!(
        pap.get_policy(&spec.policy.policy_id, spec.policy.revision)
            .unwrap(),
        spec.policy
    );
    let created = pap
        .create_policy("independent", &spec.policy.template)
        .unwrap();
    assert_eq!(created.revision.get(), 1);
    let policy = pap
        .update_policy(&created.policy_id, "renamed", &created.template)
        .unwrap();
    assert_eq!(policy.revision.get(), 2);
    assert_eq!(
        pap.get_policy(&policy.policy_id, policy.revision).unwrap(),
        policy
    );
    let created_scope = pap.create_scope(&spec.scope.selector).unwrap();
    let scope = pap
        .update_scope(
            &created_scope.scope_id,
            &asc_policy_types::scope::ScopeSelector::Pid { pid: 9876 },
        )
        .unwrap();
    assert_eq!(scope.revision.get(), 2);
    assert_eq!(
        pap.get_scope(&scope.scope_id, scope.revision).unwrap(),
        scope
    );
    assert_eq!(
        pap.create_binding(
            &policy.policy_id,
            policy.revision,
            &scope.scope_id,
            scope.revision
        ),
        Err(asc_pap::PapError::Unavailable)
    );
    assert_eq!(
        pap.update_binding(
            &spec.binding_id,
            &policy.policy_id,
            policy.revision,
            &scope.scope_id,
            scope.revision
        ),
        Err(asc_pap::PapError::Unavailable)
    );
    assert_eq!(
        pap.delete_binding(&spec.binding_id),
        Err(asc_pap::PapError::Unavailable)
    );
    assert_eq!(pap.list_bindings(10, 0).unwrap().total, 0);
    assert_eq!(
        pap.delete_policy_revision(&policy.policy_id, policy.revision)
            .unwrap(),
        policy
    );
    assert_eq!(
        pap.delete_scope_revision(&scope.scope_id, scope.revision)
            .unwrap(),
        scope
    );
    assert_eq!(pap.list_policies(10, 0).unwrap().total, 1);
    assert_eq!(pap.list_scopes(10, 0).unwrap().total, 0);
}
