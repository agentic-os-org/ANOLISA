//! Deterministic handoff deadlines and preparation races without maintenance timing guesses.

use super::*;
use crate::{Capabilities, Operation, RegistrationOwner};
use std::{collections::BTreeMap, fs, path::Path};

struct Fixture {
    runtime: Arc<Runtime>,
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut document = aw_config::Validator::new()
            .unwrap()
            .parse(include_bytes!(
                "../../../../aw-config/examples/aw.minimal.yaml"
            ))
            .unwrap()
            .as_value()
            .clone();
        document["spec"]["agents"] = json!({"target":{"adapter":"qoder-sdk","argv":["/bin/true"]}});
        let python = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("python3"))
            .find(|path| path.is_file())
            .unwrap()
            .canonicalize()
            .unwrap();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/policy.py");
        document["spec"]["providers"]["policy"] = json!({
            "protocol":aw_provider::VERSION,"config":{},"timeout_ms":5000,"max_output_bytes":65536,
            "transport":{"type":"stdio","location":"agent","argv":[python,"-I",script,directory.path()]}
        });
        document["spec"]["events"]["tool.before"]["steps"] = json!([
            {"id":"observe","provider":"policy","operation":"observe","effects":["observe"],"on_error":"report"}
        ]);
        let runtime = Runtime::new(
            serde_json::to_vec(&document).unwrap(),
            &directory.path().join("audit"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        Self { runtime, directory }
    }
    fn key(&self, request: &str) -> RegistrationKey {
        RegistrationKey {
            request_id: request.into(),
            owner: RegistrationOwner {
                pid: std::process::id(),
                start_ticks: Process::read(std::process::id()).unwrap().metadata()["start_ticks"]
                    .as_str()
                    .unwrap()
                    .into(),
            },
        }
    }
    fn command(&self, request: &str) -> RegistrationCommand {
        RegistrationCommand::Register {
            key: self.key(request),
            target: "target".into(),
            cwd: self.directory.path().to_str().unwrap().into(),
            environment: BTreeMap::new(),
            capabilities: Capabilities {
                adapter: "qoder-sdk".into(),
                version: "1.0.56".into(),
                entrypoint: "sdk-worker".into(),
                events: BTreeMap::from([
                    ("tool.before".into(), vec!["observe".into()]),
                    ("tool.after".into(), vec!["observe".into()]),
                ]),
                failure_actions: BTreeMap::new(),
            },
        }
    }
    fn call(&self, command: RegistrationCommand) -> Result<Value> {
        self.runtime.registration(
            command,
            &Process::read(std::process::id()).unwrap(),
            Instant::now() + Duration::from_secs(10),
            &AtomicBool::new(false),
        )
    }
    fn open(&self, id: &str) -> Result<Value> {
        self.runtime.handle(
            Operation::OpenScope {
                parent_id: id.into(),
                pid: std::process::id(),
                cwd: self.directory.path().to_str().unwrap().into(),
                environment: BTreeMap::new(),
            },
            Instant::now() + Duration::from_secs(10),
            &AtomicBool::new(false),
        )
    }
    fn query(&self, request: &str) -> Value {
        self.call(RegistrationCommand::Query {
            key: self.key(request),
        })
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.runtime.maintain(true).unwrap();
    }
}
fn handle(value: &Value) -> RegistrationHandle {
    serde_json::from_value(value["handle"].clone()).unwrap()
}
fn code(result: Result<Value>, expected: &str) {
    assert!(
        matches!(result, Err(Error::Rejected(ref code)) if code == expected),
        "{result:?}"
    );
}

#[test]
fn pending_ready_gates_tasks_and_expiration_cannot_be_renewed_by_duplicate() {
    let f = Fixture::new();
    let first = f.call(f.command("one")).unwrap();
    let h = handle(&first);
    assert_eq!(first["state"], "pending_ready");
    code(f.open(&h.binding.instance_id), "registration_not_ready");
    let duplicate = f.call(f.command("one")).unwrap();
    assert_eq!(duplicate["handle"], first["handle"]);
    assert_eq!(f.runtime.instances.lock().unwrap().len(), 1);
    f.runtime
        .owners
        .lock()
        .unwrap()
        .get_mut(&h.binding.instance_id)
        .unwrap()
        .acknowledge_by = Some(Instant::now());
    code(
        f.call(RegistrationCommand::Ready {
            handle: h,
            policy_revision: first["policy_revision"].as_str().unwrap().into(),
        }),
        "registration_confirmation_expired",
    );
    f.runtime.maintain_scopes().unwrap();
    assert_eq!(f.query("one")["state"], "expired");
    assert_eq!(f.call(f.command("one")).unwrap()["state"], "expired");
    assert!(f.runtime.instances.lock().unwrap().is_empty());
    assert!(f.runtime.owners.lock().unwrap().is_empty());
}

#[test]
fn ready_and_release_are_idempotent_and_handles_cannot_cross_instances() {
    let f = Fixture::new();
    let first = f.call(f.command("one")).unwrap();
    let second = f.call(f.command("two")).unwrap();
    let h = handle(&first);
    let mut wrong = h.clone();
    wrong.binding = handle(&second).binding;
    code(
        f.call(RegistrationCommand::Release { handle: wrong }),
        "registration_handle_mismatch",
    );
    let mut old = h.clone();
    old.binding.identity.generation.push('x');
    code(
        f.call(RegistrationCommand::Release { handle: old }),
        "stale_service",
    );
    code(
        f.call(RegistrationCommand::Ready {
            handle: h.clone(),
            policy_revision: "wrong".into(),
        }),
        "registration_policy_mismatch",
    );
    for _ in 0..2 {
        assert_eq!(
            f.call(RegistrationCommand::Ready {
                handle: h.clone(),
                policy_revision: first["policy_revision"].as_str().unwrap().into()
            })
            .unwrap()["state"],
            "ready"
        );
    }
    let task = f.open(&h.binding.instance_id).unwrap();
    let id = task["instance_id"].as_str().unwrap();
    f.runtime
        .handle(
            Operation::AcknowledgeScope {
                instance_id: id.into(),
            },
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        )
        .unwrap();
    f.runtime.check_scope_owner(id).unwrap();
    code(
        f.runtime
            .check_scope_owner(&h.binding.instance_id)
            .map(|()| json!({})),
        "registration_requires_scope",
    );
    for _ in 0..2 {
        assert_eq!(
            f.call(RegistrationCommand::Release { handle: h.clone() })
                .unwrap()["state"],
            "released"
        );
    }
    assert_eq!(f.runtime.instances.lock().unwrap().len(), 1);
    assert_eq!(f.query("two")["state"], "pending_ready");
    f.call(RegistrationCommand::Release {
        handle: handle(&second),
    })
    .unwrap();
    assert!(f.runtime.owners.lock().unwrap().is_empty());
}

#[test]
fn request_identity_rejects_changed_context_and_process_start() {
    let f = Fixture::new();
    let first = f.call(f.command("one")).unwrap();
    let mut different = f.command("one");
    if let RegistrationCommand::Register { environment, .. } = &mut different {
        environment.insert("CONTEXT".into(), "different".into());
    }
    code(f.call(different), "registration_request_conflict");
    let mut key = f.key("one");
    key.owner.start_ticks.push('0');
    code(
        f.call(RegistrationCommand::Query { key }),
        "registration_owner_mismatch",
    );
    f.call(RegistrationCommand::Release {
        handle: handle(&first),
    })
    .unwrap();
}

#[test]
fn preparation_is_queryable_and_cancellation_drains_before_failure_is_reported() {
    let f = Fixture::new();
    fs::write(f.directory.path().join("hold"), "").unwrap();
    let cancel = AtomicBool::new(false);
    thread::scope(|scope| {
        let preparation = scope.spawn(|| {
            f.runtime.registration(
                f.command("slow"),
                &Process::read(std::process::id()).unwrap(),
                Instant::now() + Duration::from_secs(10),
                &cancel,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f.directory.path().join("preparing").exists() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(f.query("slow")["state"], "preparing");
        assert_eq!(f.call(f.command("slow")).unwrap()["state"], "preparing");
        cancel.store(true, Ordering::Release);
        assert!(preparation.join().unwrap().is_err());
    });
    let result = f.query("slow");
    assert_eq!(result["state"], "failed");
    assert!(result["audit_key"].is_string(), "{result}");
    assert_eq!(
        f.runtime
            .audit
            .read(result["audit_key"].as_str().unwrap())
            .unwrap()["terminal"],
        true
    );
    assert!(f.runtime.instances.lock().unwrap().is_empty());
    assert!(f.runtime.owners.lock().unwrap().is_empty());
    assert_eq!(f.call(f.command("slow")).unwrap()["state"], "failed");
}

#[test]
fn ready_audit_failure_keeps_the_instance_closed_to_tasks() {
    let f = Fixture::new();
    let prepared = f.call(f.command("audit")).unwrap();
    let h = handle(&prepared);
    fs::rename(
        f.directory.path().join("audit"),
        f.directory.path().join("audit-original"),
    )
    .unwrap();
    fs::write(f.directory.path().join("audit"), "not a directory").unwrap();
    code(
        f.call(RegistrationCommand::Ready {
            handle: h.clone(),
            policy_revision: prepared["policy_revision"].as_str().unwrap().into(),
        }),
        "audit_unavailable",
    );
    assert!(!f.runtime.audit.healthy());
    assert!(f.open(&h.binding.instance_id).is_err());
}
