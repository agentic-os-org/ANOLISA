//! Actual Adapter -> Reconciler -> actual Client/Ureq -> loopback HTTP PEP mock.
#![allow(clippy::too_many_lines)]

#[path = "../../asc-agentsight-client/tests/common/mod.rs"]
mod common;
#[path = "support/http.rs"]
mod http;

#[path = "support/store.rs"]
mod store;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use store::{TestAdmission, TestStore};

use asc_agentsight_client::*;
use asc_pcp::*;
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_translator_agentsight::AgentSightTranslator;
use asc_policy_types::binding::{BindingStatus, BindingView, PreparedBinding};
use asc_policy_types::identifiers::{ResourceId, Revision};
use common::*;
use http::{Exchange, MockHttp};
use serde_json::Value;

#[derive(Default)]
struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 3,
        base_delay_ms: 100,
        max_delay_ms: 150,
    }
}

fn spec(revision: u32) -> PreparedBinding {
    let mut spec: PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    spec.binding_revision = Revision::new(revision).unwrap();
    spec.policy.policy_name = format!("prevent deletion {revision}");
    spec
}

fn initial() -> ReconcileRecord {
    ReconcileRecord {
        status_version: 1,
        binding: BindingView {
            spec: spec(7),
            status: (BindingStatus::PendingApply).into(),
        },
        deployments: vec![],
    }
}

#[test]
fn cedar_delete_and_write_flow_through_pap_sqlite_reconciler_and_http() {
    use asc_pap::{PapService, ScopeBindingSink};
    use asc_policy_types::scope::{PolicyReference, ScopeSelector};
    use asc_policy_types::target::TranslationOutcome;
    use serde_json::json;

    struct Discovery;
    impl asc_pap::ScopeDiscovery for Discovery {
        fn start(&self, _: &asc_pap::ScopeDiscoverySeed) -> Result<(), asc_pap::PapError> {
            Ok(())
        }
        fn stop(&self, _: &ResourceId) -> Result<(), asc_pap::PapError> {
            Ok(())
        }
    }

    for (source, delete, write) in [
        (
            include_str!(
                "../../../fixtures/adapters/agentsight/cedar-file-protection/prevent-delete.json"
            ),
            true,
            false,
        ),
        (
            include_str!(
                "../../../fixtures/adapters/agentsight/cedar-file-protection/prevent-write.json"
            ),
            false,
            true,
        ),
    ] {
        let repo = Arc::new(SqlitePolicyRepository::temporary().unwrap());
        let pap = PapService::new(repo.clone()).with_scope_discovery(Arc::new(Discovery));
        let policy = pap
            .create_policy(
                "Cedar file protection",
                &serde_json::from_str(source).unwrap(),
            )
            .unwrap();
        let scope = pap
            .create_scope_assignment(
                &ScopeSelector::Pid { pid: 4242 },
                &[PolicyReference {
                    policy_id: policy.policy_id.clone(),
                    policy_revision: policy.revision,
                }],
                None,
            )
            .unwrap();
        pap.sync_instances(&scope.scope_id, &[spec(7).scope.process])
            .unwrap();
        let binding = pap.list_bindings(10, 0).unwrap().items.remove(0);
        assert_eq!(binding.spec.policy, policy);
        let id = binding.spec.binding_id.clone();
        let TranslationOutcome::Translated(plan) =
            AgentSightTranslator.translate(&binding.spec).unwrap()
        else {
            panic!("expected Cedar translation");
        };
        let prepared = AgentSightClient::with_dependencies(Wire::new([]), Identity::default())
            .prepare_apply(&plan)
            .unwrap();
        let payload: Value = serde_json::from_slice(&prepared.content).unwrap();
        let bytes: Vec<u8> = serde_json::from_value(payload["request"].clone()).unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body["file_protection_policy"],
            json!({
                "version":1,"topology_mode":"static_at_apply","targets":[{
                    "path":"/workspace","kind":"directory_tree",
                    "protections":{"delete":delete,"ordinary_write":write},
                    "exclusions":[{"path":"/workspace/cache","kind":"directory_subtree"}]
                }]
            })
        );
        assert!(body.get("policy_dsl").is_none());
        let inspect_repo = repo.clone();
        let inspect_id = id.clone();
        let server = MockHttp::start(
            vec![
                Exchange {
                    request: request(AgentSightHttpMethod::Get, ENFORCEMENT_HEALTH_PATH, None),
                    response: Some(response(
                        200,
                        &serde_json::to_vec(&structured_health()).unwrap(),
                    )),
                },
                Exchange {
                    request: request(
                        AgentSightHttpMethod::Post,
                        ENFORCEMENT_BINDINGS_PATH,
                        Some(bytes),
                    ),
                    response: Some(response(
                        200,
                        &serde_json::to_vec(
                            &json!({"request":body,"state":"enforced","domain_id":1}),
                        )
                        .unwrap(),
                    )),
                },
                Exchange {
                    request: request(
                        AgentSightHttpMethod::Delete,
                        &path(&prepared.target.id),
                        None,
                    ),
                    response: Some(response(204, b"")),
                },
            ],
            move |request| {
                if request.method == AgentSightHttpMethod::Post {
                    let saved = inspect_repo.read(&inspect_id).unwrap().unwrap();
                    assert_eq!(saved.deployments.len(), 1);
                    assert_eq!(saved.deployments[0].presence, Presence::Unknown);
                }
            },
        );
        let worker = core(
            repo.clone(),
            Arc::new(TestClock::default()),
            &server.base_url,
        );
        let mut schedule = AttemptSchedule::default();
        assert_eq!(
            worker.reconcile(&id, &mut schedule).unwrap(),
            Disposition::Completed
        );
        assert_eq!(
            pap.get_binding(&id).unwrap().status.phase,
            BindingStatus::Ready
        );
        assert_eq!(
            repo.read(&id).unwrap().unwrap().deployments[0].presence,
            Presence::Present
        );
        pap.delete_scope(&scope.scope_id).unwrap();
        assert_eq!(
            worker.reconcile(&id, &mut schedule).unwrap(),
            Disposition::Completed
        );
        assert_eq!(pap.get_binding(&id), Err(asc_pap::PapError::NotFound));
        server.finish(3);
    }
}

fn deployment(revision: u32, presence: Presence, confirmed: Option<Presence>) -> Deployment {
    Deployment {
        target: prepared(revision).target,
        revision: Revision::new(revision).unwrap(),
        presence,
        last_confirmed: confirmed,
    }
}

fn ready(revision: u32, attempts: u32, _is_update: bool) -> ReconcileRecord {
    ReconcileRecord {
        status_version: i64::from(attempts) * 2 + 1,
        binding: {
            let mut binding = BindingView {
                spec: spec(revision),
                status: (BindingStatus::Ready).into(),
            };
            binding.status.error = None;
            binding
        },

        deployments: vec![deployment(
            revision,
            Presence::Present,
            Some(Presence::Present),
        )],
    }
}

fn request(
    method: AgentSightHttpMethod,
    path: &str,
    body: Option<Vec<u8>>,
) -> AgentSightHttpRequest {
    AgentSightHttpRequest {
        method,
        path: path.into(),
        body,
    }
}
fn health() -> Exchange {
    Exchange {
        request: request(AgentSightHttpMethod::Get, "/enforcement/health", None),
        response: Some(response(200, HEALTH)),
    }
}
fn post(revision: u32, response: AgentSightHttpResponse) -> Exchange {
    let payload: Value = serde_json::from_slice(&prepared(revision).content).unwrap();
    Exchange {
        request: request(
            AgentSightHttpMethod::Post,
            "/enforcement/bindings",
            Some(serde_json::from_value(payload["request"].clone()).unwrap()),
        ),
        response: Some(response),
    }
}
fn delete(revision: u32, response: Option<AgentSightHttpResponse>) -> Exchange {
    Exchange {
        request: request(
            AgentSightHttpMethod::Delete,
            &path(if revision == 7 { ID7 } else { ID8 }),
            None,
        ),
        response,
    }
}

fn core(
    repo: Arc<dyn BindingStateRepository>,
    clock: Arc<TestClock>,
    url: &str,
) -> BindingReconciler {
    let transport = UreqAgentSightTransport::new(url, "test-local-only").unwrap();
    let client: Arc<dyn TargetDeploymentClient> = Arc::new(AgentSightClient::with_dependencies(
        transport,
        Identity::default(),
    ));
    let translator = |binding: &PreparedBinding| AgentSightTranslator.translate(binding);
    BindingReconciler::new(
        repo,
        Arc::new(translator),
        BTreeMap::from([(
            DEFAULT_AGENTSIGHT_ROUTE.into(),
            Arc::new(move || Ok(client.clone())) as Arc<dyn asc_pcp::TargetDeploymentClientFactory>,
        )]),
        DEFAULT_AGENTSIGHT_ROUTE.into(),
        clock,
        policy(),
    )
    .unwrap()
}

fn inspect_registered(repo: &SqlitePolicyRepository, request: &AgentSightHttpRequest) {
    if request.method == AgentSightHttpMethod::Get {
        return;
    }
    let record = repo.read(&spec(7).binding_id).unwrap().unwrap();
    if request.method == AgentSightHttpMethod::Post {
        let prepared = prepared(record.binding.spec.binding_revision.get());
        let payload: Value = serde_json::from_slice(&prepared.content).unwrap();
        assert_eq!(
            request.body.as_ref().unwrap(),
            &serde_json::from_value::<Vec<u8>>(payload["request"].clone()).unwrap()
        );
        assert!(
            record
                .deployments
                .iter()
                .any(|d| d.target == prepared.target && d.presence == Presence::Unknown)
        );
    } else {
        assert!(
            record
                .deployments
                .iter()
                .any(|d| path(&d.target.id) == request.path && d.presence == Presence::Unknown)
        );
    }
}

#[test]
fn real_http_apply_then_uncertain_delete_and_retry() {
    let mut schedule = AttemptSchedule::default();
    let repo = Arc::new(SqlitePolicyRepository::with_binding_states(vec![initial()]).unwrap());
    let inspect = repo.clone();
    let server = MockHttp::start(
        vec![
            health(),
            post(7, applied(7)),
            delete(7, None),
            delete(7, Some(response(204, b""))),
        ],
        move |req| inspect_registered(&inspect, req),
    );
    let clock = Arc::new(TestClock::default());
    let worker = core(repo.clone(), clock.clone(), &server.base_url);
    let id = spec(7).binding_id;
    assert_eq!(
        worker.reconcile(&id, &mut schedule).unwrap(),
        Disposition::Completed
    );
    assert_eq!(repo.read(&id).unwrap(), Some(ready(7, 1, false)));

    let mut desired = ready(7, 1, false).binding;
    desired.status = BindingStatus::PendingDelete.into();
    assert!(
        repo.compare_exchange_reconcile_intent(
            &ExpectedBinding::from_snapshot(&ready(7, 1, false)),
            &desired
        )
        .unwrap()
    );
    assert_eq!(
        worker.reconcile(&id, &mut schedule).unwrap(),
        Disposition::RetryAt { at: 100 }
    );
    let failed = ReconcileRecord {
        status_version: 6,
        binding: {
            let mut binding = desired.clone();
            binding.status.error = Some(Failure::new(
                FailureKind::Retryable,
                "AGENTSIGHT_TRANSPORT_UNAVAILABLE",
            ));
            binding
        },

        deployments: vec![deployment(7, Presence::Unknown, Some(Presence::Present))],
    };
    assert_eq!(repo.read(&id).unwrap(), Some(failed));
    clock.0.store(99, Ordering::SeqCst);
    assert_eq!(
        worker.reconcile(&id, &mut schedule).unwrap(),
        Disposition::Skipped
    );
    clock.0.store(100, Ordering::SeqCst);
    assert_eq!(
        worker.reconcile(&id, &mut schedule).unwrap(),
        Disposition::Completed
    );
    assert_eq!(repo.read(&id).unwrap(), None);
    server.finish(4);
}

#[test]
fn real_http_partial_update_retry_cleans_old_once_and_reuses_new_request() {
    let mut schedule = AttemptSchedule::default();
    let repo =
        Arc::new(SqlitePolicyRepository::with_binding_states(vec![ready(7, 1, false)]).unwrap());
    let inspect = repo.clone();
    let server = MockHttp::start(
        vec![
            health(),
            delete(7, Some(response(204, b""))),
            post(8, remote_error(503, "enforcer_unavailable", true)),
            health(),
            post(8, applied(8)),
        ],
        move |req| inspect_registered(&inspect, req),
    );
    let clock = Arc::new(TestClock::default());
    let worker = core(repo.clone(), clock.clone(), &server.base_url);
    let id = spec(7).binding_id;
    let desired = BindingView {
        spec: spec(8),
        status: (BindingStatus::PendingApply).into(),
    };
    assert!(
        repo.compare_exchange_reconcile_intent(
            &ExpectedBinding::from_snapshot(&ready(7, 1, false)),
            &desired
        )
        .unwrap()
    );
    assert_eq!(
        worker.reconcile(&id, &mut schedule).unwrap(),
        Disposition::RetryAt { at: 100 }
    );
    assert_eq!(
        repo.read(&id).unwrap(),
        Some(ReconcileRecord {
            status_version: 6,
            binding: {
                let mut binding = desired;
                binding.status.error = Some(Failure::new(
                    FailureKind::Retryable,
                    "AGENTSIGHT_ENFORCER_UNAVAILABLE",
                ));
                binding
            },

            deployments: vec![deployment(8, Presence::Unknown, None)]
        })
    );
    clock.0.store(100, Ordering::SeqCst);
    assert_eq!(
        worker.reconcile(&id, &mut schedule).unwrap(),
        Disposition::Completed
    );
    assert_eq!(
        repo.read(&id).unwrap(),
        Some(ReconcileRecord {
            status_version: 8,
            ..ready(8, 2, true)
        })
    );
    server.finish(5);
}

#[test]
fn same_revision_delete_during_real_http_apply_preserves_target_for_cleanup() {
    let mut schedule = AttemptSchedule::default();
    let repo = Arc::new(SqlitePolicyRepository::with_binding_states(vec![initial()]).unwrap());
    let inspect = repo.clone();
    let server = MockHttp::start(
        vec![
            health(),
            post(7, applied(7)),
            delete(7, Some(response(204, b""))),
        ],
        move |req| {
            inspect_registered(&inspect, req);
            if req.method == AgentSightHttpMethod::Post {
                let record = inspect.read(&spec(7).binding_id).unwrap().unwrap();
                let mut desired = record.binding.clone();
                desired.status = BindingStatus::PendingDelete.into();
                assert!(
                    inspect
                        .compare_exchange_reconcile_intent(
                            &ExpectedBinding::from_snapshot(&record),
                            &desired
                        )
                        .unwrap()
                );
            }
        },
    );
    let worker = core(
        repo.clone(),
        Arc::new(TestClock::default()),
        &server.base_url,
    );
    let id = spec(7).binding_id;
    assert_eq!(
        worker.reconcile(&id, &mut schedule).unwrap(),
        Disposition::Superseded
    );
    assert_eq!(
        repo.read(&id).unwrap(),
        Some(ReconcileRecord {
            status_version: 3,
            binding: BindingView {
                spec: spec(7),
                status: BindingStatus::PendingDelete.into()
            },
            deployments: vec![deployment(7, Presence::Unknown, None)]
        })
    );
    assert_eq!(
        worker.reconcile(&id, &mut schedule).unwrap(),
        Disposition::Completed
    );
    assert_eq!(repo.read(&id).unwrap(), None);
    server.finish(3);
}

struct FailFinishOnce {
    repo: Arc<SqlitePolicyRepository>,
    fail: AtomicBool,
}
impl BindingStateRepository for FailFinishOnce {
    fn get_binding_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError> {
        self.repo.get_binding_state(id)
    }
    fn compare_exchange_binding_state(
        &self,
        expected: &BindingStateSnapshot,
        write: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError> {
        if store::write_phase(expected, write) == "finish"
            && self.fail.swap(false, Ordering::SeqCst)
        {
            return Err(StoreError::Unavailable);
        }
        self.repo.compare_exchange_binding_state(expected, write)
    }
}

#[test]
fn result_storage_failure_retains_success_without_repeating_http() {
    let mut schedule = AttemptSchedule::default();
    let repo = Arc::new(SqlitePolicyRepository::with_binding_states(vec![initial()]).unwrap());
    let inspect = repo.clone();
    let server = MockHttp::start(vec![health(), post(7, applied(7))], move |req| {
        inspect_registered(&inspect, req);
    });
    let wrapped = Arc::new(FailFinishOnce {
        repo: repo.clone(),
        fail: AtomicBool::new(true),
    });
    let clock = Arc::new(TestClock::default());
    let worker = core(wrapped, clock.clone(), &server.base_url);
    let id = spec(7).binding_id;
    assert_eq!(
        worker.reconcile(&id, &mut schedule),
        Err(StoreError::Unavailable)
    );
    let mut expected = ready(7, 1, false);
    expected.binding.status.phase = BindingStatus::Applying;
    expected.status_version = 2;
    expected.deployments = vec![deployment(7, Presence::Unknown, None)];
    assert_eq!(repo.read(&id).unwrap(), Some(expected));
    assert_eq!(
        worker.reconcile(&id, &mut schedule).unwrap(),
        Disposition::Completed
    );
    assert_eq!(schedule.attempts_started, 1);
    clock.0.store(100, Ordering::SeqCst);
    assert_eq!(
        worker.reconcile(&id, &mut schedule).unwrap(),
        Disposition::Skipped
    );
    assert_eq!(repo.read(&id).unwrap(), Some(ready(7, 1, false)));
    server.finish(2);
}

#[test]
fn retry_rejects_reused_process_identity_and_retains_unknown_cleanup() {
    let mut schedule = AttemptSchedule::default();
    for change_boot in [false, true] {
        let repo = Arc::new(SqlitePolicyRepository::with_binding_states(vec![initial()]).unwrap());
        let wire = Wire::new([
            Ok(response(200, HEALTH)),
            Err(AgentSightTransportError::Unavailable),
        ]);
        let identity = Identity::default();
        let client = Arc::new(AgentSightClient::with_dependencies(
            wire.clone(),
            identity.clone(),
        ));
        let clock = Arc::new(TestClock::default());
        let core = BindingReconciler::new(
            repo.clone(),
            Arc::new(|b: &PreparedBinding| AgentSightTranslator.translate(b)),
            BTreeMap::from([(DEFAULT_AGENTSIGHT_ROUTE.into(), {
                Arc::new(move || Ok(client.clone() as Arc<dyn TargetDeploymentClient>))
                    as Arc<dyn asc_pcp::TargetDeploymentClientFactory>
            })]),
            DEFAULT_AGENTSIGHT_ROUTE.into(),
            clock.clone(),
            policy(),
        )
        .unwrap();
        let id = spec(7).binding_id;
        assert_eq!(
            core.reconcile(&id, &mut schedule).unwrap(),
            Disposition::RetryAt { at: 100 }
        );
        let previous = repo.read(&id).unwrap().unwrap();
        if change_boot {
            identity.0.lock().unwrap().boot = Ok("20000000-0000-4000-8000-000000000002".into());
        } else {
            identity.0.lock().unwrap().start = Ok(987_655);
        }
        clock.0.store(100, Ordering::SeqCst);
        assert_eq!(
            core.reconcile(&id, &mut schedule).unwrap(),
            Disposition::Failed {
                error: Failure::new(FailureKind::Rejected, "AGENTSIGHT_PROCESS_IDENTITY_CHANGED")
            }
        );
        let after = repo.read(&id).unwrap().unwrap();
        assert_eq!(after.binding.spec, previous.binding.spec);
        assert_eq!(after.deployments, previous.deployments);
        assert_eq!(wire.requests().len(), 2);
        let cleanup: Value = serde_json::from_slice(&after.deployments[0].target.cleanup).unwrap();
        assert_eq!(
            cleanup,
            serde_json::json!({
                "schemaVersion": 2,
                "endpoint": "http://127.0.0.1:7396/api", "bindingId": id, "bindingRevision": 7
            })
        );
        wire.consumed();
    }
}
