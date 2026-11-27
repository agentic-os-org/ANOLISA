//! Target-specific composition; queue and lifecycle remain in Policy Runtime.
use asc_agentsight_client::AgentSightClientFactory;
#[cfg(all(target_os = "linux", feature = "lua-lsm-testing"))]
use asc_lua_lsm_client::LuaLsmClient;
use asc_lua_lsm_protocol::LUA_LSM_ROUTE;
use asc_pcp::{
    AttemptSchedule, BindingReconciler, Clock, Disposition, RetryPolicy, StoreError,
    TargetDeploymentClient, TargetDeploymentClientFactory,
};
use asc_policy_repository::{BindingReconcileCatalog, BindingStateRepository};
use asc_policy_runtime::reconciliation::{
    MonotonicClock, ReconcileAttempt, ReconciliationRuntime, RuntimeConfig,
};
use asc_policy_translator_agentsight::AgentSightTranslator;
use asc_policy_translator_lua_lsm::LuaLsmTranslator;
use asc_policy_types::binding::PreparedBinding;
use asc_policy_types::target::{
    AdapterFault, Failure, FailureKind, TargetBindingPlan, TranslationOutcome,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, TryLockError};

/// File-policy backend selected for new Bindings; saved deployments retain their route.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FilePolicyBackend {
    /// Existing `AgentSight` HTTP deployment.
    #[default]
    AgentSight,
    /// Direct Lua-LSM securityfs deployment.
    LuaLsm,
}

impl FilePolicyBackend {
    /// Reads the explicit daemon selection, preserving `AgentSight` when unset.
    /// # Errors
    /// Rejects unknown, unavailable or non-Unicode selections.
    pub fn from_env() -> Result<Self, StoreError> {
        match std::env::var("AGENT_SEC_FILE_BACKEND") {
            Ok(value) => Self::parse(Some(&value)),
            Err(std::env::VarError::NotPresent) => Self::parse(None),
            _ => Err(StoreError::Invalid),
        }
    }
    fn parse(value: Option<&str>) -> Result<Self, StoreError> {
        match value {
            None | Some("agentsight") => Ok(Self::AgentSight),
            Some("lua-lsm") => Self::LuaLsm.validate_selection(),
            _ => Err(StoreError::Invalid),
        }
    }
    fn validate_selection(self) -> Result<Self, StoreError> {
        if self == Self::LuaLsm && !cfg!(all(target_os = "linux", feature = "lua-lsm-testing")) {
            return Err(StoreError::Invalid);
        }
        Ok(self)
    }
    /// Automatic PAP admission must use the compiler that will build new file Bindings.
    #[must_use]
    pub fn deployment_selectors(
        self,
    ) -> &'static [asc_policy_cedar::deployment::DeploymentSelector] {
        use asc_gateway_plan::selection::SELECTOR as GATEWAY;
        match self {
            Self::AgentSight => &[
                asc_policy_translator_agentsight::selection::SELECTOR,
                GATEWAY,
            ],
            Self::LuaLsm => &[asc_policy_translator_lua_lsm::SELECTOR, GATEWAY],
        }
    }
    fn route(self) -> &'static str {
        match self {
            Self::AgentSight => "agentsight",
            Self::LuaLsm => LUA_LSM_ROUTE,
        }
    }
}

struct GatewayAdmission<R> {
    repository: Arc<R>,
    core: Arc<BindingReconciler>,
    gateway: Mutex<()>,
}

impl<R: BindingStateRepository> ReconcileAttempt for GatewayAdmission<R> {
    fn clock(&self) -> Arc<dyn Clock> {
        self.core.clock()
    }

    fn reconcile(
        &self,
        id: &asc_policy_types::identifiers::ResourceId,
        schedule: &mut AttemptSchedule,
    ) -> Result<Disposition, StoreError> {
        let credential = self
            .repository
            .get_binding_state(id)?
            .is_some_and(|record| {
                record.binding.spec.policy.uses_credentials()
                    || record
                        .deployments
                        .iter()
                        .any(|deployment| deployment.target.route == "gateway")
            });
        if !credential {
            return self.core.reconcile(id, schedule);
        }
        // Only one worker can wait for gateway control operations; other routes stay runnable.
        let _admission = match self.gateway.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Ok(Disposition::DeferredAt {
                    at: self.clock().now_ms().saturating_add(1000),
                });
            }
        };
        self.core.reconcile(id, schedule)
    }
}

/// Composes file and credential execution routes without coupling their readiness.
/// # Errors
/// Returns repository readiness or worker startup failures.
pub fn start_policy_reconciliation_with_gateway<
    R: BindingStateRepository + BindingReconcileCatalog + 'static,
>(
    repository: Arc<R>,
    gateway: Option<Arc<asc_gateway_runtime::GatewayService>>,
) -> Result<ReconciliationRuntime, StoreError> {
    let factory: Arc<dyn TargetDeploymentClientFactory> = Arc::new(move || {
        let service = gateway
            .as_ref()
            .ok_or_else(|| Failure::new(FailureKind::Retryable, "GATEWAY_UNAVAILABLE"))?;
        Ok(Arc::new(asc_gateway_runtime::GatewayBindingClient::new(
            service.clone(),
        )) as Arc<dyn TargetDeploymentClient>)
    });
    start_with_factories(
        repository,
        Arc::new(AgentSightClientFactory::default()),
        Some(factory),
        FilePolicyBackend::AgentSight,
    )
}

/// Composes the selected file backend while preserving routes of saved Bindings.
/// # Errors
/// Returns storage, recovery, configuration or worker startup failures.
pub fn start_policy_reconciliation_with_backend<
    R: BindingStateRepository + BindingReconcileCatalog + 'static,
>(
    repository: Arc<R>,
    gateway: Option<Arc<asc_gateway_runtime::GatewayService>>,
    backend: FilePolicyBackend,
) -> Result<ReconciliationRuntime, StoreError> {
    let factory: Arc<dyn TargetDeploymentClientFactory> = Arc::new(move || {
        let service = gateway
            .as_ref()
            .ok_or_else(|| Failure::new(FailureKind::Retryable, "GATEWAY_UNAVAILABLE"))?;
        Ok(Arc::new(asc_gateway_runtime::GatewayBindingClient::new(
            service.clone(),
        )) as Arc<dyn TargetDeploymentClient>)
    });
    start_with_factories(
        repository,
        Arc::new(AgentSightClientFactory::default()),
        Some(factory),
        backend,
    )
}

/// Uses an injected file target Client for component validation.
/// # Errors
/// Returns repository readiness or worker startup failures.
pub fn start_policy_reconciliation_with_client<
    R: BindingStateRepository + BindingReconcileCatalog + 'static,
>(
    repository: Arc<R>,
    client: Arc<dyn TargetDeploymentClient>,
) -> Result<ReconciliationRuntime, StoreError> {
    start_with_factories(
        repository,
        Arc::new(move || Ok(client.clone())),
        None,
        FilePolicyBackend::AgentSight,
    )
}

fn start_with_factories<R: BindingStateRepository + BindingReconcileCatalog + 'static>(
    repository: Arc<R>,
    file_factory: Arc<dyn TargetDeploymentClientFactory>,
    gateway_factory: Option<Arc<dyn TargetDeploymentClientFactory>>,
    backend: FilePolicyBackend,
) -> Result<ReconciliationRuntime, StoreError> {
    let core = Arc::new(build_reconciler(
        repository.clone(),
        file_factory,
        gateway_factory,
        Arc::new(MonotonicClock::default()),
        backend,
    )?);
    let attempt = Arc::new(GatewayAdmission {
        repository: repository.clone(),
        core,
        gateway: Mutex::new(()),
    });
    ReconciliationRuntime::start(repository, attempt, RuntimeConfig::default())
}

fn build_reconciler<R: BindingStateRepository + BindingReconcileCatalog + 'static>(
    repository: Arc<R>,
    file_factory: Arc<dyn TargetDeploymentClientFactory>,
    gateway_factory: Option<Arc<dyn TargetDeploymentClientFactory>>,
    clock: Arc<dyn Clock>,
    backend: FilePolicyBackend,
) -> Result<BindingReconciler, StoreError> {
    backend.validate_selection()?;
    let routes = recover_file_bindings(repository.as_ref())?;
    let mut factories = BTreeMap::from([("agentsight".into(), file_factory)]);
    #[cfg(all(target_os = "linux", feature = "lua-lsm-testing"))]
    {
        let lua: Arc<dyn TargetDeploymentClient> = Arc::new(LuaLsmClient::default());
        factories.insert(LUA_LSM_ROUTE.into(), Arc::new(move || Ok(lua.clone())));
    }
    if let Some(factory) = gateway_factory {
        factories.insert("gateway".into(), factory);
    }
    Ok(BindingReconciler::new(
        repository.clone(),
        Arc::new(|binding: &PreparedBinding| AgentSightTranslator.translate(binding)),
        factories,
        "agentsight".into(),
        clock,
        RetryPolicy {
            max_attempts: 5,
            base_delay_ms: 1000,
            max_delay_ms: 30_000,
        },
    )?
    .with_plan_builder(move |binding| {
        let backend = routes.get(&binding.binding_id).map_or(backend, |route| {
            if route == LUA_LSM_ROUTE {
                FilePolicyBackend::LuaLsm
            } else {
                FilePolicyBackend::AgentSight
            }
        });
        target_plans(binding, backend)
            .map_err(|failure| manual_source_failure(repository.as_ref(), binding, failure))
    }))
}

fn manual_source_failure(
    repository: &impl BindingStateRepository,
    binding: &PreparedBinding,
    failure: Failure,
) -> Failure {
    use asc_policy_types::authoring::PolicySource;
    let (None, PolicySource::Cedar(pinned)) =
        (&binding.policy.deployment, &binding.policy.template)
    else {
        return failure;
    };
    let Some((index, reason)) = failure
        .code
        .strip_prefix("CEDAR_RULE_")
        .and_then(|code| code.split_once('_'))
        .and_then(|(index, reason)| Some((index.parse::<usize>().ok()?, reason)))
    else {
        return failure;
    };
    let policy = &binding.policy;
    match repository.archived_cedar_source(&policy.policy_id, policy.revision) {
        Ok(Some(source)) => match asc_pap::manual_source_index(&source, pinned, index) {
            Some(original) => {
                Failure::new(failure.kind, &format!("CEDAR_RULE_{original}_{reason}"))
            }
            None => failure,
        },
        Ok(None) => failure,
        Err(_) => Failure::new(FailureKind::Retryable, "POLICY_SOURCE_HISTORY_UNAVAILABLE"),
    }
}

fn translated_plan(
    outcome: Result<TranslationOutcome, AdapterFault>,
) -> Result<TargetBindingPlan, Failure> {
    match outcome {
        Ok(TranslationOutcome::Translated(plan)) => Ok(plan),
        Ok(TranslationOutcome::Rejected(rejection)) => {
            Err(Failure::new(FailureKind::Rejected, &rejection.code))
        }
        Err(fault) => Err(Failure::new(FailureKind::Retryable, &fault.code)),
    }
}

fn target_plans(
    binding: &PreparedBinding,
    backend: FilePolicyBackend,
) -> Result<Vec<(String, TargetBindingPlan)>, Failure> {
    let asc_policy_types::authoring::PolicySource::Cedar(source) = &binding.policy.template else {
        return Ok(vec![(
            backend.route().into(),
            translated_plan(match backend {
                FilePolicyBackend::AgentSight => AgentSightTranslator.translate(binding),
                FilePolicyBackend::LuaLsm => LuaLsmTranslator::default().translate(binding),
            })?,
        )]);
    };
    let invalid_source = |_| Failure::new(FailureKind::Rejected, "INVALID_CEDAR_POLICY");
    let (files, network) = if let Some(selection) = &binding.policy.deployment {
        let selected = asc_policy_cedar::deployment::selected_pst(source, selection)
            .map_err(invalid_source)?;
        match selection.backend {
            asc_policy_types::policy::DeploymentBackend::ActPlane => (Some(selected), None),
            asc_policy_types::policy::DeploymentBackend::Gateway => (None, Some(selected)),
        }
    } else {
        let syntax = asc_policy_cedar::pst(source).map_err(invalid_source)?;
        asc_policy_cedar::manual_binding_views(&syntax, binding.policy.uses_credentials())
    };
    let mut plans = Vec::new();
    if let Some(files) = files {
        plans.push((
            backend.route().into(),
            translated_plan(match backend {
                FilePolicyBackend::AgentSight => {
                    AgentSightTranslator.translate_cedar_pst(binding, &files)
                }
                FilePolicyBackend::LuaLsm => {
                    LuaLsmTranslator::default().translate_cedar_pst(binding, &files)
                }
            })?,
        ));
    }
    if let Some(network) = network {
        let plan = asc_gateway_plan::compile_binding(asc_gateway_plan::GatewayBindingView {
            source: asc_gateway_plan::GatewayBindingSource::from_binding(binding),
            schema: &source.schema,
            credential_bindings: &binding.policy.credential_bindings,
            network: &network,
        })
        .map_err(|_| Failure::new(FailureKind::Rejected, "GATEWAY_UNSUPPORTED_NETWORK_POLICY"))?;
        let content = serde_json::to_vec(&plan)
            .map_err(|_| Failure::new(FailureKind::Retryable, "GATEWAY_ENCODING_FAILED"))?;
        plans.push((
            "gateway".into(),
            TargetBindingPlan {
                format: asc_gateway_plan::GATEWAY_BINDING_FORMAT.into(),
                content,
            },
        ));
    }
    Ok(plans)
}

#[cfg(test)]
#[path = "reconciliation/mixed_tests.rs"]
mod mixed_tests;

fn recover_file_bindings<R: BindingStateRepository + BindingReconcileCatalog>(
    repository: &R,
) -> Result<BTreeMap<asc_policy_types::identifiers::ResourceId, String>, StoreError> {
    let mut routes = BTreeMap::new();
    let mut cursor = None;
    loop {
        let batch = repository.scan_reconciliation(cursor.as_ref(), 128)?;
        for candidate in &batch {
            let Some(record) = repository.get_binding_state(&candidate.id)? else {
                continue;
            };
            if let Some(route) = record.deployments.iter().find_map(|deployment| {
                matches!(
                    deployment.target.route.as_str(),
                    "agentsight" | LUA_LSM_ROUTE
                )
                .then(|| deployment.target.route.clone())
            }) {
                routes.insert(candidate.id.clone(), route);
            }
        }
        if batch.len() < 128 {
            break;
        }
        cursor = batch.last().map(|candidate| candidate.id.clone());
    }
    Ok(routes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_pap::PapRepository;
    use asc_pcp::{DeploymentReport, Observation, PreparedApply, Presence, TargetRef};
    use asc_policy_repository::{BindingStateSnapshot, BindingStateWrite, Deployment, WriteResult};
    use asc_policy_repository_sqlite::SqlitePolicyRepository;
    use asc_policy_types::binding::BindingStatus;
    use asc_policy_types::identifiers::ResourceId;
    use asc_policy_types::scope::{PreparedScope, ProcessMatcher, ScopeSelector, ScopeStatus};
    use std::sync::{Condvar, mpsc};
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn file_backend_selection_matches_the_deployment_contract_fixture() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/file-backends.json")).unwrap();
        for case in fixture.as_array().unwrap() {
            let selected = FilePolicyBackend::parse(case["value"].as_str());
            let expected = if cfg!(all(target_os = "linux", feature = "lua-lsm-testing")) {
                case.get("linuxTestRoute").unwrap_or(&case["route"])
            } else {
                &case["route"]
            };
            if let Some(route) = expected.as_str() {
                assert_eq!(selected.unwrap().route(), route);
            } else {
                assert_eq!(selected, Err(StoreError::Invalid));
            }
        }
    }

    #[test]
    fn restart_preserves_ready_mixed_binding_and_saved_file_route() {
        let repository = SqlitePolicyRepository::temporary().unwrap();
        let scope = admit_scope(&repository, "lua-recovery", false, 1);
        let id = binding_ids(&repository, &scope).remove(0);
        let original = repository.get_binding_state(&id).unwrap().unwrap();
        let mut ready = original.clone();
        ready.binding.status = BindingStatus::Ready.into();
        ready.deployments = vec![Deployment {
            target: TargetRef {
                route: LUA_LSM_ROUTE.into(),
                id: "asc_recovery_r1".into(),
                cleanup: b"asc_recovery_r1".to_vec(),
            },
            revision: ready.binding.spec.binding_revision,
            presence: Presence::Present,
            last_confirmed: Some(Presence::Present),
        }];
        ready.deployments.push(Deployment {
            target: TargetRef {
                route: "gateway".into(),
                id: "gateway_recovery".into(),
                cleanup: b"gateway_recovery".to_vec(),
            },
            ..ready.deployments[0].clone()
        });
        repository
            .compare_exchange_binding_state(&original, &BindingStateWrite::new(ready))
            .unwrap();
        let before = repository.get_binding_state(&id).unwrap().unwrap();
        let routes = recover_file_bindings(&repository).unwrap();
        assert_eq!(routes.get(&id).unwrap(), LUA_LSM_ROUTE);
        assert_eq!(repository.get_binding_state(&id).unwrap().unwrap(), before);
    }

    struct TestClock;
    impl Clock for TestClock {
        fn now_ms(&self) -> u64 {
            1234
        }
    }

    #[derive(Default)]
    struct CreateGate {
        released: Mutex<bool>,
        changed: Condvar,
    }
    impl CreateGate {
        fn release(&self) {
            *self.released.lock().unwrap() = true;
            self.changed.notify_all();
        }
    }
    struct ReleaseOnDrop(Arc<CreateGate>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    struct ScriptedTargetClient {
        route: &'static str,
        operations: Mutex<Vec<&'static str>>,
        blocked_create: Option<(mpsc::Sender<()>, Arc<CreateGate>)>,
    }
    impl TargetDeploymentClient for ScriptedTargetClient {
        fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
            self.operations.lock().unwrap().push("prepare");
            let binding: PreparedBinding = serde_json::from_slice(&plan.content).unwrap();
            Ok(PreparedApply {
                target: TargetRef {
                    route: self.route.into(),
                    id: binding.binding_id.to_string(),
                    cleanup: vec![1],
                },
                format: plan.format.clone(),
                content: plan.content.clone(),
            })
        }
        fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
            let first = {
                let mut operations = self.operations.lock().unwrap();
                let first = !operations.contains(&"create");
                operations.push("create");
                first
            };
            if first && let Some((entered, gate)) = &self.blocked_create {
                entered.send(()).unwrap();
                let released = gate.released.lock().unwrap();
                let (released, timeout) = gate
                    .changed
                    .wait_timeout_while(released, WAIT * 4, |released| !*released)
                    .unwrap();
                assert!(*released && !timeout.timed_out(), "create was not released");
            }
            DeploymentReport {
                observations: vec![Observation {
                    target: prepared.target.clone(),
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
                    .map(|target| Observation {
                        target: target.clone(),
                        presence: Presence::Absent,
                    })
                    .collect(),
                error: None,
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum PendingWork {
        CredentialApply,
        CredentialDelete,
        GatewayCleanup,
    }

    fn admit_scope(
        repository: &SqlitePolicyRepository,
        name: &str,
        credential: bool,
        count: u32,
    ) -> PreparedScope {
        let mut spec: PreparedBinding = serde_json::from_str(include_str!(
            "../../../crates/asc-policy-types/tests/fixtures/prepared-binding.json"
        ))
        .unwrap();
        spec.policy.policy_id = ResourceId::new(format!("policy:{name}")).unwrap();
        if credential {
            spec.policy.template = serde_json::from_value(serde_json::json!({
                "language": "cedar",
                "content": {
                    "schema": {"schemaId": "seccore.system", "revision": 3},
                    "text": r#"@id("reconciliation-credential")
@modify("inject_credential")
@real_token_file("/secure/github-real")
@fake_token_file("/secure/github-fake")
@inject_header("Authorization")
@inject_format("auto")
@response_rewrite("true")
permit(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
when {
    resource.host == "api.github.com" && context.input.scheme == "https" &&
    context.input.method == "GET" && context.input.path like "/repos/*"
};"#
                }
            }))
            .unwrap();
            spec.policy.credential_bindings = vec![asc_policy_types::policy::CredentialBinding {
                rule_id: "reconciliation-credential".into(),
                pair_id: "pair-fixture".into(),
            }];
        }
        repository.put_policy(&spec.policy).unwrap();
        let scope = repository
            .put_scope(&PreparedScope {
                scope_id: ResourceId::new(format!("scope:{name}")).unwrap(),
                selector: ScopeSelector::Process {
                    matcher: ProcessMatcher::Name {
                        process_name: "worker".into(),
                    },
                },
                policy_snapshots: vec![spec.policy],
                owner_uid: credential.then_some(4001),
                status: ScopeStatus::Active,
            })
            .unwrap();
        let processes: Vec<_> = (0..count)
            .map(|index| {
                let mut process = spec.scope.process.clone();
                process.pid += index;
                process
            })
            .collect();
        repository
            .sync_scope_instances(&scope.scope_id, &processes)
            .unwrap();
        scope
    }

    fn binding_ids(repository: &SqlitePolicyRepository, scope: &PreparedScope) -> Vec<ResourceId> {
        repository
            .list_bindings(10, 0)
            .unwrap()
            .items
            .into_iter()
            .filter(|binding| binding.spec.scope.scope_id == scope.scope_id)
            .map(|binding| binding.spec.binding_id)
            .collect()
    }

    fn prepare_saved_delete(
        repository: &SqlitePolicyRepository,
        scope: &PreparedScope,
        ids: &[ResourceId],
        route: &str,
    ) {
        // Cleanup follows the saved target even when the current template uses a different route.
        for id in ids {
            let current = repository.get_binding_state(id).unwrap().unwrap();
            let mut ready = current.clone();
            ready.binding.status = BindingStatus::Ready.into();
            ready.deployments = vec![Deployment {
                target: TargetRef {
                    route: route.into(),
                    id: id.to_string(),
                    cleanup: vec![1],
                },
                revision: ready.binding.spec.binding_revision,
                presence: Presence::Present,
                last_confirmed: Some(Presence::Present),
            }];
            assert!(matches!(
                repository
                    .compare_exchange_binding_state(&current, &BindingStateWrite::new(ready))
                    .unwrap(),
                WriteResult::Applied(_)
            ));
        }
        repository
            .sync_scope_instances(&scope.scope_id, &[])
            .unwrap();
    }

    fn client_factory(
        client: Arc<dyn TargetDeploymentClient>,
    ) -> Arc<dyn TargetDeploymentClientFactory> {
        Arc::new(move || Ok(client.clone()))
    }

    #[cfg(not(all(target_os = "linux", feature = "lua-lsm-testing")))]
    #[test]
    fn default_daemon_retains_saved_lua_cleanup_when_client_is_unavailable() {
        let repository = Arc::new(SqlitePolicyRepository::temporary().unwrap());
        let scope = admit_scope(&repository, "saved-lua-cleanup", false, 1);
        let ids = binding_ids(&repository, &scope);
        prepare_saved_delete(&repository, &scope, &ids, LUA_LSM_ROUTE);
        let id = &ids[0];
        let pending = repository.get_binding_state(id).unwrap().unwrap();
        let reconciler = build_reconciler(
            repository.clone(),
            Arc::new(|| panic!("saved Lua-LSM cleanup reached AgentSight")),
            None,
            Arc::new(TestClock),
            FilePolicyBackend::AgentSight,
        )
        .unwrap();
        let result = reconciler
            .reconcile(id, &mut AttemptSchedule::default())
            .unwrap();
        assert!(matches!(result, Disposition::RetryAt { .. }));
        let after = repository.get_binding_state(id).unwrap().unwrap();
        assert_eq!(after.binding.status.phase, BindingStatus::PendingDelete);
        assert_eq!(
            after.binding.status.error.unwrap().code,
            "RECONCILE_TARGET_UNAVAILABLE"
        );
        assert_eq!(after.deployments.len(), 1);
        assert_eq!(after.deployments[0].target, pending.deployments[0].target);
    }

    fn test_admission(
        repository: Arc<SqlitePolicyRepository>,
        gateway: Arc<ScriptedTargetClient>,
        file: Arc<ScriptedTargetClient>,
    ) -> GatewayAdmission<SqlitePolicyRepository> {
        let factories = BTreeMap::from([
            ("gateway".into(), client_factory(gateway)),
            ("agentsight".into(), client_factory(file)),
        ]);
        let core = Arc::new(
            BindingReconciler::new(
                repository.clone(),
                Arc::new(|binding: &PreparedBinding| {
                    Ok(TranslationOutcome::Translated(TargetBindingPlan {
                        format: "test-binding/v1".into(),
                        content: serde_json::to_vec(binding).unwrap(),
                    }))
                }),
                factories,
                "agentsight".into(),
                Arc::new(TestClock),
                RetryPolicy {
                    max_attempts: 1,
                    base_delay_ms: 1000,
                    max_delay_ms: 1000,
                },
            )
            .unwrap()
            .with_route_selector(|binding| {
                if binding.policy.uses_credentials() {
                    "gateway".into()
                } else {
                    "agentsight".into()
                }
            }),
        );
        GatewayAdmission {
            repository,
            core,
            gateway: Mutex::new(()),
        }
    }

    struct AdmissionFixture {
        admission: GatewayAdmission<SqlitePolicyRepository>,
        gateway: Arc<ScriptedTargetClient>,
        file: Arc<ScriptedTargetClient>,
        gate: Arc<CreateGate>,
        first_id: ResourceId,
        file_id: ResourceId,
        pending: Vec<BindingStateSnapshot>,
    }

    impl AdmissionFixture {
        fn new(work: PendingWork) -> (Self, mpsc::Receiver<()>) {
            let repository = Arc::new(SqlitePolicyRepository::temporary().unwrap());
            let first_scope = admit_scope(&repository, "first", true, 1);
            let deferred_scope = admit_scope(
                &repository,
                "deferred",
                !matches!(work, PendingWork::GatewayCleanup),
                4,
            );
            let file_scope = admit_scope(&repository, "file", false, 1);
            let first_id = binding_ids(&repository, &first_scope)[0].clone();
            let file_id = binding_ids(&repository, &file_scope)[0].clone();
            let ids = binding_ids(&repository, &deferred_scope);
            if !matches!(work, PendingWork::CredentialApply) {
                prepare_saved_delete(&repository, &deferred_scope, &ids, "gateway");
            }
            let pending: Vec<_> = ids
                .iter()
                .map(|id| repository.get_binding_state(id).unwrap().unwrap())
                .collect();
            let expected_phase = if matches!(work, PendingWork::CredentialApply) {
                BindingStatus::PendingApply
            } else {
                BindingStatus::PendingDelete
            };
            assert_eq!(pending.len(), 4);
            assert!(
                pending
                    .iter()
                    .all(|record| record.binding.status.phase == expected_phase)
            );
            let (entered_tx, entered_rx) = mpsc::channel();
            let gate = Arc::new(CreateGate::default());
            let gateway = Arc::new(ScriptedTargetClient {
                route: "gateway",
                operations: Mutex::new(vec![]),
                blocked_create: Some((entered_tx, gate.clone())),
            });
            let file = Arc::new(ScriptedTargetClient {
                route: "agentsight",
                operations: Mutex::new(vec![]),
                blocked_create: None,
            });
            let admission = test_admission(repository, gateway.clone(), file.clone());
            (
                Self {
                    admission,
                    gateway,
                    file,
                    gate,
                    first_id,
                    file_id,
                    pending,
                },
                entered_rx,
            )
        }

        fn phase(&self, id: &ResourceId) -> BindingStatus {
            self.admission
                .repository
                .get_binding_state(id)
                .unwrap()
                .unwrap()
                .binding
                .status
                .phase
        }

        fn assert_deferred(
            &self,
            work: PendingWork,
            id: &ResourceId,
            result: Result<Disposition, StoreError>,
            schedule: &AttemptSchedule,
        ) {
            assert_eq!(
                result.unwrap(),
                Disposition::DeferredAt { at: 2234 },
                "{work:?}"
            );
            assert_eq!(*schedule, AttemptSchedule::default());
            let expected = self
                .pending
                .iter()
                .find(|record| &record.binding.spec.binding_id == id)
                .unwrap();
            assert_eq!(
                self.admission
                    .repository
                    .get_binding_state(id)
                    .unwrap()
                    .as_ref(),
                Some(expected)
            );
        }

        fn complete_deferred(
            &self,
            work: PendingWork,
            deferred: Vec<(ResourceId, AttemptSchedule)>,
        ) {
            let mut expected = vec!["prepare", "create"];
            for (id, mut schedule) in deferred {
                assert_eq!(
                    self.admission.reconcile(&id, &mut schedule).unwrap(),
                    Disposition::Completed
                );
                assert_eq!(schedule.attempts_started, 1);
                let actual = self.admission.repository.get_binding_state(&id).unwrap();
                if matches!(work, PendingWork::CredentialApply) {
                    let actual = actual.unwrap();
                    assert_eq!(actual.deployments.len(), 1);
                    assert_eq!(
                        (
                            actual.binding.status.phase,
                            actual.deployments[0].target.route.as_str(),
                            actual.deployments[0].presence,
                        ),
                        (BindingStatus::Ready, "gateway", Presence::Present)
                    );
                    expected.extend(["prepare", "create"]);
                } else {
                    assert_eq!(actual, None);
                    expected.push("delete");
                }
            }
            assert_eq!(*self.gateway.operations.lock().unwrap(), expected);
        }
    }

    #[test]
    fn gateway_admission_defers_pending_work_without_budget_or_file_starvation() {
        for work in [
            PendingWork::CredentialApply,
            PendingWork::CredentialDelete,
            PendingWork::GatewayCleanup,
        ] {
            let (fixture, entered_rx) = AdmissionFixture::new(work);
            std::thread::scope(|threads| {
                // Release before scoped joins, including assertion failures on a regression.
                let _release = ReleaseOnDrop(fixture.gate.clone());
                let first = threads.spawn(|| {
                    let mut schedule = AttemptSchedule::default();
                    let result = fixture
                        .admission
                        .reconcile(&fixture.first_id, &mut schedule);
                    (result, schedule)
                });
                entered_rx
                    .recv_timeout(WAIT)
                    .expect("first gateway create must start");
                let (results_tx, results_rx) = mpsc::channel();
                for record in &fixture.pending {
                    let id = &record.binding.spec.binding_id;
                    let results_tx = results_tx.clone();
                    let admission = &fixture.admission;
                    threads.spawn(move || {
                        let mut schedule = AttemptSchedule::default();
                        let result = admission.reconcile(id, &mut schedule);
                        results_tx.send((id.clone(), result, schedule)).unwrap();
                    });
                }
                let mut deferred = Vec::new();
                for _ in &fixture.pending {
                    let (id, result, schedule) = results_rx
                        .recv_timeout(WAIT)
                        .expect("busy gateway work must defer, not occupy another worker");
                    fixture.assert_deferred(work, &id, result, &schedule);
                    deferred.push((id, schedule));
                }
                assert_eq!(
                    *fixture.gateway.operations.lock().unwrap(),
                    vec!["prepare", "create"]
                );
                let (file_tx, file_rx) = mpsc::channel();
                let file_admission = &fixture.admission;
                let file_binding_id = &fixture.file_id;
                threads.spawn(move || {
                    let mut schedule = AttemptSchedule::default();
                    file_tx
                        .send(file_admission.reconcile(file_binding_id, &mut schedule))
                        .unwrap();
                });
                assert_eq!(
                    file_rx
                        .recv_timeout(WAIT)
                        .expect("file work must remain runnable")
                        .unwrap(),
                    Disposition::Completed
                );
                assert_eq!(fixture.phase(&fixture.file_id), BindingStatus::Ready);
                assert_eq!(
                    *fixture.file.operations.lock().unwrap(),
                    vec!["prepare", "create"]
                );
                assert!(!first.is_finished(), "gateway create must still be blocked");
                assert_eq!(fixture.phase(&fixture.first_id), BindingStatus::Applying);
                fixture.gate.release();
                let (result, schedule) = first.join().unwrap();
                assert_eq!(result.unwrap(), Disposition::Completed);
                assert_eq!(schedule.attempts_started, 1);
                fixture.complete_deferred(work, deferred);
            });
        }
    }
}
