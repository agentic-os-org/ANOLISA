//! Target-specific composition; queue and lifecycle remain in Policy Runtime.
use asc_agentsight_client::AgentSightClientFactory;
use asc_pcp::{
    AttemptSchedule, BindingReconciler, Clock, Disposition, RetryPolicy, StoreError,
    TargetDeploymentClient, TargetDeploymentClientFactory,
};
use asc_policy_repository::{BindingReconcileCatalog, BindingStateRepository};
use asc_policy_runtime::reconciliation::{
    MonotonicClock, ReconcileAttempt, ReconciliationRuntime, RuntimeConfig,
};
use asc_policy_translator_agentsight::AgentSightTranslator;
use asc_policy_types::Validate;
use asc_policy_types::binding::PreparedBinding;
use asc_policy_types::target::{
    AdapterFault, Failure, FailureKind, TargetBindingPlan, TranslationOutcome,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, TryLockError};

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
                record.binding.spec.policy.template.uses_credentials()
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
    start_with_factories(repository, Arc::new(move || Ok(client.clone())), None)
}

fn start_with_factories<R: BindingStateRepository + BindingReconcileCatalog + 'static>(
    repository: Arc<R>,
    file_factory: Arc<dyn TargetDeploymentClientFactory>,
    gateway_factory: Option<Arc<dyn TargetDeploymentClientFactory>>,
) -> Result<ReconciliationRuntime, StoreError> {
    let clock = Arc::new(MonotonicClock::default());
    let mut factories = BTreeMap::from([("agentsight".into(), file_factory)]);
    if let Some(factory) = gateway_factory {
        factories.insert("gateway".into(), factory);
    }
    let adapter = |binding: &PreparedBinding| {
        if binding.policy.template.uses_credentials() {
            binding.validate().map_err(|_| AdapterFault {
                code: "GATEWAY_INVALID_BINDING".into(),
            })?;
            let content = serde_json::to_vec(binding).map_err(|_| AdapterFault {
                code: "GATEWAY_ENCODING_FAILED".into(),
            })?;
            Ok(TranslationOutcome::Translated(TargetBindingPlan {
                format: "agent-sec-gateway-binding/v1".into(),
                content,
            }))
        } else {
            AgentSightTranslator.translate(binding)
        }
    };
    let core = Arc::new(
        BindingReconciler::new(
            repository.clone(),
            Arc::new(adapter),
            factories,
            "agentsight".into(),
            clock,
            RetryPolicy {
                max_attempts: 5,
                base_delay_ms: 1000,
                max_delay_ms: 30_000,
            },
        )?
        .with_route_selector(|binding| {
            if binding.policy.template.uses_credentials() {
                "gateway".into()
            } else {
                "agentsight".into()
            }
        }),
    );
    let attempt = Arc::new(GatewayAdmission {
        repository: repository.clone(),
        core,
        gateway: Mutex::new(()),
    });
    ReconciliationRuntime::start(repository, attempt, RuntimeConfig::default())
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
            spec.policy.template = asc_policy_types::authoring::PolicySource::SecCore(
                serde_json::from_value(serde_json::json!({
                    "specVersion":"0.1", "rules":[{
                        "effect":"allow", "category":"network", "action":"use_credential",
                        "target":{"type":"http_request", "credentialId":"github",
                            "scheme":"https", "host":"api.github.com", "path":"/repos/**",
                            "methods":["GET"], "responseRewrite":true}
                    }]
                }))
                .unwrap(),
            );
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

    fn prepare_gateway_delete(
        repository: &SqlitePolicyRepository,
        scope: &PreparedScope,
        ids: &[ResourceId],
    ) {
        // Model durable gateway responsibility even when the current template is file-only.
        for id in ids {
            let current = repository.get_binding_state(id).unwrap().unwrap();
            let mut ready = current.clone();
            ready.binding.status = BindingStatus::Ready.into();
            ready.deployments = vec![Deployment {
                target: TargetRef {
                    route: "gateway".into(),
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
                if binding.policy.template.uses_credentials() {
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
                prepare_gateway_delete(&repository, &deferred_scope, &ids);
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
