//! Exercise authenticated SDK bindings and deferred post-tool evaluation.

use super::*;
use std::os::unix::fs::DirBuilderExt;

fn with_provider(after_budget: Option<u64>) -> Fixture {
    with_provider_config(after_budget, |_| ())
}

fn with_provider_config(after_budget: Option<u64>, configure: impl FnOnce(&mut Value)) -> Fixture {
    let mut f = Fixture::new();
    let mut document: Value = serde_json::from_slice(&f.runtime.bytes).unwrap();
    let provider = &mut document["spec"]["providers"]["policy"];
    let argv = provider["transport"]["argv"].as_array_mut().unwrap();
    argv[2] =
        json!(Path::new(env!("CARGO_MANIFEST_DIR")).join("../aw-host/tests/fixtures/provider.py"));
    argv.extend([json!("binding"), json!("fixture")]);
    provider["config"] = json!({"scenario":"replace"});
    document["spec"]["agents"]["target"]["workspace_id"] = json!("test-workspace");
    document["spec"]["events"]["tool.before"]["steps"][0]["operation"] = json!("check");
    document["spec"]["events"]["tool.after"]["steps"] = json!([{
        "id":"shadow-result","provider":"policy","operation":"replace",
        "mode":"shadow","effects":["replace_result"],"on_error":"withhold_result"
    }]);
    if let Some(budget) = after_budget {
        document["spec"]["events"]["tool.after"]["budget_ms"] = json!(budget);
    }
    configure(&mut document);
    f.runtime = Runtime::new(
        serde_json::to_vec(&document).unwrap(),
        &f.directory.path().join("provider-audit"),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    f
}

fn requests(f: &Fixture) -> Vec<Value> {
    fs::read_dir(f.directory.path())
        .unwrap()
        .filter_map(|entry| {
            let file = entry.unwrap().path();
            if file
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let record: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
                let request = &record["request"];
                (request["method"] == "invoke").then(|| request.clone())
            } else {
                None
            }
        })
        .collect()
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

#[test]
fn admitted_siblings_have_distinct_sessions_and_ignore_callback_aliases() {
    let f = with_provider(None);
    let (root, first) = task(&f, "provider-session");
    let second = f.open(&root.binding.instance_id).unwrap()["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.runtime.scope_policy(&second).unwrap();
    f.runtime.acknowledge_scope(&second, deadline()).unwrap();
    for (scope, alias, call) in [
        (&first, "same", "first-call"),
        (&first, "other", "second-call"),
        (&second, "same", "third-call"),
    ] {
        let mut operation = callback(&f, scope);
        if let Operation::SdkCallback { native_input, .. } = &mut operation {
            native_input["session_id"] = json!(alias);
            native_input["tool_use_id"] = json!(call);
        }
        f.runtime
            .handle(operation, deadline(), &AtomicBool::new(false))
            .unwrap();
    }
    let evidence = requests(&f);
    let session = |call: &str| {
        &evidence
            .iter()
            .find(|request| request["event"]["tool"]["call_id"] == call)
            .unwrap()["binding"]["session_id"]
    };
    assert_eq!(evidence.len(), 3);
    assert!(session("first-call").is_string());
    assert_eq!(session("first-call"), session("second-call"));
    assert_ne!(session("first-call"), session("third-call"));
}

fn post_tool(f: &Fixture) -> Value {
    let mut native = fixture()["native_before"].clone();
    native["cwd"] = json!(f.directory.path());
    native["hook_event_name"] = json!("PostToolUse");
    native["tool_response"] = json!("original result text");
    native
}

#[test]
fn native_sdk_shadow_projects_result_and_keeps_original_callback_output() {
    let f = with_provider(None);
    let (_, id) = task(&f, "shadow-projection");
    let native = post_tool(&f);
    let result = f
        .runtime
        .handle(
            Operation::SdkCallback {
                instance_id: id.clone(),
                event: "tool.after".into(),
                step_id: "shadow-result".into(),
                native_input: native,
                close: false,
            },
            deadline(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(result, fixture()["neutral_callback"]);
    let until = deadline();
    loop {
        let event_path = f.directory.path().join("provider-audit/shadow");
        let projected = requests(&f).iter().any(|request| {
            request["event"]["tool"]["result_projection"]["text"] == "original result text"
        });
        let recorded = fs::read_dir(event_path)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| fs::read_to_string(entry.path()).ok())
            .any(|body| body.contains(r#""effect_types":["replace_result"]"#));
        if projected && recorded {
            break;
        }
        assert!(Instant::now() < until, "shadow result did not complete");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn exhausted_shadow_budget_reports_unevaluated() {
    let f = with_provider(Some(1000));
    let (_, id) = task(&f, "shadow-remaining-budget");
    let result = f
        .runtime
        .handle(
            Operation::SdkCallback {
                instance_id: id,
                event: "tool.after".into(),
                step_id: "shadow-result".into(),
                native_input: post_tool(&f),
                close: false,
            },
            deadline(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(result, fixture()["neutral_callback"]);
    let until = deadline();
    loop {
        let recorded = fs::read_dir(f.directory.path().join("provider-audit/shadow"))
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| fs::read_to_string(entry.path()).ok())
            .any(|body| body.contains(r#""reason":"event_budget","status":"not_evaluated""#));
        if recorded {
            break;
        }
        assert!(
            Instant::now() < until,
            "shadow should be recorded as unevaluated"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(requests(&f).is_empty());
}

fn invoke(f: &Fixture, operation: Operation) -> Value {
    f.runtime
        .handle(operation, deadline(), &AtomicBool::new(false))
        .unwrap()
}

fn sdk_event(id: &str, event: &str, step: &str, native_input: Value, close: bool) -> Operation {
    Operation::SdkCallback {
        instance_id: id.into(),
        event: event.into(),
        step_id: step.into(),
        native_input,
        close,
    }
}

fn wait_until<T>(mut poll: impl FnMut() -> Option<T>) -> T {
    let until = deadline();
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(
            Instant::now() < until,
            "fixture did not reach the expected state"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn event_key(f: &Fixture, call_id: &str) -> Option<String> {
    fs::read_dir(f.directory.path().join("provider-audit"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let key = path.file_stem()?.to_str()?;
            if path.extension()? != "jsonl" {
                return None;
            }
            let records = fs::read_to_string(&path).ok()?;
            let plan: Value = serde_json::from_str(records.lines().next()?).ok()?;
            (plan["record"]["plan"]["event"]["tool"]["call_id"] == call_id).then(|| key.to_owned())
        })
        .next()
}

fn before(f: &Fixture, call_id: &str) -> Value {
    let mut input = fixture()["native_before"].clone();
    input["cwd"] = json!(f.directory.path());
    input["tool_use_id"] = json!(call_id);
    input
}

fn check_durable_enforce(f: &Fixture, call_id: &str, step: &str) {
    let key = wait_until(|| event_key(f, call_id));
    wait_until(|| {
        let record = f.runtime.audit.read(&key).ok()?;
        let completed = record["records"].as_array()?.iter().any(|entry| {
            let value = &entry["record"];
            value["phase"] == "completed"
                && value["status"] == "ok"
                && value["call"]["step_id"] == step
        });
        (record["terminal"] == true && completed).then_some(())
    });
}

#[test]
fn slow_shadow_releases_the_event_slot_for_the_next_enforcing_callback() {
    let mut responses = Vec::new();
    for mode in ["off", "shadow"] {
        let f = with_provider_config(None, |document| {
            document["spec"]["daemon"]["limits"]["events"] = json!(1);
            document["spec"]["events"]["tool.before"]["budget_ms"] = json!(10000);
            let provider = document["spec"]["providers"]["policy"].clone();
            document["spec"]["providers"]["optional"] = provider;
            document["spec"]["providers"]["optional"]["config"] = json!({"scenario":"wait"});
            document["spec"]["events"]["tool.before"]["steps"]
                .as_array_mut()
                .unwrap()
                .insert(
                    0,
                    json!({
                        "id":"optional", "provider":"optional", "operation":"second",
                        "mode":mode, "effects":["observe"], "on_error":"report"
                    }),
                );
        });
        let (_, id) = task(&f, "shadow-event-capacity");
        let mut first = before(&f, "capacity-first");
        first["release"] = json!("capacity-first");
        let first_reply = invoke(
            &f,
            sdk_event(&id, "tool.before", "observe", first.clone(), false),
        );
        assert_eq!(first_reply, fixture()["neutral_callback"]);
        if mode == "shadow" {
            assert!(
                f.runtime.shadow.lock().unwrap().is_some(),
                "the final native callback must reserve the isolated slot"
            );
        }
        invoke(&f, sdk_event(&id, "tool.before", "observe", first, true));
        if mode == "shadow" {
            let key = event_key(&f, "capacity-first").unwrap();
            wait_until(|| {
                let started = requests(&f).iter().any(|req| req["operation"] == "second");
                if !started {
                    let observation = f.runtime.audit.read(&key).unwrap();
                    assert!(observation["shadow_records"].is_null(), "{observation}");
                }
                started.then_some(())
            });
            assert!(f.runtime.shadow.lock().unwrap().is_some());
            assert!(f.runtime.events.lock().unwrap().is_empty());
        }
        let second = before(&f, "capacity-second");
        let response = invoke(&f, sdk_event(&id, "tool.before", "observe", second, false));
        check_durable_enforce(&f, "capacity-second", "observe");
        if mode == "shadow" {
            assert!(f.runtime.shadow.lock().unwrap().is_some());
            let key = event_key(&f, "capacity-second").unwrap();
            wait_until(|| {
                let audit = f.runtime.audit.read(&key).ok()?;
                let skipped = audit["shadow_records"]
                    .as_array()?
                    .iter()
                    .any(|entry| entry["record"]["reason"] == "shadow_busy");
                skipped.then_some(())
            });
        }
        invoke(&f, Operation::ReleaseInstance { instance_id: id });
        assert!(f.runtime.shadow.lock().unwrap().is_none());
        responses.push(response);
    }
    assert_eq!(responses[0], responses[1]);
    assert_eq!(responses[1], fixture()["neutral_callback"]);
}

#[test]
fn shadow_journal_write_failure_keeps_enforcing_admission_and_audit_healthy() {
    let f = with_provider(None);
    let (_, id) = task(&f, "shadow-audit-failure");
    let mut first = post_tool(&f);
    first["tool_use_id"] = json!("audit-failure-shadow");
    first["scenario"] = json!("wait");
    first["release"] = json!("shadow-audit");
    let response = invoke(
        &f,
        sdk_event(&id, "tool.after", "shadow-result", first, false),
    );
    assert_eq!(response, fixture()["neutral_callback"]);
    let key = wait_until(|| {
        requests(&f)
            .iter()
            .any(|req| req["operation"] == "replace")
            .then(|| event_key(&f, "audit-failure-shadow"))
            .flatten()
    });
    let dir = f.directory.path().join("provider-audit/shadow");
    fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    // Force the optional journal's exclusive event-file creation to fail.
    fs::create_dir(dir.join(format!("{key}.jsonl"))).unwrap();
    fs::write(f.directory.path().join("release-shadow-audit"), "").unwrap();
    wait_until(|| f.runtime.audit.shadow.failed().then_some(()));
    assert!(f.runtime.audit.healthy());
    assert!(f.runtime.audit.read(&key).unwrap()["shadow_records"].is_null());

    let second = before(&f, "audit-failure-enforce");
    let response = invoke(&f, sdk_event(&id, "tool.before", "observe", second, false));
    assert_eq!(response, fixture()["neutral_callback"]);
    check_durable_enforce(&f, "audit-failure-enforce", "observe");
    assert!(f.runtime.audit.healthy());
    assert!(!f.runtime.stopped.load(Ordering::Acquire));
    invoke(&f, Operation::ReleaseInstance { instance_id: id });
}
