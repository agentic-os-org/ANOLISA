//! Native roots reserve execution independently of callback-side Hook planning.
use super::*;
use crate::{LifecyclePhase, Request};

struct Native {
    fixture: Fixture,
    snapshot: Value,
    handle: RegistrationHandle,
}
impl Native {
    fn new() -> Self {
        let mut fixture = Fixture::new();
        let mut document: Value = serde_json::from_slice(&fixture.runtime.bytes).unwrap();
        document["spec"]["agents"]["target"]["adapter"] = json!("openclaw");
        fixture.runtime = Runtime::new(
            serde_json::to_vec(&document).unwrap(),
            &fixture.directory.path().join("native-audit"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut command = serde_json::to_value(fixture.command("native-execution")).unwrap();
        command["action"] = json!("register_native");
        command["capabilities"]["adapter"] = json!("openclaw");
        command["capabilities"]["version"] = json!("2026.9.6");
        command["capabilities"]["entrypoint"] = json!("gateway");
        let snapshot = fixture
            .call(serde_json::from_value(command).unwrap())
            .unwrap();
        let handle = handle(&snapshot);
        fixture
            .runtime
            .enable_reload(&handle.binding.instance_id)
            .unwrap();
        fixture
            .call(RegistrationCommand::Ready {
                handle: handle.clone(),
                policy_revision: snapshot["policy_revision"].as_str().unwrap().into(),
            })
            .unwrap();
        Self {
            fixture,
            snapshot,
            handle,
        }
    }
    fn id(&self) -> &str {
        &self.handle.binding.instance_id
    }
    fn operation(&self, operation: Operation) -> Result<Value> {
        self.fixture.runtime.authorize_registration_peer(
            &operation,
            Some(&Process::read(std::process::id()).unwrap()),
        )?;
        self.fixture.runtime.handle(
            operation,
            Instant::now() + Duration::from_secs(10),
            &AtomicBool::new(false),
        )
    }
    fn lifecycle(&self, phase: LifecyclePhase) {
        self.lifecycle_run(phase, "request");
    }
    fn lifecycle_run(&self, phase: LifecyclePhase, run: &str) {
        self.operation(Operation::BridgeLifecycle {
            instance_id: self.id().into(),
            pid: std::process::id(),
            key: if matches!(phase, LifecyclePhase::Ready) {
                String::new()
            } else {
                format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&["session", run]).unwrap())
                )
            },
            phase,
        })
        .unwrap();
    }
    fn open(&self, native: bool, call: &str) -> Result<Value> {
        self.open_run(native, "request", call)
    }
    fn open_run(&self, native: bool, run: &str, call: &str) -> Result<Value> {
        let input = json!({"hook":"before_tool_call", "event":{"toolName":"exec",
            "params":{"command":"fixture"},"toolCallId":call,"runId":run},
            "context":{"sessionId":"session","runId":run,"toolCallId":call}});
        let event = aw_adapter::select("openclaw")
            .unwrap()
            .normalize(
                &aw_adapter::NativeContext {
                    target: "target".into(),
                    instance_id: self.id().into(),
                    cwd: self.fixture.directory.path().into(),
                },
                "tool.before",
                &input,
            )
            .unwrap();
        self.operation(if native {
            Operation::OpenHookEvent {
                instance_id: self.id().into(),
                native_input: serde_json::to_vec(&input).unwrap(),
                event,
            }
        } else {
            Operation::OpenEvent {
                instance_id: self.id().into(),
                event,
            }
        })
    }
    fn invoke(&self, event: &Value) -> Result<Value> {
        self.operation(Operation::InvokeStep {
            instance_id: self.id().into(),
            event_id: event["event_id"].as_str().unwrap().into(),
            step_id: "observe".into(),
            native_environment: None,
        })
    }
    fn close(&self, event: &Value) {
        self.operation(Operation::CloseEvent {
            instance_id: self.id().into(),
            event_id: event["event_id"].as_str().unwrap().into(),
        })
        .unwrap();
    }
    fn associate(&self) {
        self.operation(Operation::AssociateProcess {
            instance_id: self.id().into(),
            pid: std::process::id(),
        })
        .unwrap();
    }
    fn update(&self) -> Result<Value> {
        let mut candidate: Value = serde_json::from_slice(&self.fixture.runtime.bytes).unwrap();
        candidate["spec"]["execution"]["default_event_budget_ms"] = json!(4321);
        self.fixture.runtime.update_policy(
            &Request {
                api_version: crate::VERSION.into(),
                identity: Some(self.handle.binding.identity.clone()),
                deadline_ns: crate::deadline::encode(Instant::now() + Duration::from_secs(10))
                    .unwrap(),
                operation: Operation::UpdatePolicy {
                    instance_id: self.id().into(),
                    expected_revision: self.snapshot["policy_revision"].as_str().unwrap().into(),
                    config: candidate.to_string(),
                },
            },
            (0, std::process::id() as i32),
            &Arc::new(AtomicBool::new(false)),
        )
    }
}
fn wait_for(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "native execution barrier timed out"
        );
        thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn direct_native_events_require_process_bridge_and_active_request() {
    let f = Native::new();
    for native in [false, true] {
        code(f.open(native, "unassociated"), "process_not_associated");
    }
    f.associate();
    for native in [false, true] {
        code(f.open(native, "pre-ready"), "request_not_admitted");
    }
    f.lifecycle(LifecyclePhase::Ready);
    for native in [false, true] {
        code(f.open(native, "pre-request"), "request_not_admitted");
    }
    assert!(!f.fixture.directory.path().join("invoked").exists());
    f.lifecycle(LifecyclePhase::RequestStart);
    for native in [false, true] {
        let event = f
            .open(native, if native { "hook-valid" } else { "event-valid" })
            .unwrap();
        f.invoke(&event).unwrap();
        assert!(f.fixture.directory.path().join("invoked").exists());
        if !native {
            f.close(&event);
        }
    }
    fs::remove_file(f.fixture.directory.path().join("invoked")).unwrap();
    let outstanding = f.open(false, "outstanding").unwrap();
    f.lifecycle(LifecyclePhase::RequestEnd);
    for native in [false, true] {
        code(f.open(native, "post-request"), "request_not_admitted");
    }
    code(f.open(true, "hook-valid"), "request_not_admitted");
    code(f.invoke(&outstanding), "request_not_admitted");
    assert!(!f.fixture.directory.path().join("invoked").exists());
    f.close(&outstanding);
    f.fixture
        .call(RegistrationCommand::Release {
            handle: f.handle.clone(),
        })
        .unwrap();
    assert!(f.fixture.runtime.instances.lock().unwrap().is_empty());
    assert!(f.fixture.runtime.events.lock().unwrap().is_empty());
}
#[test]
fn native_invocation_rechecks_reservation_after_durable_started_audit() {
    let f = Native::new();
    f.associate();
    f.lifecycle(LifecyclePhase::Ready);
    f.lifecycle(LifecyclePhase::RequestStart);
    let event = f.open(false, "delayed").unwrap();
    let journal = f.fixture.runtime.audit.hold_journal();
    let result = thread::scope(|threads| {
        let invoke = threads.spawn(|| f.invoke(&event));
        wait_for(|| f.fixture.runtime.audit.started_appends() == 1);
        f.lifecycle(LifecyclePhase::RequestEnd);
        drop(journal);
        invoke.join().unwrap()
    });
    code(result, "request_not_admitted");
    assert!(!f.fixture.directory.path().join("invoked").exists());
    f.close(&event);
}
#[test]
fn native_release_during_activation_audit_prevents_commit() {
    let f = Native::new();
    f.associate();
    f.lifecycle(LifecyclePhase::Ready);
    let cancellation = f.fixture.runtime.owners.lock().unwrap()[f.id()]
        .cancelled
        .clone();
    let activation = f.fixture.runtime.audit.activation_gate.lock().unwrap();
    let result = thread::scope(|threads| {
        let update = threads.spawn(|| f.update());
        wait_for(|| {
            f.fixture
                .runtime
                .audit
                .activation_appends
                .load(Ordering::Acquire)
                == 1
        });
        let release = threads.spawn(|| {
            f.fixture.call(RegistrationCommand::Release {
                handle: f.handle.clone(),
            })
        });
        wait_for(|| cancellation.load(Ordering::Acquire));
        drop(activation);
        let result = update.join().unwrap();
        release.join().unwrap().unwrap();
        result
    });
    assert_rejected_activation(&f, result, "scope_closing");
    assert!(f.fixture.runtime.instances.lock().unwrap().is_empty());
}
fn assert_rejected_activation(f: &Native, result: Result<Value>, expected: &str) {
    let key = match &result {
        Err(Error::Attempt { code, audit_key }) if code == expected => audit_key,
        other => panic!("expected {expected}: {other:?}"),
    };
    let audit = f.fixture.runtime.audit.read(key).unwrap();
    let records = audit["records"].as_array().unwrap();
    assert!(records.iter().any(|r| r["record"]["phase"] == "activating"));
    assert!(!records.iter().any(|r| r["record"]["status"] == "active"));
    assert_eq!(records.last().unwrap()["record"]["status"], "rejected");
    assert_eq!(audit["terminal"], true);
}
#[test]
fn native_owner_exit_during_activation_audit_keeps_previous_policy() {
    let f = Native::new();
    f.associate();
    f.lifecycle(LifecyclePhase::Ready);
    let mut owner = std::process::Command::new("sleep")
        .arg("15")
        .spawn()
        .unwrap();
    // Keep the associated native worker alive while its independent owner exits.
    f.fixture
        .runtime
        .owners
        .lock()
        .unwrap()
        .get_mut(f.id())
        .unwrap()
        .process = Process::read(owner.id()).unwrap();
    let activation = f.fixture.runtime.audit.activation_gate.lock().unwrap();
    let result = thread::scope(|threads| {
        let update = threads.spawn(|| f.update());
        wait_for(|| {
            f.fixture
                .runtime
                .audit
                .activation_appends
                .load(Ordering::Acquire)
                == 1
        });
        owner.kill().unwrap();
        owner.wait().unwrap();
        drop(activation);
        update.join().unwrap()
    });
    assert_rejected_activation(&f, result, "stale_process");
    assert_eq!(
        f.fixture.runtime.policy_status().unwrap()[0]["policy_revision"],
        f.snapshot["policy_revision"]
    );
    Process::read(std::process::id()).unwrap().check().unwrap();
}

#[test]
fn native_event_keeps_its_exact_reservation_while_parallel_runs_continue() {
    let f = Native::new();
    f.associate();
    f.lifecycle(LifecyclePhase::Ready);
    f.lifecycle_run(LifecyclePhase::RequestStart, "first");
    f.lifecycle_run(LifecyclePhase::RequestStart, "second");
    let first = f.open_run(false, "first", "first-call").unwrap();
    let second = f.open_run(false, "second", "second-call").unwrap();
    let first_hook = f.open_run(true, "first", "first-hook").unwrap();
    f.lifecycle_run(LifecyclePhase::RequestEnd, "first");
    code(f.invoke(&first), "request_not_admitted");
    code(f.invoke(&first_hook), "request_not_admitted");
    assert!(!f.fixture.directory.path().join("invoked").exists());
    // An unrelated reservation remains usable after the first one retires.
    f.invoke(&second).unwrap();
    assert!(f.fixture.directory.path().join("invoked").exists());
    fs::remove_file(f.fixture.directory.path().join("invoked")).unwrap();
    // Reusing a key cannot revive the earlier event's generation.
    f.lifecycle_run(LifecyclePhase::RequestStart, "first");
    code(f.invoke(&first), "request_not_admitted");
    code(
        f.open_run(true, "first", "first-hook")
            .and_then(|event| f.invoke(&event)),
        "request_not_admitted",
    );
    assert!(!f.fixture.directory.path().join("invoked").exists());
    let fresh = f.open_run(false, "first", "fresh-call").unwrap();
    f.invoke(&fresh).unwrap();
    assert!(f.fixture.directory.path().join("invoked").exists());
    for event in [&first, &second, &fresh] {
        f.close(event);
    }
    f.lifecycle_run(LifecyclePhase::RequestEnd, "first");
    f.lifecycle_run(LifecyclePhase::RequestEnd, "second");
    f.fixture
        .call(RegistrationCommand::Release {
            handle: f.handle.clone(),
        })
        .unwrap();
    assert!(f.fixture.runtime.events.lock().unwrap().is_empty());
}
