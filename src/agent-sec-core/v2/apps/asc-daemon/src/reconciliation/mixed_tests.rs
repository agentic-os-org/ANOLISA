//! Exercise production route composition with `SQLite` and bounded scripted PEP clients.

use super::*;
use asc_pap::PapRepository;
use asc_pcp::{DeploymentReport, Observation, PreparedApply, Presence, TargetRef};
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_types::binding::BindingStatus;
use asc_policy_types::policy::CredentialBinding;
use asc_policy_types::scope::{PreparedScope, ProcessMatcher, ScopeSelector, ScopeStatus};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../fixtures/reconciliation/mixed-file-network.json"
    ))
    .unwrap()
}

fn source() -> String {
    fixture()["binding"]["policy"]["template"]["content"]["text"]
        .as_str()
        .unwrap()
        .into()
}

#[derive(Default)]
struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Client {
    route: &'static str,
    binding: PreparedBinding,
    repository: Arc<SqlitePolicyRepository>,
    calls: Arc<Mutex<Vec<String>>>,
    fail_prepare: AtomicBool,
    fail_create: AtomicBool,
    fail_delete: AtomicBool,
}
impl Client {
    fn record(&self, operation: &str) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{}:{operation}", self.route));
    }
    fn report(targets: &[TargetRef], presence: Presence, fail: bool) -> DeploymentReport {
        if fail {
            return DeploymentReport {
                observations: vec![],
                error: Some(Failure::new(
                    FailureKind::Retryable,
                    "TEST_TARGET_UNAVAILABLE",
                )),
            };
        }
        DeploymentReport {
            observations: targets
                .iter()
                .map(|target| Observation {
                    target: target.clone(),
                    presence,
                })
                .collect(),
            error: None,
        }
    }
}
impl TargetDeploymentClient for Client {
    fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        self.record("prepare");
        if self.fail_prepare.swap(false, Ordering::SeqCst) {
            return Err(Failure::new(FailureKind::Retryable, "TEST_PREPARE_FAILED"));
        }
        if self.route == "gateway" {
            assert_eq!(plan.format, fixture()["expectedFormats"]["gateway"]);
            let plan: asc_gateway_plan::GatewayBindingPlan =
                serde_json::from_slice(&plan.content).unwrap();
            assert!(plan.source.matches(&self.binding));
            assert_eq!(
                serde_json::to_value(&plan.network).unwrap(),
                fixture()["expectedGatewayNetwork"]
            );
            assert!(!serde_json::to_string(&plan).unwrap().contains("/work/data"));
        } else {
            let plan: asc_policy_translator_agentsight::AgentSightBindingPlan =
                serde_json::from_slice(&plan.content).unwrap();
            assert_eq!(plan.source.binding_id, self.binding.binding_id);
            assert_eq!(plan.source.policy_revision, self.binding.policy.revision);
            let protection: serde_json::Value = serde_json::from_str(&plan.policy.content).unwrap();
            assert_eq!(protection, fixture()["expectedFileProtection"]);
        }
        Ok(PreparedApply {
            target: TargetRef {
                route: self.route.into(),
                id: self.binding.binding_id.to_string(),
                cleanup: vec![1],
            },
            format: plan.format.clone(),
            content: plan.content.clone(),
        })
    }
    fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        let state = self
            .repository
            .get_binding_state(&self.binding.binding_id)
            .unwrap()
            .unwrap();
        assert_eq!(state.binding.status.phase, BindingStatus::Applying);
        assert_eq!(state.deployments.len(), 2);
        assert!(
            state
                .deployments
                .iter()
                .all(|d| d.presence == Presence::Unknown)
        );
        if self.route == "gateway" {
            assert!(
                self.calls
                    .lock()
                    .unwrap()
                    .contains(&"agentsight:create".to_owned())
            );
        }
        self.record("create");
        Self::report(
            std::slice::from_ref(&prepared.target),
            Presence::Present,
            self.fail_create.swap(false, Ordering::SeqCst),
        )
    }
    fn update(&self, previous: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        assert!(
            previous.is_empty(),
            "retry must not delete a sibling backend"
        );
        self.create(prepared)
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        assert!(targets.iter().all(|target| target.route == self.route));
        self.record("delete");
        Self::report(
            targets,
            Presence::Absent,
            self.fail_delete.swap(false, Ordering::SeqCst),
        )
    }
}

fn setup(source: &str) -> (Arc<SqlitePolicyRepository>, PreparedBinding) {
    let repository = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let mut seed: PreparedBinding = serde_json::from_value(fixture()["binding"].clone()).unwrap();
    seed.policy.template = serde_json::from_value(serde_json::json!({
        "language": "cedar", "content": {
            "schema": {"schemaId": "seccore.system", "revision": 2}, "text": source
        }
    }))
    .unwrap();
    seed.policy.credential_bindings = if source.contains("@modify") {
        vec![CredentialBinding {
            rule_id: "github".into(),
            pair_id: "pair-fixture".into(),
        }]
    } else {
        vec![]
    };
    repository.put_policy(&seed.policy).unwrap();
    repository
        .put_scope(&PreparedScope {
            scope_id: seed.scope.scope_id.clone(),
            selector: ScopeSelector::Process {
                matcher: ProcessMatcher::Name {
                    process_name: "worker".into(),
                },
            },
            policy_snapshots: vec![seed.policy],
            owner_uid: Some(1000),
            status: ScopeStatus::Active,
        })
        .unwrap();
    repository
        .sync_scope_instances(&seed.scope.scope_id, &[seed.scope.process])
        .unwrap();
    let bindings = repository.list_bindings(10, 0).unwrap();
    assert_eq!(bindings.total, 1, "one mixed Policy creates one Binding");
    (repository, bindings.items[0].spec.clone())
}

fn client(
    route: &'static str,
    repository: &Arc<SqlitePolicyRepository>,
    binding: &PreparedBinding,
    calls: &Arc<Mutex<Vec<String>>>,
) -> Arc<Client> {
    Arc::new(Client {
        route,
        binding: binding.clone(),
        repository: repository.clone(),
        calls: calls.clone(),
        fail_prepare: AtomicBool::new(false),
        fail_create: AtomicBool::new(false),
        fail_delete: AtomicBool::new(false),
    })
}

fn factory(client: Arc<Client>) -> Arc<dyn TargetDeploymentClientFactory> {
    Arc::new(move || Ok(client.clone() as Arc<dyn TargetDeploymentClient>))
}

#[test]
fn network_without_credentials_is_routed_and_rejected_before_target_mutation() {
    let (repository, binding) = setup(
        r#"permit(principal, action == SecCore::Action::"network.use", resource == SecCore::Network::"all");"#,
    );
    assert!(!binding.policy.uses_credentials());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let core = build_reconciler(
        repository.clone(),
        factory(client("agentsight", &repository, &binding, &calls)),
        Some(factory(client("gateway", &repository, &binding, &calls))),
        Arc::new(TestClock::default()),
    )
    .unwrap();
    core.reconcile(&binding.binding_id, &mut AttemptSchedule::default())
        .unwrap();
    let state = repository
        .get_binding_state(&binding.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(state.binding.status.phase, BindingStatus::ApplyFailed);
    assert_eq!(
        state.binding.status.error.as_ref().unwrap().code,
        "GATEWAY_UNSUPPORTED_NETWORK_POLICY"
    );
    assert!(state.deployments.is_empty());
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn mixed_binding_requires_both_backends_and_retains_partial_cleanup_across_retry() {
    let (repository, binding) = setup(&source());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let file = client("agentsight", &repository, &binding, &calls);
    let gateway = client("gateway", &repository, &binding, &calls);
    gateway.fail_create.store(true, Ordering::SeqCst);
    let clock = Arc::new(TestClock::default());
    let planning_calls = Arc::new(AtomicU64::default());
    let count = planning_calls.clone();
    let core = build_reconciler(
        repository.clone(),
        factory(file.clone()),
        Some(factory(gateway)),
        clock.clone(),
    )
    .unwrap()
    .with_plan_builder(move |binding| {
        count.fetch_add(1, Ordering::SeqCst);
        target_plans(binding)
    });
    let mut schedule = AttemptSchedule::default();
    assert!(matches!(
        core.reconcile(&binding.binding_id, &mut schedule).unwrap(),
        Disposition::RetryAt { .. }
    ));
    let state = repository
        .get_binding_state(&binding.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(state.binding.status.phase, BindingStatus::PendingApply);
    assert_eq!(state.deployments[0].presence, Presence::Present);
    assert_eq!(state.deployments[1].presence, Presence::Unknown);
    assert_eq!(
        planning_calls.load(Ordering::SeqCst),
        1,
        "one plan covers both routes"
    );
    clock.0.store(100_000, Ordering::SeqCst);
    assert_eq!(
        core.reconcile(&binding.binding_id, &mut schedule).unwrap(),
        Disposition::Completed
    );
    let ready = repository
        .get_binding_state(&binding.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(ready.binding.status.phase, BindingStatus::Ready);
    assert_eq!(ready.binding.spec, binding);
    assert_eq!(
        planning_calls.load(Ordering::SeqCst),
        2,
        "retry replans once"
    );
    assert!(
        ready
            .deployments
            .iter()
            .all(|d| d.presence == Presence::Present)
    );
    // Scope retirement owns both backend cleanup responsibilities, including failed deletes.
    repository
        .sync_scope_instances(&binding.scope.scope_id, &[])
        .unwrap();
    file.fail_delete.store(true, Ordering::SeqCst);
    assert!(matches!(
        core.reconcile(&binding.binding_id, &mut schedule).unwrap(),
        Disposition::RetryAt { .. }
    ));
    let deleting = repository
        .get_binding_state(&binding.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(deleting.binding.status.phase, BindingStatus::PendingDelete);
    assert_eq!(deleting.deployments.len(), 1);
    assert_eq!(deleting.deployments[0].target.route, "agentsight");
    assert!(calls.lock().unwrap().contains(&"gateway:delete".to_owned()));
    clock.0.store(200_000, Ordering::SeqCst);
    assert_eq!(
        core.reconcile(&binding.binding_id, &mut schedule).unwrap(),
        Disposition::Completed
    );
    assert!(
        repository
            .get_binding_state(&binding.binding_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        planning_calls.load(Ordering::SeqCst),
        2,
        "cleanup uses persisted targets"
    );
}

#[test]
fn failed_file_protection_does_not_enable_gateway_routing() {
    let (repository, binding) = setup(&source());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let file = client("agentsight", &repository, &binding, &calls);
    let gateway = client("gateway", &repository, &binding, &calls);
    file.fail_create.store(true, Ordering::SeqCst);
    let core = build_reconciler(
        repository.clone(),
        factory(file),
        Some(factory(gateway)),
        Arc::new(TestClock::default()),
    )
    .unwrap();
    assert!(matches!(
        core.reconcile(&binding.binding_id, &mut AttemptSchedule::default())
            .unwrap(),
        Disposition::RetryAt { .. }
    ));
    let state = repository
        .get_binding_state(&binding.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(state.binding.status.phase, BindingStatus::PendingApply);
    assert_eq!(state.deployments.len(), 2);
    assert!(
        state
            .deployments
            .iter()
            .all(|d| d.presence == Presence::Unknown)
    );
    assert!(!calls.lock().unwrap().contains(&"gateway:create".to_owned()));
}

#[test]
fn mixed_binding_preflights_both_targets_before_modifying_either() {
    for unsupported_file in [false, true] {
        let source = if unsupported_file {
            source().replace(
                "resource.path == \"/work/data\"",
                "resource.path == \"/work/data\" && context.input.program == \"/bin/rm\"",
            )
        } else {
            source()
        };
        let (repository, binding) = setup(&source);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let file = client("agentsight", &repository, &binding, &calls);
        let gateway = client("gateway", &repository, &binding, &calls);
        gateway.fail_prepare.store(true, Ordering::SeqCst);
        let core = build_reconciler(
            repository.clone(),
            factory(file),
            Some(factory(gateway)),
            Arc::new(TestClock::default()),
        )
        .unwrap();
        core.reconcile(&binding.binding_id, &mut AttemptSchedule::default())
            .unwrap();
        let state = repository
            .get_binding_state(&binding.binding_id)
            .unwrap()
            .unwrap();
        assert_ne!(state.binding.status.phase, BindingStatus::Ready);
        if unsupported_file {
            assert_eq!(
                state.binding.status.error.as_ref().unwrap().code,
                "CEDAR_RULE_1_UNSUPPORTED_CONDITION"
            );
        }
        assert!(state.deployments.is_empty());
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|call| call.ends_with(":prepare"))
        );
    }
}

#[test]
fn automatic_file_and_gateway_plans_use_only_their_selected_source_statements() {
    use asc_policy_types::policy::{DeploymentBackend, DeploymentSelection};
    let mut binding: PreparedBinding =
        serde_json::from_value(fixture()["binding"].clone()).unwrap();
    let asc_policy_types::authoring::PolicySource::Cedar(cedar) = &mut binding.policy.template
    else {
        panic!("fixture must contain Cedar");
    };
    cedar.schema = asc_policy_cedar::schema::principal_reference();
    cedar.text = cedar.text.replace(
        "when {",
        r#"when { principal has agentType && principal.agentType == "qoder" &&"#,
    );
    cedar.text.push_str(
        r#"
        forbid(principal, action, resource is SecCore::HttpEndpoint)
        when { principal has agentType && principal.agentType == "qoder" &&
               resource.host == "private.example" };"#,
    );
    let selection = |backend, ids: &[&str]| {
        Some(DeploymentSelection {
            backend,
            statement_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
        })
    };
    binding.policy.deployment = selection(DeploymentBackend::ActPlane, &["policy1"]);
    assert!(!binding.policy.uses_credentials());
    let plans = target_plans(&binding).unwrap();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].0, "agentsight");
    let file: asc_policy_translator_agentsight::AgentSightBindingPlan =
        serde_json::from_slice(&plans[0].1.content).unwrap();
    let protection: serde_json::Value = serde_json::from_str(&file.policy.content).unwrap();
    assert_eq!(protection, fixture()["expectedFileProtection"]);

    binding.policy.deployment = selection(DeploymentBackend::Gateway, &["policy0"]);
    let plans = target_plans(&binding).unwrap();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].0, "gateway");
    let network: asc_gateway_plan::GatewayBindingPlan =
        serde_json::from_slice(&plans[0].1.content).unwrap();
    assert_eq!(
        serde_json::to_value(network.network).unwrap(),
        fixture()["expectedGatewayNetwork"]
    );

    binding.policy.deployment = selection(DeploymentBackend::Gateway, &["policy0", "policy2"]);
    assert!(target_plans(&binding).is_err());
}
