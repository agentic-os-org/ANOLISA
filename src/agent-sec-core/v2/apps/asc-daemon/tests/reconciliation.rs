//! Composition slice: real PAP, runtime, core and Adapter with a scripted Client.
//! Full CLI/daemon process E2E is intentionally a separate suite.
#[cfg(test)]
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/policy.rs"
));

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
                id: serde_json::from_slice::<serde_json::Value>(&plan.content).unwrap()["source"]["bindingId"].as_str().unwrap().into(),
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
    let spec: PreparedBinding = serde_json::from_str(include_str!(
        "../../../crates/asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    repository.put_policy(&spec.policy).unwrap();
    let client = Arc::new(Client::default());
    let runtime =
        start_policy_reconciliation_with_client(repository.clone(), client.clone()).unwrap();
    let pap = PapService::new(repository.clone())
        .with_reconcile_enqueuer(runtime.enqueuer())
        .with_scope_discovery(Arc::new(Discovery));
    let scope = pap
        .create_scope_assignment(
            &spec.scope.selector,
            &[asc_policy_types::scope::PolicyReference {
                policy_id: spec.policy.policy_id.clone(),
                policy_revision: spec.policy.revision,
            }],
        )
        .unwrap();
    asc_pap::ScopeBindingSink::sync_instances(
        &pap,
        &scope.scope_id,
        std::slice::from_ref(&spec.scope.process),
    )
    .unwrap();
    let accepted = pap.list_bindings(10, 0).unwrap().items.remove(0);
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
                id: id.to_string(),
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
    golden["source"]["scopeId"] = serde_json::json!(scope.scope_id);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&plan.content).unwrap(),
        golden
    );
    pap.delete_scope(&scope.scope_id).unwrap();
    wait(|| repository.get_binding_state(id).unwrap().is_none());
    runtime.shutdown().unwrap();
    assert_eq!(*client.operations.lock().unwrap(), vec!["create", "delete"]);
}

#[test]
fn unavailable_reconciliation_rejects_new_assignments_but_keeps_policy_crud() {
    let repository = Arc::new(ProcessLocalPapRepository::default());
    let pap = PapService::new(repository)
        .with_reconcile_enqueuer(Arc::new(asc_daemon::UnavailableReconciliation))
        .with_scope_discovery(Arc::new(Discovery));
    let policy = pap
        .create_policy("test", &file_policy(vec!["/a".into()]))
        .unwrap();
    let refs = [asc_policy_types::scope::PolicyReference {
        policy_id: policy.policy_id.clone(),
        policy_revision: policy.revision,
    }];
    assert_eq!(
        pap.create_scope_assignment(
            &asc_policy_types::scope::ScopeSelector::Pid { pid: 42 },
            &refs
        ),
        Err(asc_pap::PapError::Unavailable)
    );
    assert_eq!(pap.list_scopes(100, 0).unwrap().total, 0);
    let policy = pap
        .update_policy(&policy.policy_id, "new name", &policy.template)
        .unwrap();
    pap.delete_policy_revision(&policy.policy_id, policy.revision)
        .unwrap();
}

struct Discovery;
impl asc_pap::ScopeDiscovery for Discovery {
    fn start(&self, _: &asc_pap::ScopeDiscoverySeed) -> Result<(), asc_pap::PapError> {
        Ok(())
    }
    fn stop(&self, _: &asc_foundation_types::ResourceId) -> Result<(), asc_pap::PapError> {
        Ok(())
    }
}

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn procfs_discovery_uses_saved_policy_and_scope_delete_cleans_all_instances() {
    procfs_assignment_lifecycle(Arc::new(ProcessLocalPapRepository::default()));
}

#[test]
fn sqlite_procfs_discovery_cleans_exited_instances_and_scope() {
    use std::os::unix::fs::PermissionsExt;
    let data = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let repository = asc_policy_repository_sqlite::SqlitePolicyRepository::open(
        &data.path().join("policy-state.db"),
    )
    .unwrap();
    procfs_assignment_lifecycle(Arc::new(repository));
}

fn procfs_assignment_lifecycle<R>(repository: Arc<R>)
where
    R: asc_pap::PapRepository
        + asc_policy_repository::BindingStateRepository
        + asc_policy_repository::BindingReconcileCatalog
        + 'static,
{
    use asc_policy_types::scope::{PolicyReference, ProcessMatcher, ScopeSelector};
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("assignment-test");
    std::fs::copy("/bin/sleep", &executable).unwrap();
    let spawn = || {
        Child(
            std::process::Command::new(&executable)
                .arg("60")
                .spawn()
                .unwrap(),
        )
    };
    let first = spawn();
    let client = Arc::new(Client::default());
    let runtime =
        start_policy_reconciliation_with_client(repository.clone(), client.clone()).unwrap();
    let pap = PapService::new(repository).with_reconcile_enqueuer(runtime.enqueuer());
    let discovery = Arc::new(asc_daemon::ScopeDiscoveryRegistry::new(Arc::new(
        pap.clone(),
    )));
    let pap = pap.with_scope_discovery(discovery.clone());
    let policy = pap
        .create_policy("original", &file_policy(vec!["/protected".into()]))
        .unwrap();
    let scope = pap
        .create_scope_assignment(
            &ScopeSelector::Process {
                matcher: ProcessMatcher::Executable {
                    executable: executable.to_str().unwrap().into(),
                },
            },
            &[PolicyReference {
                policy_id: policy.policy_id.clone(),
                policy_revision: policy.revision,
            }],
        )
        .unwrap();
    let ready = |count| {
        let bindings = pap.list_bindings(10, 0).unwrap();
        bindings.total == count
            && bindings
                .items
                .iter()
                .all(|b| b.status == BindingStatus::Ready)
    };
    wait(|| ready(1));
    let updated = pap
        .update_policy(&policy.policy_id, "replacement", &policy.template)
        .unwrap();
    pap.delete_policy_revision(&updated.policy_id, updated.revision)
        .unwrap();
    let second = spawn();
    wait(|| ready(2));
    let bindings = pap.list_bindings(10, 0).unwrap().items;
    for binding in &bindings {
        assert_eq!(binding.spec.policy, policy);
        assert_eq!(binding.spec.scope.scope_id, scope.scope_id);
        assert!([first.0.id(), second.0.id()].contains(&binding.spec.scope.process.pid));
    }
    assert_ne!(bindings[0].spec.binding_id, bindings[1].spec.binding_id);
    assert_ne!(
        bindings[0].spec.scope.process,
        bindings[1].spec.scope.process
    );
    drop(first);
    wait(|| ready(1));
    assert_eq!(pap.get_scope(&scope.scope_id).unwrap(), scope);
    let _third = spawn();
    wait(|| ready(2));
    pap.delete_scope(&scope.scope_id).unwrap();
    wait(|| pap.get_scope(&scope.scope_id) == Err(asc_pap::PapError::NotFound));
    assert_eq!(pap.list_bindings(10, 0).unwrap().total, 0);
    assert!(pap.delete_scope(&scope.scope_id).unwrap().completed);
    discovery.shutdown().unwrap();
    runtime.shutdown().unwrap();
    assert_eq!(
        *client.operations.lock().unwrap(),
        vec!["create", "create", "delete", "create", "delete", "delete"]
    );
}
