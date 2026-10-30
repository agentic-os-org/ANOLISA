//! Resident updates retain ownership through preparation, native handoff and cleanup.

use super::*;
use crate::{Binding, Capabilities};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Fixture {
    runtime: Arc<Runtime>,
    directory: tempfile::TempDir,
    document: Value,
    root: Binding,
}

impl Fixture {
    fn new(adapter: &str) -> Self {
        Self::with_provider(adapter, false)
    }

    fn with_provider(adapter: &str, provider: bool) -> Self {
        Self::configured(adapter, provider, 64)
    }

    fn configured(adapter: &str, provider: bool, bindings: usize) -> Self {
        let directory = tempfile::Builder::new()
            .prefix("resident-policy-")
            .tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))
            .unwrap();
        let mut document = aw_config::Validator::new()
            .unwrap()
            .parse(include_bytes!(
                "../../../../aw-config/examples/aw.minimal.yaml"
            ))
            .unwrap()
            .as_value()
            .clone();
        document["spec"]["daemon"]["limits"] = json!({"bindings":bindings});
        document["spec"]["agents"] = json!({"target":{"adapter":adapter,"argv":["/bin/true"]}});
        if provider {
            let python = std::env::split_paths(&std::env::var_os("PATH").unwrap())
                .map(|path| path.join("python3"))
                .find(|path| path.is_file())
                .unwrap()
                .canonicalize()
                .unwrap();
            let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/policy.py");
            document["spec"]["providers"]["policy"] = json!({
                "protocol":aw_provider::VERSION, "config":{}, "timeout_ms":5000,
                "max_output_bytes":65536,
                "transport":{"type":"stdio","location":"agent","argv":[python,"-I",script,directory.path()]}
            });
            document["spec"]["events"]["tool.before"]["steps"] = json!([
                {"id":"observe", "provider":"policy", "operation":"observe", "effects":["observe"],"on_error":"report"}
            ]);
        }
        let runtime = Runtime::new(
            serde_json::to_vec(&document).unwrap(),
            &directory.path().join("audit"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let (version, entrypoint) = match adapter {
            "qoder-sdk" => ("1.0.56", "sdk-worker"),
            "qwenpaw" => ("2.2.2.post1", "app"),
            _ => unreachable!(),
        };
        let root = serde_json::from_value(
            runtime
                .handle(
                    Operation::Bind {
                        target: "target".into(),
                        capabilities: Capabilities {
                            adapter: adapter.into(),
                            version: version.into(),
                            entrypoint: entrypoint.into(),
                            events: BTreeMap::from([
                                ("tool.before".into(), vec!["observe".into()]),
                                ("tool.after".into(), vec!["observe".into()]),
                            ]),
                            failure_actions: Default::default(),
                        },
                        cwd: directory.path().to_str().unwrap().into(),
                        environment: BTreeMap::new(),
                    },
                    Instant::now() + TIMEOUT,
                    &AtomicBool::new(false),
                )
                .unwrap(),
        )
        .unwrap();
        let fixture = Self {
            runtime,
            directory,
            document,
            root,
        };
        fixture
            .call(Operation::OwnInstance {
                instance_id: fixture.id(),
                pid: std::process::id(),
            })
            .unwrap();
        fixture
            .call(Operation::EnableReload {
                instance_id: fixture.id(),
            })
            .unwrap();
        fixture
            .call(Operation::AssociateProcess {
                instance_id: fixture.id(),
                pid: std::process::id(),
            })
            .unwrap();
        fixture.lifecycle(LifecyclePhase::Ready, "");
        fixture
    }

    fn id(&self) -> String {
        self.root.instance_id.clone()
    }

    fn call(&self, operation: Operation) -> Result<Value> {
        self.runtime
            .handle(operation, Instant::now() + TIMEOUT, &AtomicBool::new(false))
    }

    fn lifecycle(&self, phase: LifecyclePhase, key: &str) {
        self.call(Operation::BridgeLifecycle {
            instance_id: self.id(),
            pid: std::process::id(),
            phase,
            key: key.into(),
        })
        .unwrap();
    }

    fn open(&self) -> Result<Value> {
        self.call(Operation::OpenScope {
            parent_id: self.id(),
            pid: std::process::id(),
            cwd: self.directory.path().to_str().unwrap().into(),
            environment: BTreeMap::new(),
        })
    }

    fn revision(&self) -> String {
        self.runtime.policy_revision(&self.id()).unwrap().unwrap()
    }

    fn update(&self, expected: &str, document: &Value) -> Result<Value> {
        let request = Request {
            api_version: crate::VERSION.into(),
            identity: Some(self.root.identity.clone()),
            deadline_ns: crate::deadline::encode(Instant::now() + TIMEOUT).unwrap(),
            operation: Operation::UpdatePolicy {
                instance_id: self.id(),
                expected_revision: expected.into(),
                config: serde_json::to_string(document).unwrap(),
            },
        };
        self.runtime.update_policy(
            &request,
            (0, std::process::id() as i32),
            &Arc::new(AtomicBool::new(false)),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.runtime.maintain(true).unwrap();
    }
}

fn rejects(result: Result<Value>, expected: &str) {
    assert!(
        matches!(result, Err(Error::Rejected(ref code) | Error::Attempt { ref code, .. }) if code == expected),
        "{result:?}"
    );
}

#[test]
fn resident_claim_covers_preparation_unacknowledged_scope_and_final_cleanup() {
    for adapter in ["qoder-sdk", "qwenpaw"] {
        let f = Fixture::new(adapter);
        let original = f.revision();
        let mut candidate = f.document.clone();
        candidate["spec"]["execution"]["default_event_budget_ms"] = json!(4321);
        rejects(f.open(), "request_not_admitted");
        f.lifecycle(LifecyclePhase::RequestStart, "first");
        rejects(f.update(&original, &candidate), "busy");
        let task: Binding = serde_json::from_value(f.open().unwrap()).unwrap();
        let status = f.runtime.policy_status().unwrap();
        let instances = status.as_array().unwrap();
        assert_eq!(
            instances
                .iter()
                .find(|item| item["instance_id"] == task.instance_id)
                .unwrap()["parent_instance_id"],
            f.id()
        );
        assert!(instances
            .iter()
            .find(|item| item["instance_id"] == f.id())
            .unwrap()["parent_instance_id"]
            .is_null());
        rejects(
            f.call(Operation::AcknowledgeScope {
                instance_id: task.instance_id.clone(),
            }),
            "scope_policy_unconfirmed",
        );
        // Even an early caller release cannot conceal a preparing/unacknowledged child.
        f.lifecycle(LifecyclePhase::RequestEnd, "first");
        rejects(f.update(&original, &candidate), "busy");
        f.lifecycle(LifecyclePhase::RequestStart, "cleanup");
        let snapshot = f
            .call(Operation::ScopePolicy {
                instance_id: task.instance_id.clone(),
            })
            .unwrap();
        assert_eq!(snapshot["policy_revision"], original);
        assert!(snapshot.get("spec").is_none());
        assert!(snapshot.get("providers").is_none());
        f.call(Operation::AcknowledgeScope {
            instance_id: task.instance_id.clone(),
        })
        .unwrap();
        assert_eq!(
            f.call(Operation::ScopePolicy {
                instance_id: task.instance_id.clone()
            })
            .unwrap(),
            snapshot
        );
        rejects(f.update(&original, &candidate), "busy");
        f.call(Operation::ReleaseInstance {
            instance_id: task.instance_id.clone(),
        })
        .unwrap();
        rejects(f.update(&original, &candidate), "busy");
        f.lifecycle(LifecyclePhase::RequestEnd, "cleanup");
        let updated = f.update(&original, &candidate).unwrap();
        assert_ne!(updated["policy_revision"], original);
        let audit = f
            .runtime
            .audit
            .read(updated["audit_key"].as_str().unwrap())
            .unwrap();
        let records = audit["records"].as_array().unwrap();
        assert_eq!(audit["terminal"], true);
        assert_eq!(records[records.len() - 2]["record"]["phase"], "activating");
        assert_eq!(records.last().unwrap()["record"]["status"], "active");
        assert_eq!(
            records
                .iter()
                .filter(|record| record["record"]["phase"] == "closed")
                .count(),
            1
        );
        rejects(
            f.call(Operation::ScopePolicy {
                instance_id: task.instance_id.clone(),
            }),
            "scope_unavailable",
        );
        rejects(
            f.call(Operation::AcknowledgeScope {
                instance_id: task.instance_id,
            }),
            "scope_unavailable",
        );
        f.lifecycle(LifecyclePhase::RequestStart, "next");
        let task: Binding = serde_json::from_value(f.open().unwrap()).unwrap();
        let snapshot = f
            .call(Operation::ScopePolicy {
                instance_id: task.instance_id.clone(),
            })
            .unwrap();
        assert_eq!(snapshot["policy_revision"], updated["policy_revision"]);
        f.call(Operation::ReleaseInstance {
            instance_id: task.instance_id,
        })
        .unwrap();
        f.lifecycle(LifecyclePhase::RequestEnd, "next");
    }
}

#[test]
fn resident_failed_preparation_and_revision_conflict_retain_active_policy() {
    let f = Fixture::new("qwenpaw");
    let original = f.revision();
    let mut candidate = f.document.clone();
    candidate["spec"]["execution"]["default_event_budget_ms"] = json!(3456);
    rejects(
        f.update(&"0".repeat(64), &candidate),
        "policy_revision_conflict",
    );
    assert_eq!(f.revision(), original);
    candidate["spec"]["agents"]["target"]["argv"] = json!(["/different"]);
    rejects(f.update(&original, &candidate), "configuration_rejected");
    assert_eq!(f.revision(), original);
    assert!(f.runtime.audit.healthy());
}

#[test]
fn resident_request_capacity_is_concurrent_and_configured() {
    let f = Fixture::configured("qoder-sdk", false, 128);
    for n in 0..f.runtime.limits.bindings {
        f.lifecycle(LifecyclePhase::RequestStart, &format!("task-{n}"));
    }
    rejects(
        f.call(Operation::BridgeLifecycle {
            instance_id: f.id(),
            pid: std::process::id(),
            phase: LifecyclePhase::RequestStart,
            key: "overflow".into(),
        }),
        "busy",
    );
    for n in 0..f.runtime.limits.bindings {
        f.lifecycle(LifecyclePhase::RequestEnd, &format!("task-{n}"));
    }
    for n in 0..1025 {
        f.lifecycle(LifecyclePhase::RequestStart, &format!("next-{n}"));
        f.lifecycle(LifecyclePhase::RequestEnd, &format!("next-{n}"));
    }
    assert_eq!(f.runtime.policy_status().unwrap()[0]["requests"], 0);
}

#[test]
fn resident_audit_failure_does_not_activate_candidate() {
    let f = Fixture::new("qwenpaw");
    let original = f.revision();
    let mut candidate = f.document.clone();
    candidate["spec"]["execution"]["default_event_budget_ms"] = json!(3456);
    fs::rename(
        f.directory.path().join("audit"),
        f.directory.path().join("audit-offline"),
    )
    .unwrap();
    assert!(f.update(&original, &candidate).is_err());
    assert_eq!(f.revision(), original);
}

fn wait_for(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "policy fixture did not reach its barrier"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn resident_reservation_wins_during_candidate_preparation() {
    let f = Fixture::with_provider("qwenpaw", true);
    let original = f.revision();
    let mut candidate = f.document.clone();
    candidate["spec"]["execution"]["default_event_budget_ms"] = json!(3456);
    fs::write(f.directory.path().join("hold"), "hold").unwrap();
    let result = std::thread::scope(|threads| {
        let update = threads.spawn(|| f.update(&original, &candidate));
        wait_for(|| f.directory.path().join("preparing").exists());
        f.lifecycle(LifecyclePhase::RequestStart, "task");
        fs::write(f.directory.path().join("release"), "release").unwrap();
        update.join().unwrap()
    });
    rejects(result, "busy");
    assert_eq!(f.revision(), original);
    f.lifecycle(LifecyclePhase::RequestEnd, "task");
    f.update(&original, &candidate).unwrap();
}

#[test]
fn pending_scope_blocks_activation_after_early_root_claim_release() {
    let f = Fixture::with_provider("qwenpaw", true);
    let original = f.revision();
    f.lifecycle(LifecyclePhase::RequestStart, "task");
    fs::write(f.directory.path().join("hold"), "hold").unwrap();
    let task: Binding = serde_json::from_value(std::thread::scope(|threads| {
        let task = threads.spawn(|| f.open());
        wait_for(|| f.directory.path().join("preparing").exists());
        f.lifecycle(LifecyclePhase::RequestEnd, "task");
        rejects(f.update(&original, &f.document), "busy");
        fs::write(f.directory.path().join("release"), "release").unwrap();
        task.join().unwrap().unwrap()
    }))
    .unwrap();
    f.call(Operation::ReleaseInstance {
        instance_id: task.instance_id,
    })
    .unwrap();
    f.update(&original, &f.document).unwrap();
}

#[test]
fn owner_exit_during_activation_audit_preserves_previous_policy() {
    let f = Fixture::new("qwenpaw");
    let original = f.revision();
    let mut candidate = f.document.clone();
    candidate["spec"]["execution"]["default_event_budget_ms"] = json!(3456);
    let mut owner = OwnerChild(
        std::process::Command::new("sleep")
            .arg("15")
            .spawn()
            .unwrap(),
    );
    let child = &mut owner.0;
    {
        let mut instances = f.runtime.instances.lock().unwrap();
        instances
            .get_mut(&f.id())
            .unwrap()
            .as_mut()
            .unwrap()
            .reload
            .as_mut()
            .unwrap()
            .process = Some(Process::read(child.id()).unwrap());
    }
    let guard = f.runtime.audit.activation_gate.lock().unwrap();
    let result = std::thread::scope(|threads| {
        let update = threads.spawn(|| f.update(&original, &candidate));
        wait_for(|| f.runtime.audit.activation_appends.load(Ordering::Acquire) == 1);
        child.kill().unwrap();
        child.wait().unwrap();
        drop(guard);
        update.join().unwrap()
    });
    let key = match &result {
        Err(Error::Attempt { audit_key, .. }) => audit_key,
        other => panic!("expected audited rejection: {other:?}"),
    };
    let audit = f.runtime.audit.read(key).unwrap();
    let records = audit["records"].as_array().unwrap();
    assert_eq!(audit["terminal"], true);
    assert_eq!(records[records.len() - 2]["record"]["phase"], "activating");
    assert_eq!(records.last().unwrap()["record"]["status"], "rejected");
    assert_eq!(
        records
            .iter()
            .filter(|record| record["record"]["phase"] == "closed")
            .count(),
        1
    );
    assert!(!records
        .iter()
        .any(|record| record["record"]["status"] == "active"));
    rejects(result, "stale_process");
    assert_eq!(f.revision(), original);
    assert!(!Path::new("/proc").join(child.id().to_string()).exists());
}

#[test]
fn terminal_audit_failure_fences_committed_policy_without_false_rejection() {
    for failure in [1, 2] {
        let f = Fixture::new("qwenpaw");
        let original = f.revision();
        let mut candidate = f.document.clone();
        candidate["spec"]["execution"]["default_event_budget_ms"] = json!(3456);
        f.runtime
            .audit
            .activation_failure
            .store(failure, Ordering::Release);
        let result = f.update(&original, &candidate);
        let key = match &result {
            Err(Error::Attempt { audit_key, .. }) => audit_key,
            other => panic!("expected indeterminate activation: {other:?}"),
        };
        let audit = f.runtime.audit.read(key).unwrap();
        let records = audit["records"].as_array().unwrap();
        assert!(!records
            .iter()
            .any(|record| record["record"]["status"] == "rejected"));
        assert_eq!(audit["terminal"], failure == 2);
        assert_eq!(
            records.last().unwrap()["record"]["phase"],
            if failure == 1 { "activating" } else { "closed" }
        );
        rejects(result, "policy_activation_indeterminate");
        assert_ne!(f.revision(), original);
        assert!(f.runtime.stopped.load(Ordering::Acquire));
        assert!(!f.runtime.audit.healthy());
        rejects(f.open(), "service_stopping");
        // A caller already past dispatch's stopped check still observes the
        // closing fence under the same gate used by activation.
        rejects(
            f.runtime.lifecycle(
                &f.id(),
                std::process::id(),
                LifecyclePhase::RequestStart,
                "late-task",
            ),
            "unknown_or_draining_instance",
        );
    }
}

struct OwnerChild(std::process::Child);

impl Drop for OwnerChild {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}
