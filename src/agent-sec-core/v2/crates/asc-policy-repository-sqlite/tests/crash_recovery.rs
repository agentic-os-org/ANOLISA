//! SIGKILL a real repository/reconciler process; the external mock keeps its own ledger.
use asc_foundation_types::ResourceId;
use asc_pap::PapRepository;
use asc_pcp::{AttemptSchedule, BindingReconciler, Clock, RetryPolicy, TargetDeploymentClient};
use asc_policy_repository::*;
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_types::{
    binding::{BindingStatus, PreparedBinding},
    scope::{PreparedScope, ScopeStatus},
    target::*,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::{fs::PermissionsExt, process::ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

fn fixture() -> PreparedBinding {
    serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap()
}
fn gate(name: &str) {
    if std::env::var("ASC_POLICY_CRASH_STAGE").as_deref() == Ok(name) {
        println!("GATE:{name}");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }
}
struct FaultBoundary(Arc<SqlitePolicyRepository>);
impl BindingStateRepository for FaultBoundary {
    fn check_writable(&self) -> Result<(), StoreError> {
        self.0.check_writable()
    }
    fn get_binding_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError> {
        self.0.get_binding_state(id)
    }
    fn compare_exchange_binding_state(
        &self,
        expected: &BindingStateSnapshot,
        write: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError> {
        let result = self.0.compare_exchange_binding_state(expected, write)?;
        if result != WriteResult::Conflict {
            match &write.next {
                None => gate("delete_result_saved"),
                Some(patch) => {
                    if let Some(status) = &patch.status {
                        match status.phase {
                            BindingStatus::Applying => gate("claim_saved"),
                            BindingStatus::Ready => gate("result_saved"),
                            BindingStatus::PendingApply => gate("recovery_saved"),
                            _ => {}
                        }
                    } else if let Some(deployments) = &patch.deployments {
                        if deployments
                            .iter()
                            .any(|deployment| deployment.presence == Presence::Present)
                        {
                            gate("compound_observation_saved");
                        }
                        gate(
                            if expected.binding.status.phase == BindingStatus::Deleting {
                                "delete_unknown_saved"
                            } else {
                                "unknown_saved"
                            },
                        );
                    }
                }
            }
        }
        Ok(result)
    }
}
struct Tick(AtomicU64);
impl Clock for Tick {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Remote {
    address: String,
    repository: Arc<SqlitePolicyRepository>,
}
impl Remote {
    fn send(&self, operation: &str, target: &TargetRef) -> Observation {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(
            stream,
            "{}",
            serde_json::json!({"operation":operation,"target":target.id})
        )
        .unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        assert_eq!(response.trim(), "committed");
        if operation == "apply"
            && std::env::var("ASC_POLICY_CRASH_STAGE")
                .is_ok_and(|stage| stage.starts_with("compound_"))
        {
            // Accept deletion while this Apply still owns its earlier status snapshot.
            let scope_id = fixture().scope.scope_id;
            self.repository.begin_scope_delete(&scope_id).unwrap();
            gate("compound_delete_intent");
            self.repository.finish_scope_discovery(&scope_id).unwrap();
            gate("compound_pending_delete");
        }
        gate(if operation == "apply" {
            "applied_before_result"
        } else {
            "removed_before_result"
        });
        Observation {
            target: target.clone(),
            presence: if operation == "apply" {
                Presence::Present
            } else {
                Presence::Absent
            },
        }
    }
}
impl TargetDeploymentClient for Remote {
    fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        let id = String::from_utf8(plan.content.clone()).unwrap();
        Ok(PreparedApply {
            target: TargetRef {
                route: "mock".into(),
                id,
                cleanup: b"stable-cleanup-v1".to_vec(),
            },
            format: "mock.v1".into(),
            content: vec![],
        })
    }
    fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        DeploymentReport {
            observations: vec![self.send("apply", &prepared.target)],
            error: None,
        }
    }
    fn update(&self, previous: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        assert!(previous.is_empty());
        self.create(prepared)
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        DeploymentReport {
            observations: targets.iter().map(|t| self.send("delete", t)).collect(),
            error: None,
        }
    }
}
fn converge(repo: &Arc<SqlitePolicyRepository>, remote: &str) {
    let clock = Arc::new(Tick(AtomicU64::new(0)));
    let client: Arc<dyn TargetDeploymentClient> = Arc::new(Remote {
        address: remote.into(),
        repository: repo.clone(),
    });
    let core = BindingReconciler::new(
        Arc::new(FaultBoundary(repo.clone())),
        Arc::new(|binding: &PreparedBinding| {
            Ok(TranslationOutcome::Translated(TargetBindingPlan {
                format: "mock.v1".into(),
                content: binding.binding_id.to_string().into_bytes(),
            }))
        }),
        BTreeMap::from([(
            "mock".into(),
            Arc::new(move || Ok(client.clone())) as Arc<dyn asc_pcp::TargetDeploymentClientFactory>,
        )]),
        "mock".into(),
        clock.clone(),
        RetryPolicy {
            max_attempts: 3,
            base_delay_ms: 1,
            max_delay_ms: 10,
        },
    )
    .unwrap();
    let mut schedules = BTreeMap::<ResourceId, AttemptSchedule>::new();
    for tick in 0..10 {
        clock.0.store(tick * 1000, Ordering::SeqCst);
        let candidates = repo.scan_reconciliation(None, 100).unwrap();
        let pending: Vec<_> = candidates
            .into_iter()
            .filter(|c| {
                c.status.is_reconciling()
                    || matches!(
                        c.status,
                        BindingStatus::PendingApply | BindingStatus::PendingDelete
                    )
            })
            .collect();
        if pending.is_empty() {
            return;
        }
        for candidate in pending {
            core.reconcile(
                &candidate.id,
                schedules.entry(candidate.id.clone()).or_default(),
            )
            .unwrap();
        }
    }
    panic!("recovery did not converge");
}

#[test]
fn crash_child() {
    let Ok(root) = std::env::var("ASC_POLICY_CRASH_DIR") else {
        return;
    };
    let repo =
        Arc::new(SqlitePolicyRepository::open(&Path::new(&root).join("policy-state.db")).unwrap());
    let input = fixture();
    let scope_id = &input.scope.scope_id;
    let remote = std::env::var("ASC_POLICY_CRASH_REMOTE").unwrap();
    if std::env::var("ASC_POLICY_CRASH_INIT").as_deref() == Ok("1") {
        repo.put_policy(&input.policy).unwrap();
        repo.put_scope(&PreparedScope {
            scope_id: scope_id.clone(),
            selector: input.scope.selector.clone(),
            policy_snapshots: vec![input.policy],
            status: ScopeStatus::Active,
        })
        .unwrap();
        gate("scope_saved");
    }
    if let Ok(scope) = repo.get_scope(scope_id) {
        if scope.status == ScopeStatus::Deleting {
            repo.finish_scope_discovery(scope_id).unwrap();
        } else {
            repo.sync_scope_instances(scope_id, &[input.scope.process])
                .unwrap();
            gate("binding_saved");
        }
    }
    converge(&repo, &remote);
    if std::env::var("ASC_POLICY_CRASH_DELETE").as_deref() == Ok("1") {
        repo.begin_scope_delete(scope_id).unwrap();
        gate("delete_intent");
        repo.finish_scope_discovery(scope_id).unwrap();
        gate("stop_saved");
        converge(&repo, &remote);
    }
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn child(root: &Path, remote: &str, stage: &str, initialize: bool, delete: bool) {
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash_child", "--nocapture"])
            .env("ASC_POLICY_CRASH_DIR", root)
            .env("ASC_POLICY_CRASH_REMOTE", remote)
            .env("ASC_POLICY_CRASH_STAGE", stage)
            .env("ASC_POLICY_CRASH_INIT", if initialize { "1" } else { "0" })
            .env("ASC_POLICY_CRASH_DELETE", if delete { "1" } else { "0" })
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line.contains("GATE:") {
                tx.send(line).unwrap();
            }
        }
    });
    if stage == "none" {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "child recovery timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    } else {
        let reached = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("child never reached the requested crash window");
        assert!(reached.contains(&format!("GATE:{stage}")));
        child.0.kill().unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
    }
    reader.join().unwrap();
}
struct Mock {
    address: String,
    stop: Arc<AtomicBool>,
    calls: Arc<Mutex<Vec<String>>>,
    ledger: PathBuf,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Mock {
    fn start(root: &Path) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let ledger = root.join("remote-ledger.json");
        std::fs::write(&ledger, b"[]").unwrap();
        let (flag, trace, path) = (stop.clone(), calls.clone(), ledger.clone());
        let thread = std::thread::spawn(move || {
            let mut present = BTreeSet::<String>::new();
            while !flag.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    other => panic!("mock accept failed {other:?}"),
                };
                let mut line = String::new();
                BufReader::new(&mut stream).read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let target = request["target"].as_str().unwrap().to_owned();
                let operation = request["operation"].as_str().unwrap();
                if operation == "apply" {
                    present.insert(target);
                } else {
                    present.remove(&target);
                }
                let mut file = std::fs::File::create(&path).unwrap();
                file.write_all(&serde_json::to_vec(&present).unwrap())
                    .unwrap();
                file.sync_all().unwrap();
                trace.lock().unwrap().push(operation.into());
                writeln!(stream, "committed").unwrap();
            }
        });
        Self {
            address,
            stop,
            calls,
            ledger,
            thread: Some(thread),
        }
    }
    fn present(&self) -> BTreeSet<String> {
        serde_json::from_slice(&std::fs::read(&self.ledger).unwrap()).unwrap()
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
#[test]
fn sigkill_at_confirmed_windows_recovers_and_cleans_external_ledger() {
    for stage in [
        "scope_saved",
        "binding_saved",
        "claim_saved",
        "unknown_saved",
        "applied_before_result",
        "result_saved",
        "delete_intent",
        "stop_saved",
        "delete_unknown_saved",
        "removed_before_result",
        "delete_result_saved",
        "recovery_saved",
    ] {
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let mock = Mock::start(root.path());
        let deleting = matches!(
            stage,
            "delete_intent"
                | "stop_saved"
                | "delete_unknown_saved"
                | "removed_before_result"
                | "delete_result_saved"
        );
        if stage == "recovery_saved" {
            child(root.path(), &mock.address, "claim_saved", true, false);
            child(root.path(), &mock.address, stage, false, false);
        } else {
            child(root.path(), &mock.address, stage, true, deleting);
        }
        let before = SqlitePolicyRepository::open(&root.path().join("policy-state.db")).unwrap();
        let records = before.list_bindings(100, 0).unwrap();
        if stage == "applied_before_result" {
            assert_eq!(mock.present().len(), 1);
            let saved = before
                .get_binding_state(&records.items[0].spec.binding_id)
                .unwrap()
                .unwrap();
            assert_eq!(saved.binding.status.phase, BindingStatus::Applying);
            assert_eq!(saved.deployments[0].presence, Presence::Unknown);
        }
        drop(before);
        let calls = mock.calls.lock().unwrap().len();
        child(root.path(), &mock.address, "none", false, false);
        if !deleting {
            let restored =
                SqlitePolicyRepository::open(&root.path().join("policy-state.db")).unwrap();
            let recovered = restored.list_bindings(100, 0).unwrap();
            assert_eq!(recovered.total, 1, "{stage}");
            assert_eq!(
                recovered.items[0].status.phase,
                BindingStatus::Ready,
                "{stage}"
            );
            assert_eq!(recovered.items[0].spec.policy, fixture().policy, "{stage}");
            if let Some(original) = records.items.first() {
                assert_eq!(
                    recovered.items[0].spec.binding_id, original.spec.binding_id,
                    "{stage}"
                );
            }
            assert_eq!(
                restored
                    .get_scope(&fixture().scope.scope_id)
                    .unwrap()
                    .status,
                ScopeStatus::Active
            );
            assert_eq!(mock.present().len(), 1, "{stage}");
        }
        if stage == "result_saved" {
            assert_eq!(
                mock.calls.lock().unwrap().len(),
                calls,
                "Ready must not reapply after restart"
            );
        }
        child(root.path(), &mock.address, "none", false, true);
        let after = SqlitePolicyRepository::open(&root.path().join("policy-state.db")).unwrap();
        assert_eq!(after.list_bindings(100, 0).unwrap().total, 0, "{stage}");
        assert_eq!(after.list_scopes(100, 0).unwrap().total, 0, "{stage}");
        assert!(
            mock.present().is_empty(),
            "orphaned remote target at {stage}"
        );
    }
}

#[test]
fn sigkill_during_apply_and_scope_deletion_preserves_cleanup_intent() {
    for stage in [
        "compound_delete_intent",
        "compound_pending_delete",
        "compound_observation_saved",
    ] {
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let mock = Mock::start(root.path());
        child(root.path(), &mock.address, stage, true, false);
        let before = SqlitePolicyRepository::open(&root.path().join("policy-state.db")).unwrap();
        assert_eq!(
            before.get_scope(&fixture().scope.scope_id).unwrap().status,
            ScopeStatus::Deleting
        );
        let bindings = before.list_bindings(100, 0).unwrap();
        assert_eq!(bindings.total, 1, "{stage}");
        let id = &bindings.items[0].spec.binding_id;
        let saved = before.get_binding_state(id).unwrap().unwrap();
        assert_eq!(
            saved.binding.status.phase,
            if stage == "compound_delete_intent" {
                BindingStatus::Applying
            } else {
                BindingStatus::PendingDelete
            },
            "an old Apply overwrote deletion at {stage}"
        );
        assert_eq!(saved.deployments.len(), 1, "{stage}");
        assert_eq!(
            saved.deployments[0].presence,
            if stage == "compound_observation_saved" {
                Presence::Present
            } else {
                Presence::Unknown
            },
            "{stage}"
        );
        assert_eq!(mock.present(), BTreeSet::from([id.to_string()]), "{stage}");
        drop(before);
        child(root.path(), &mock.address, "none", false, false);
        let after = SqlitePolicyRepository::open(&root.path().join("policy-state.db")).unwrap();
        assert_eq!(after.list_bindings(100, 0).unwrap().total, 0, "{stage}");
        assert_eq!(after.list_scopes(100, 0).unwrap().total, 0, "{stage}");
        assert!(mock.present().is_empty(), "orphaned target at {stage}");
        assert_eq!(*mock.calls.lock().unwrap(), ["apply", "delete"], "{stage}");
    }
}
