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
        Self::gateway("openclaw")
    }

    fn gateway(adapter: &str) -> Self {
        Self::gateway_policy(adapter, |_| {}, false)
    }

    fn gateway_policy(adapter: &str, configure: impl FnOnce(&mut Value), enforce: bool) -> Self {
        Self::gateway_fixture(Fixture::new(), adapter, configure, enforce)
    }

    fn gateway_fixture(
        mut fixture: Fixture,
        adapter: &str,
        configure: impl FnOnce(&mut Value),
        enforce: bool,
    ) -> Self {
        let mut document: Value = serde_json::from_slice(&fixture.runtime.bytes).unwrap();
        document["spec"]["agents"]["target"]["adapter"] = json!(adapter);
        configure(&mut document);
        fixture.runtime = Runtime::new(
            serde_json::to_vec(&document).unwrap(),
            &fixture.directory.path().join("native-audit"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut command = serde_json::to_value(fixture.command("native-execution")).unwrap();
        command["action"] = json!("register_native");
        command["capabilities"]["adapter"] = json!(adapter);
        command["capabilities"]["version"] = json!("2026.9.6");
        command["capabilities"]["entrypoint"] = json!("gateway");
        if enforce {
            command["capabilities"]["events"]["tool.before"] = json!(["observe", "block"]);
        }
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
                result_status: None,
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

#[test]
fn hermes_gateway_scopes_snapshot_policy_and_reclaim_without_registration() {
    let f = Native::gateway("hermes");
    let workspace = f.fixture.directory.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let open = || {
        f.operation(Operation::OpenScope {
            parent_id: f.id().into(),
            pid: std::process::id(),
            cwd: workspace.to_str().unwrap().into(),
            environment: BTreeMap::new(),
        })
    };
    let plan = |id: &str| {
        f.operation(Operation::HookPlan {
            instance_id: id.into(),
            event: "tool.before".into(),
        })
    };
    f.associate();
    code(open(), "bridge_not_ready");
    f.lifecycle(LifecyclePhase::Ready);
    let child = open().unwrap()["instance_id"].as_str().unwrap().to_owned();
    code(plan(&child), "scope_unacknowledged");
    assert!(matches!(f.update(), Err(Error::Attempt { code, .. }) if code == "busy"));
    f.operation(Operation::ReleaseInstance { instance_id: child })
        .unwrap();

    let updated = f.update().unwrap();
    let policy = updated["policy_revision"].as_str().unwrap();
    assert_ne!(policy, f.snapshot["policy_revision"]);
    for _ in 0..4 {
        let child = open().unwrap()["instance_id"].as_str().unwrap().to_owned();
        f.operation(Operation::AcknowledgeScope {
            instance_id: child.clone(),
        })
        .unwrap();
        let snapshot = plan(&child).unwrap();
        assert_eq!(snapshot["policy_revision"], policy);
        assert_eq!(snapshot["plan"]["budget_ms"], 4321);
        assert_eq!(f.fixture.runtime.registrations.lock().unwrap().len(), 1);
        f.operation(Operation::ReleaseInstance {
            instance_id: child.clone(),
        })
        .unwrap();
        code(plan(&child), "unknown_or_draining_instance");
    }
    f.fixture
        .call(RegistrationCommand::Release {
            handle: f.handle.clone(),
        })
        .unwrap();
    assert!(f.fixture.runtime.instances.lock().unwrap().is_empty());
}

fn decision_policy(document: &mut Value, scenario: &str) {
    document["spec"]["agents"]["target"]["workspace_id"] = json!("configured-workspace");
    let mut arbiter = document["spec"]["providers"]["policy"].clone();
    let argv = arbiter["transport"]["argv"].as_array_mut().unwrap();
    argv[2] =
        json!(Path::new(env!("CARGO_MANIFEST_DIR")).join("../aw-host/tests/fixtures/provider.py"));
    argv.extend([json!("arbiter"), json!("fixture")]);
    arbiter["config"] = json!({"scenario":scenario});
    let mut executor = arbiter.clone();
    executor["transport"]["argv"][4] = json!("executor");
    document["spec"]["providers"] = json!({"policy":arbiter,"checkpoint":executor});
    document["spec"]["events"]["tool.before"]["steps"] = json!([{
        "id":"observe","provider":"policy","operation":"evaluate",
        "mode":"enforce","decision":true,"effects":["observe","block"],"on_error":"block",
        "match":{"tools":["native:openclaw:*"]},
        "obligations":{
            "snapshot":{"provider":"checkpoint","operation":"checkpoint","order":10},
            "audit":{"provider":"checkpoint","operation":"checkpoint","order":20}
        },
    }]);
}

#[test]
fn admitted_request_supplies_provider_binding_and_audits_executor_confirmation() {
    let f = Native::gateway_policy(
        "openclaw",
        |document| decision_policy(document, "snapshot-completed"),
        true,
    );
    f.associate();
    f.lifecycle(LifecyclePhase::Ready);
    f.lifecycle(LifecyclePhase::RequestStart);
    let legacy = f.open(false, "uncorroborated-call").unwrap();
    let rejected = f.invoke(&legacy).unwrap();
    assert_eq!(
        rejected["failure_detail"],
        "authoritative_binding_unavailable"
    );
    assert_eq!(rejected["failure_action"], "block");
    assert!(!f
        .fixture
        .directory
        .path()
        .join("confirmed-snapshot")
        .exists());
    f.close(&legacy);

    let event = f.open(true, "protected-call").unwrap();
    let result = f.invoke(&event).unwrap();
    assert_eq!(result["status"], "ok");
    assert_eq!(result["effects"], json!([]));
    assert!(f
        .fixture
        .directory
        .path()
        .join("confirmed-snapshot")
        .exists());
    assert!(result.get("step_trace").is_none());

    let audit = f
        .fixture
        .runtime
        .audit
        .read(event["event_id"].as_str().unwrap())
        .unwrap();
    let record = audit["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| &entry["record"])
        .find(|entry| entry["phase"] == "completed")
        .unwrap();
    assert_eq!(record["step_trace"]["decision"], "allow");
    let action = &record["step_trace"]["actions"][0];
    assert_eq!(action["kind"], "snapshot");
    assert_eq!(action["status"], "completed");
    let journaled = audit["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| &entry["record"])
        .filter(|entry| entry["phase"] == "obligation")
        .map(|entry| {
            (
                entry["kind"].as_str().unwrap(),
                entry["status"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        journaled,
        [
            ("snapshot", "pending"),
            ("snapshot", "completed"),
            ("audit", "pending"),
            ("audit", "completed")
        ]
    );
    let requests = [&result["call"], &action["call"]].map(|call| {
        let name = format!("{}.json", call["request_id"].as_str().unwrap());
        serde_json::from_slice::<Value>(&fs::read(f.fixture.directory.path().join(name)).unwrap())
            .unwrap()
    });
    let binding = &requests[0]["request"]["binding"];
    assert_eq!(binding["workspace_id"], "configured-workspace");
    assert_eq!(binding["instance_id"], f.id());
    assert_eq!(binding["config_revision"], f.snapshot["policy_revision"]);
    assert!(binding["task_version"]
        .as_str()
        .unwrap()
        .starts_with("request-"));
    assert_ne!(binding["session_id"], "session");
    assert_eq!(requests[1]["request"]["binding"], *binding);
    assert!(!audit.to_string().contains("configured-workspace"));
    assert!(!audit.to_string().contains("snapshot-completed"));
    f.lifecycle(LifecyclePhase::RequestEnd);
}

#[test]
fn protection_remains_pending_after_service_process_exit() {
    const ROOT: &str = "AW_TEST_OBLIGATION_CRASH_DIRECTORY";
    if let Some(root) = std::env::var_os(ROOT) {
        let root = std::path::PathBuf::from(root);
        let fixture = Fixture::with_directory(tempfile::tempdir_in(&root).unwrap());
        let f = Native::gateway_fixture(
            fixture,
            "openclaw",
            |document| decision_policy(document, "snapshot-crash-window"),
            true,
        );
        f.associate();
        f.lifecycle(LifecyclePhase::Ready);
        f.lifecycle(LifecyclePhase::RequestStart);
        let event = f.open(true, "interrupted-snapshot").unwrap();
        fs::write(
            root.join("event.tmp"),
            json!({
                "event": event, "directory": f.fixture.directory.path()
            })
            .to_string(),
        )
        .unwrap();
        fs::rename(root.join("event.tmp"), root.join("event.json")).unwrap();
        let _ = f.invoke(&event);
        panic!("test parent must stop the service process before receipt");
    }

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            concat!(
                module_path!(),
                "::protection_remains_pending_after_service_process_exit"
            )
            .trim_start_matches("aw_service::"),
        ])
        .env(ROOT, root)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let interrupted = loop {
        if let Ok(bytes) = fs::read(root.join("event.json")) {
            let state: Value = serde_json::from_slice(&bytes).unwrap();
            let work = Path::new(state["directory"].as_str().unwrap());
            if work.join("confirmed-snapshot").exists() {
                break Some(state);
            }
        }
        if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            break None;
        }
        thread::sleep(Duration::from_millis(5));
    };
    let _ = child.kill();
    let status = child.wait().unwrap();
    let interrupted = interrupted.expect("executor must start before service exit");
    assert!(!status.success());
    let path = Path::new(interrupted["directory"].as_str().unwrap()).join("native-audit");
    let event_key = interrupted["event"]["event_id"].as_str().unwrap();
    let recovered = crate::audit::Audit::new(&path).unwrap();
    let audit = recovered.read(event_key).unwrap();
    assert_eq!(audit["terminal"], false);
    let records = audit["records"].as_array().unwrap();
    let pending = &records.last().unwrap()["record"];
    assert_eq!(pending["phase"], "obligation");
    assert_eq!(pending["status"], "pending");
    assert_eq!(pending["call"]["provider"], "checkpoint");
    assert_eq!(pending["kind"], "snapshot");
    assert!(pending["call"]["request_id"].as_str().is_some());
    assert!(
        recovered.claim(event_key, json!({"kind":"event"})).is_err(),
        "an interrupted event must not be replayed"
    );
}
