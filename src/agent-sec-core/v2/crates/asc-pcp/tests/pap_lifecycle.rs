//! PAP + memory repository + reconciler integration; the target Client is scripted.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use asc_foundation_types::Revision;
use asc_pap::{PapError, PapRepository, PapService};
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_pcp::*;
use asc_policy_types::binding::{BindingStatus, BindingView, PreparedBinding};
use asc_policy_types::target::{AdapterFault, TargetBindingPlan, TranslationOutcome};

#[derive(Default)]
struct Script {
    prepare_count: usize,
    requests: Vec<PreparedApply>,
    deletions: Vec<Vec<TargetRef>>,
    reject_apply: bool,
    reject_delete: bool,
}
#[derive(Default)]
struct Client(Mutex<Script>);
impl Clock for Client {
    fn now_ms(&self) -> u64 {
        0
    }
}
impl TargetDeploymentClient for Client {
    fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        self.0.lock().unwrap().prepare_count += 1;
        let spec: PreparedBinding = serde_json::from_slice(&plan.content).unwrap();
        Ok(PreparedApply {
            target: TargetRef {
                route: "test".into(),
                id: format!("{}:{}", spec.binding_id, spec.binding_revision.get()),
                cleanup: vec![1],
            },
            format: "request.v1".into(),
            content: plan.content.clone(),
        })
    }
    fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        let mut script = self.0.lock().unwrap();
        script.requests.push(prepared.clone());
        DeploymentReport {
            observations: vec![Observation {
                target: prepared.target.clone(),
                presence: if script.reject_apply {
                    Presence::Unknown
                } else {
                    Presence::Present
                },
            }],
            error: script
                .reject_apply
                .then(|| Failure::new(FailureKind::Rejected, "TEST_APPLY_REJECTED")),
        }
    }
    fn update(&self, previous: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        let mut report = self.create(prepared);
        report
            .observations
            .extend(previous.iter().map(|target| Observation {
                target: target.clone(),
                presence: Presence::Absent,
            }));
        report
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        let mut script = self.0.lock().unwrap();
        script.deletions.push(targets.to_vec());
        DeploymentReport {
            observations: targets
                .iter()
                .map(|target| Observation {
                    target: target.clone(),
                    presence: if script.reject_delete {
                        Presence::Unknown
                    } else {
                        Presence::Absent
                    },
                })
                .collect(),
            error: script
                .reject_delete
                .then(|| Failure::new(FailureKind::Rejected, "TEST_DELETE_REJECTED")),
        }
    }
}
struct Rig {
    repo: Arc<ProcessLocalPapRepository>,
    pap: PapService<ProcessLocalPapRepository>,
    client: Arc<Client>,
    core: BindingReconciler,
    binding: BindingView,
}
impl Rig {
    fn new() -> Self {
        Self::with_revision(1)
    }
    fn with_revision(revision: u32) -> Self {
        let mut spec: PreparedBinding = serde_json::from_str(include_str!(
            "../../asc-policy-types/tests/fixtures/prepared-binding.json"
        ))
        .unwrap();
        spec.binding_revision = Revision::new(revision).unwrap();
        let binding = BindingView {
            spec: spec.clone(),
            status: BindingStatus::PendingApply.into(),
        };
        let repo = Arc::new(
            ProcessLocalPapRepository::with_binding_states(vec![ReconcileRecord {
                status_version: 1,
                binding: binding.clone(),
                deployments: vec![],
            }])
            .unwrap(),
        );
        repo.put_policy(&spec.policy).unwrap();
        let pap = PapService::new(repo.clone()).with_scope_discovery(Arc::new(Discovery));
        let client = Arc::new(Client::default());
        let adapter = |binding: &PreparedBinding| -> Result<TranslationOutcome, AdapterFault> {
            Ok(TranslationOutcome::Translated(TargetBindingPlan {
                format: "test.v1".into(),
                content: serde_json::to_vec(binding).unwrap(),
            }))
        };
        let core = BindingReconciler::new(
            repo.clone(),
            Arc::new(adapter),
            BTreeMap::from([("test".into(), {
                let client = client.clone();
                Arc::new(move || Ok(client.clone() as Arc<dyn TargetDeploymentClient>))
                    as Arc<dyn asc_pcp::TargetDeploymentClientFactory>
            })]),
            "test".into(),
            client.clone(),
            RetryPolicy {
                max_attempts: 2,
                base_delay_ms: 100,
                max_delay_ms: 200,
            },
        )
        .unwrap();
        Self {
            repo,
            pap,
            client,
            core,
            binding,
        }
    }
    fn state(&self) -> ReconcileRecord {
        self.repo
            .get_binding_state(&self.binding.spec.binding_id)
            .unwrap()
            .unwrap()
    }
    fn delete(&self) -> Result<BindingView, PapError> {
        self.pap.delete_scope(&self.binding.spec.scope.scope_id)?;
        self.pap.get_binding(&self.binding.spec.binding_id)
    }
    fn apply(&self) -> Result<BindingView, PapError> {
        let spec = &self.binding.spec;
        if self.pap.get_scope(&spec.scope.scope_id)?.status
            == asc_policy_types::scope::ScopeStatus::Deleting
        {
            return Err(PapError::OperationInProgress);
        }
        self.pap.retry_scope(&spec.scope.scope_id)?;
        self.pap.get_binding(&spec.binding_id)
    }
}

#[test]
fn same_spec_failed_apply_prepares_again_and_resets_only_retry_controls() {
    let rig = Rig::new();
    let id = &rig.binding.spec.binding_id;
    rig.client.0.lock().unwrap().reject_apply = true;
    assert!(matches!(
        rig.core
            .reconcile(id, &mut asc_pcp::AttemptSchedule::default())
            .unwrap(),
        Disposition::Failed { .. }
    ));
    let failed = rig.state();
    assert!(failed.binding.status.error.is_some());
    assert_eq!(rig.apply().unwrap(), rig.binding);
    let pending = rig.state();
    assert_eq!(pending.deployments, failed.deployments);
    assert_eq!(pending.binding.status.error, None);
    rig.client.0.lock().unwrap().reject_apply = false;
    assert_eq!(
        rig.core
            .reconcile(id, &mut asc_pcp::AttemptSchedule::default())
            .unwrap(),
        Disposition::Completed
    );
    let script = rig.client.0.lock().unwrap();
    assert_eq!(script.prepare_count, 2);
    assert_eq!(script.requests.len(), 2);
    assert_eq!(script.requests[0], script.requests[1]);
    assert_eq!(rig.state().binding.spec, rig.binding.spec);
}

#[test]
fn deletion_retry_preserves_targets_then_removes_record_and_new_create_uses_fresh_id() {
    let rig = Rig::new();
    let spec = &rig.binding.spec;
    let id = &spec.binding_id;
    assert_eq!(
        rig.core
            .reconcile(id, &mut asc_pcp::AttemptSchedule::default())
            .unwrap(),
        Disposition::Completed
    );
    let ready = rig.state();
    let pending = rig.delete().unwrap();
    assert_eq!(pending.spec, ready.binding.spec);
    assert_eq!(rig.state().deployments, ready.deployments);
    assert_eq!(rig.state().binding.status.error, None);
    assert_eq!(rig.apply(), Err(PapError::OperationInProgress));
    rig.client.0.lock().unwrap().reject_delete = true;
    assert!(matches!(
        rig.core
            .reconcile(id, &mut asc_pcp::AttemptSchedule::default())
            .unwrap(),
        Disposition::Failed { .. }
    ));
    let failed = rig.state();
    assert_eq!(failed.binding.status.phase, BindingStatus::DeleteFailed);
    assert_eq!(rig.apply(), Err(PapError::OperationInProgress));
    assert_eq!(rig.delete().unwrap(), failed.binding);
    rig.pap.retry_scope(&spec.scope.scope_id).unwrap();
    assert_eq!(rig.state().deployments, failed.deployments);
    assert_eq!(rig.state().binding.status.error, None);
    let retry = rig.state();
    assert_eq!(rig.delete().unwrap(), pending);
    assert_eq!(
        rig.state(),
        retry,
        "duplicate DELETE preserves the retry budget"
    );
    rig.client.0.lock().unwrap().reject_delete = false;
    assert_eq!(
        rig.core
            .reconcile(id, &mut asc_pcp::AttemptSchedule::default())
            .unwrap(),
        Disposition::Completed
    );
    assert_eq!(rig.repo.get_binding_state(id).unwrap(), None);
    assert_eq!(rig.pap.get_binding(id), Err(PapError::NotFound));
    assert_eq!(rig.apply(), Err(PapError::NotFound));
    assert_eq!(rig.delete(), Err(PapError::NotFound));
    assert!(rig.pap.list_bindings(100, 0).unwrap().items.is_empty());
    assert_eq!(
        rig.core
            .reconcile(id, &mut asc_pcp::AttemptSchedule::default())
            .unwrap(),
        Disposition::Skipped
    );
    let scope = rig
        .pap
        .create_scope_assignment(
            &spec.scope.selector,
            &[asc_policy_types::scope::PolicyReference {
                policy_id: spec.policy.policy_id.clone(),
                policy_revision: spec.policy.revision,
            }],
        )
        .unwrap();
    asc_pap::ScopeBindingSink::sync_instances(
        &rig.pap,
        &scope.scope_id,
        std::slice::from_ref(&spec.scope.process),
    )
    .unwrap();
    let fresh = rig.pap.list_bindings(100, 0).unwrap().items.remove(0);
    assert_ne!(fresh.spec.binding_id, *id);
    assert_eq!(fresh.spec.binding_revision.get(), 1);
    assert_eq!(
        rig.core
            .reconcile(
                &fresh.spec.binding_id,
                &mut asc_pcp::AttemptSchedule::default()
            )
            .unwrap(),
        Disposition::Completed
    );
    let script = rig.client.0.lock().unwrap();
    assert_eq!(script.deletions.len(), 2);
    assert_eq!(script.deletions[0], script.deletions[1]);
    assert_ne!(script.requests[0].target.id, script.requests[1].target.id);
}

#[test]
fn spec_change_clears_prepared_but_keeps_previous_target_for_cleanup() {
    let rig = Rig::new();
    let id = &rig.binding.spec.binding_id;
    rig.core
        .reconcile(id, &mut asc_pcp::AttemptSchedule::default())
        .unwrap();
    let ready = rig.state();
    // Internal revision compatibility: persisted older revisions may still require cleanup.
    let mut next = ready.binding.clone();
    next.spec.binding_revision = Revision::new(2).unwrap();
    next.spec.scope.process.start_time += 1;
    next.status = BindingStatus::PendingApply.into();
    rig.repo
        .update_binding(Some(&(&ready).into()), &next)
        .unwrap();
    assert_eq!(next.spec.binding_revision.get(), 2);
    assert_eq!(rig.state().deployments, ready.deployments);
    assert_eq!(rig.state().binding.status.error, None);
    assert_eq!(
        rig.core
            .reconcile(id, &mut asc_pcp::AttemptSchedule::default())
            .unwrap(),
        Disposition::Completed
    );
    let current = rig.state();
    assert_eq!(current.deployments.len(), 1);
    assert_ne!(
        current.deployments[0].target.id,
        ready.deployments[0].target.id
    );
}

#[test]
fn maximum_revision_allows_same_spec_retry_and_delete_but_rejects_spec_change() {
    let rig = Rig::with_revision(u32::MAX);
    let id = &rig.binding.spec.binding_id;
    rig.client.0.lock().unwrap().reject_apply = true;
    assert!(matches!(
        rig.core
            .reconcile(id, &mut asc_pcp::AttemptSchedule::default())
            .unwrap(),
        Disposition::Failed { .. }
    ));
    assert_eq!(rig.apply().unwrap(), rig.binding);
    let before = rig.state();
    let mut changed = before.binding.clone();
    changed.spec.scope.process.start_time += 1;
    assert_eq!(
        rig.repo.update_binding(Some(&(&before).into()), &changed),
        Err(PapError::Conflict)
    );
    assert_eq!(rig.delete().unwrap().spec, rig.binding.spec);
    assert_eq!(
        rig.core
            .reconcile(id, &mut asc_pcp::AttemptSchedule::default())
            .unwrap(),
        Disposition::Completed
    );
    assert_eq!(rig.pap.get_binding(id), Err(PapError::NotFound));
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
