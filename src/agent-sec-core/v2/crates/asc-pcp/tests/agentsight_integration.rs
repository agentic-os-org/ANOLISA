//! Actual Adapter -> Reconciler -> actual Client/Ureq -> loopback HTTP PEP mock.
#![allow(clippy::too_many_lines)]

#[path = "../../asc-agentsight-client/tests/common/mod.rs"]
mod common;
#[path = "support/http.rs"]
mod http;

#[path = "support/store.rs"]
mod store;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use store::{TestAdmission, TestStore};

use asc_agentsight_client::*;
use asc_pcp::*;
use asc_policy_adapter_agentsight::AgentSightAdapter;
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_types::binding::{BindingStatus, BindingView, PreparedBinding};
use asc_policy_types::identifiers::{ResourceId, Revision};
use asc_policy_types::target::TranslationOutcome;
use common::*;
use http::{Exchange, MockHttp};
use serde_json::{Value, json};

#[derive(Default)]
struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

const LIVE_WORKER: &str = r"
import os, sys
fds = {}
for line in sys.stdin:
    parts = line.rstrip('\n').split('\t')
    op = parts[0]
    try:
        if op == 'openfd':
            fds[parts[1]] = os.open(parts[1], os.O_RDWR)
        elif op == 'writefd':
            os.write(fds[parts[1]], b'-fd-write')
        elif op == 'write':
            with open(parts[1], 'ab') as f:
                f.write(b'-write')
        elif op == 'pwrite':
            fd = os.open(parts[1], os.O_RDWR)
            try:
                os.pwrite(fd, b'pwrite', 0)
            finally:
                os.close(fd)
        elif op == 'read':
            with open(parts[1], 'rb') as f:
                f.read()
        elif op == 'touch':
            with open(parts[1], 'wb') as f:
                f.write(b'created')
        elif op == 'unlink':
            os.unlink(parts[1])
        elif op == 'exit':
            break
        else:
            raise ValueError(op)
        print('ok', flush=True)
    except OSError as error:
        print('errno=' + str(error.errno), flush=True)
for fd in fds.values():
    os.close(fd)
";

struct LiveWorker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl LiveWorker {
    fn spawn() -> Self {
        let mut child = Command::new("python3")
            .args(["-u", "-c", LIVE_WORKER])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
        }
    }

    fn pid(&self) -> i32 {
        i32::try_from(self.child.id()).unwrap()
    }

    fn run(&mut self, operation: &str, path: &Path) -> String {
        writeln!(self.input, "{operation}\t{}", path.display()).unwrap();
        self.input.flush().unwrap();
        let mut result = String::new();
        self.output.read_line(&mut result).unwrap();
        result.trim().to_owned()
    }
}

impl Drop for LiveWorker {
    fn drop(&mut self) {
        let _ = writeln!(self.input, "exit");
        let _ = self.input.flush();
        let _ = self.child.wait();
    }
}

struct LiveFiles(PathBuf);

impl LiveFiles {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("asc-chain-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn path(&self, suffix: &str) -> PathBuf {
        self.0.join(suffix)
    }
}

impl Drop for LiveFiles {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
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

fn structured_spec(revision: u32) -> PreparedBinding {
    let mut spec = spec(revision);
    spec.policy.template = serde_json::from_value(json!({
        "specVersion": "0.2",
        "fileProtection": {
            "topology": "static_at_apply",
            "targets": [{
                "path": "/etc/agent/config.yaml",
                "kind": "exact_file",
                "protections": {"delete": true, "ordinaryWrite": true}
            }]
        }
    }))
    .unwrap();
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
    let adapter = |binding: &PreparedBinding| AgentSightAdapter.translate(binding);
    BindingReconciler::new(
        repo,
        Arc::new(adapter),
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
fn real_http_applies_structured_policy_from_adapter_through_client() {
    let desired = structured_spec(7);
    let TranslationOutcome::Translated(plan) = AgentSightAdapter.translate(&desired).unwrap()
    else {
        panic!("structured binding must translate");
    };
    let prepared = AgentSightClient::with_dependencies(Wire::new([]), Identity::default())
        .prepare_apply(&plan)
        .unwrap();
    let payload: Value = serde_json::from_slice(&prepared.content).unwrap();
    let request_body: Vec<u8> = serde_json::from_value(payload["request"].clone()).unwrap();
    let request_value: Value = serde_json::from_slice(&request_body).unwrap();
    assert!(request_value.get("policy_dsl").is_none());
    assert_eq!(
        request_value["file_protection_policy"]["targets"][0]["protections"]["ordinary_write"],
        true
    );
    let acknowledgement = serde_json::to_vec(&json!({
        "request": request_value,
        "state": "enforced",
        "domain_id": 7
    }))
    .unwrap();
    let repo = Arc::new(
        SqlitePolicyRepository::with_binding_states(vec![ReconcileRecord {
            status_version: 1,
            binding: BindingView {
                spec: desired.clone(),
                status: BindingStatus::PendingApply.into(),
            },
            deployments: vec![],
        }])
        .unwrap(),
    );
    let server = MockHttp::start(
        vec![
            Exchange {
                request: request(AgentSightHttpMethod::Get, "/enforcement/health", None),
                response: Some(response(200, STRUCTURED_HEALTH)),
            },
            Exchange {
                request: request(
                    AgentSightHttpMethod::Post,
                    "/enforcement/bindings",
                    Some(request_body),
                ),
                response: Some(response(200, &acknowledgement)),
            },
        ],
        |_| {},
    );
    let worker = core(
        repo.clone(),
        Arc::new(TestClock::default()),
        &server.base_url,
    );
    let mut schedule = AttemptSchedule::default();
    assert_eq!(
        worker
            .reconcile(&desired.binding_id, &mut schedule)
            .unwrap(),
        Disposition::Completed
    );
    let record = repo.read(&desired.binding_id).unwrap().unwrap();
    assert_eq!(record.binding.status.phase, BindingStatus::Ready);
    assert_eq!(record.deployments.len(), 1);
    assert_eq!(record.deployments[0].presence, Presence::Present);
    server.finish(2);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires isolated live AgentSight, ActPlane enforcer, BPF LSM, and root"]
fn live_structured_policy_reaches_bpf_lsm() {
    let base_url = std::env::var("ASC_E2E_AGENTSIGHT_BASE_URL").unwrap();
    let token_file = std::env::var_os("ASC_E2E_AGENTSIGHT_TOKEN_FILE").unwrap();
    let files = LiveFiles::new();
    let protected = files.path("protected");
    let excluded_file = protected.join("excluded.txt");
    let excluded_tree = protected.join("excluded-tree");
    let excluded_child = excluded_tree.join("free.txt");
    let nested_direct = excluded_tree.join("direct.txt");
    let existing = protected.join("existing.txt");
    let future = protected.join("future.txt");
    let write_only = files.path("write-only.txt");
    let delete_only = files.path("delete-only");
    let delete_only_file = delete_only.join("entry.txt");
    fs::create_dir_all(&excluded_tree).unwrap();
    fs::create_dir(&delete_only).unwrap();
    for path in [
        &excluded_file,
        &excluded_child,
        &nested_direct,
        &existing,
        &write_only,
        &delete_only_file,
    ] {
        fs::write(path, b"fixture").unwrap();
    }

    let mut worker = LiveWorker::spawn();
    assert_eq!(worker.run("openfd", &write_only), "ok");
    let resolver = ProcProcessIdentityResolver;
    let pid = worker.pid();
    let revision = u32::try_from(pid).unwrap();
    let mut binding = spec(revision);
    binding.scope.selector = asc_policy_types::scope::ScopeSelector::Pid { pid: revision };
    binding.scope.process = asc_policy_types::process_discovery::ProcessIdentity {
        boot_id: resolver.boot_id().unwrap(),
        pid_namespace: resolver.pid_namespace(pid).unwrap(),
        pid: u32::try_from(pid).unwrap(),
        start_time: resolver.process_start_time(pid).unwrap(),
    };
    binding.policy.template = serde_json::from_value(json!({
        "specVersion": "0.2",
        "fileProtection": {
            "topology": "static_at_apply",
            "targets": [
                {
                    "path": protected,
                    "kind": "directory_tree",
                    "protections": {"delete": true, "ordinaryWrite": true},
                    "exclusions": [
                        {"path": excluded_file, "kind": "exact_entry"},
                        {"path": excluded_tree, "kind": "directory_subtree"}
                    ]
                },
                {
                    "path": nested_direct,
                    "kind": "exact_file",
                    "protections": {"delete": true, "ordinaryWrite": true}
                },
                {
                    "path": write_only,
                    "kind": "exact_file",
                    "protections": {"delete": false, "ordinaryWrite": true}
                },
                {
                    "path": delete_only,
                    "kind": "directory_tree",
                    "protections": {"delete": true, "ordinaryWrite": false}
                }
            ]
        }
    }))
    .unwrap();
    let TranslationOutcome::Translated(plan) = AgentSightAdapter.translate(&binding).unwrap()
    else {
        panic!("live structured binding must translate");
    };
    let client = AgentSightClient::new_with_token_file(&base_url, &token_file).unwrap();
    let prepared = client.prepare_apply(&plan).unwrap();
    let applied = client.create(&prepared);
    assert!(applied.error.is_none(), "apply failed: {applied:?}");
    assert_eq!(applied.observations[0].presence, Presence::Present);

    assert_eq!(worker.run("read", &existing), "ok");
    assert_eq!(worker.run("write", &existing), "errno=1");
    assert_eq!(worker.run("pwrite", &existing), "errno=1");
    assert_eq!(worker.run("unlink", &existing), "errno=1");
    assert_eq!(worker.run("writefd", &write_only), "errno=1");
    assert_eq!(worker.run("write", &excluded_file), "ok");
    assert_eq!(worker.run("unlink", &excluded_file), "ok");
    assert_eq!(worker.run("write", &excluded_child), "ok");
    assert_eq!(worker.run("unlink", &excluded_child), "ok");
    assert_eq!(worker.run("write", &nested_direct), "errno=1");
    assert_eq!(worker.run("unlink", &nested_direct), "errno=1");
    assert_eq!(worker.run("write", &delete_only_file), "ok");
    assert_eq!(worker.run("unlink", &delete_only_file), "errno=1");
    assert_eq!(worker.run("touch", &future), "errno=1");
    assert_eq!(fs::read(&future).unwrap(), b"");

    let token = fs::read_to_string(&token_file).unwrap();
    let violations_url = format!(
        "{}/enforcement/violations?limit=100",
        base_url.trim_end_matches('/')
    );
    let mut observed = false;
    for _ in 0..40 {
        let response = ureq::get(&violations_url)
            .set("Authorization", &format!("Bearer {}", token.trim()))
            .call()
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = serde_json::from_reader(response.into_reader()).unwrap();
        let violations = body["violations"].as_array().unwrap();
        let saw_unlink = violations
            .iter()
            .any(|event| event["operation"] == "unlink" && event["blocked"] == true);
        let saw_write = violations
            .iter()
            .any(|event| event["operation"] == "ordinary_write" && event["blocked"] == true);
        if saw_unlink && saw_write {
            observed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        observed,
        "AgentSight did not expose both live denial operations"
    );

    let detached = client.delete_targets(std::slice::from_ref(&prepared.target));
    assert!(detached.error.is_none(), "detach failed: {detached:?}");
    assert_eq!(detached.observations[0].presence, Presence::Absent);
    assert_eq!(worker.run("write", &existing), "ok");
    assert_eq!(worker.run("unlink", &existing), "ok");
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
            Arc::new(|b: &PreparedBinding| AgentSightAdapter.translate(b)),
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
