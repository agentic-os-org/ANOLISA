//! Native byte transport, shared callback leases and instance-owned cancellation.

use crate::common::{audit, event, invoke, Fixture, Running, PRIVATE_MARKER, TIMEOUT};
use aw_service::{Binding, Client, EventHandle, Operation};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs, thread,
    time::{Duration, Instant},
};

fn invoke_environment(
    handle: &EventHandle,
    step: &str,
    native_environment: Option<BTreeMap<std::ffi::OsString, std::ffi::OsString>>,
) -> Operation {
    Operation::InvokeStep {
        event_id: handle.event_id.clone(),
        instance_id: handle.instance_id.clone(),
        step_id: step.into(),
        native_environment,
    }
}

#[test]
fn native_callback_environment_replaces_binding_without_changing_provider_input_or_audit() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    native(&mut document, "import json; print(json.dumps({'profile': os.environ.get('PROFILE_SECRET'), 'bound': os.environ.get('AW_SERVICE_MARKER'), 'stdin': sys.stdin.buffer.read().hex()}))", true);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let secret = "callback-profile-secret-not-for-audit";
    let original = b"native input\x00\xff";
    for (index, environment, expected_profile, expected_bound) in [
        (0, None, Value::Null, json!("explicit-context")),
        (1, Some(BTreeMap::new()), Value::Null, Value::Null),
        (
            2,
            Some(BTreeMap::from([("PROFILE_SECRET".into(), secret.into())])),
            json!(secret),
            Value::Null,
        ),
    ] {
        let mut value = event(&binding, "tool.before", "allow");
        value["tool"]["call_id"] = json!(format!("environment-{index}"));
        let handle = lease(&service.client, &binding, value.clone(), original);
        let mut operation =
            serde_json::to_value(invoke_environment(&handle, "raw", environment.clone())).unwrap();
        if index == 0 {
            operation
                .as_object_mut()
                .unwrap()
                .remove("native_environment");
        }
        let result = service
            .client
            .call(
                serde_json::from_value(operation).unwrap(),
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        assert_eq!(result["status"], "ok");
        let stdout: Vec<u8> = serde_json::from_value(result["native"]["stdout"].clone()).unwrap();
        let observed: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(observed["profile"], expected_profile);
        assert_eq!(observed["bound"], expected_bound);
        assert_eq!(observed["stdin"], "6e617469766520696e70757400ff");
        let provider = service
            .client
            .call(
                invoke_environment(&handle, "check", environment),
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        assert_eq!(provider["status"], "ok");
        let calls = fixture.calls("invoke");
        let observed = calls
            .iter()
            .find(|call| call["request"]["event"]["tool"]["call_id"] == value["tool"]["call_id"])
            .unwrap();
        assert_eq!(observed["request"]["event"], value);
        assert_eq!(
            observed["environment"]["AW_SERVICE_MARKER"],
            "explicit-context"
        );
        assert!(observed["environment"].get("PROFILE_SECRET").is_none());
        let evidence = terminal(&service.client, &handle.event_id).to_string();
        assert!(!evidence.contains(secret));
        assert!(!evidence.contains("PROFILE_SECRET"));
    }
    fixture.assert_reaped();
}

#[test]
fn invalid_native_environments_are_recorded_failures_and_consume_the_step_claim() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    native(
        &mut document,
        "pathlib.Path('must-not-run').write_text('bad')",
        false,
    );
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let invalid = [
        BTreeMap::from([("".into(), PRIVATE_MARKER.into())]),
        BTreeMap::from([("BAD=KEY".into(), PRIVATE_MARKER.into())]),
        BTreeMap::from([("BAD\0KEY".into(), PRIVATE_MARKER.into())]),
        BTreeMap::from([("BAD_VALUE".into(), format!("{PRIVATE_MARKER}\0").into())]),
        BTreeMap::from([(
            "LARGE".into(),
            "x".repeat(aw_host::MAX_NATIVE_ENVIRONMENT_BYTES).into(),
        )]),
        (0..4097)
            .map(|n| (format!("K{n}").into(), PRIVATE_MARKER.into()))
            .collect(),
    ];
    for (index, environment) in invalid.into_iter().enumerate() {
        let mut value = event(&binding, "tool.before", "unused");
        value["tool"]["call_id"] = json!(format!("invalid-env-{index}"));
        let handle = lease(&service.client, &binding, value, b"{}");
        let result = service
            .client
            .call(
                invoke_environment(&handle, "raw", Some(environment)),
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        assert_eq!(result["status"], "error");
        assert_eq!(result["failure"], "native_environment");
        assert_eq!(result["failure_action"], "block");
        assert_eq!(result["call"]["process"], Value::Null);
        assert!(service
            .client
            .call(invoke(&handle, "raw"), Instant::now() + TIMEOUT)
            .is_err());
        let evidence = terminal(&service.client, &handle.event_id).to_string();
        assert!(evidence.contains("native_environment"));
        assert!(!evidence.contains(PRIVATE_MARKER));
        assert!(!evidence.contains("BAD_VALUE"));
    }
    assert!(!fixture.0.join("must-not-run").exists());
    fixture.assert_reaped();
}

#[test]
fn changing_native_snapshots_does_not_restart_event_budget_or_allow_replay() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    native(
        &mut document,
        "time.sleep(.35); print(os.environ.get('CALLBACK_STEP'))",
        false,
    );
    document["spec"]["events"]["tool.before"]["budget_ms"] = json!(600);
    let mut second = document["spec"]["events"]["tool.before"]["steps"][0].clone();
    second["id"] = json!("second");
    document["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let handle = lease(
        &service.client,
        &binding,
        event(&binding, "tool.before", "unused"),
        b"{}",
    );
    let snapshot = |value: &str| Some(BTreeMap::from([("CALLBACK_STEP".into(), value.into())]));
    let first = service
        .client
        .call(
            invoke_environment(&handle, "raw", snapshot("first")),
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    assert_eq!(first["status"], "ok");
    assert!(service
        .client
        .call(
            invoke_environment(&handle, "raw", snapshot("replay")),
            Instant::now() + TIMEOUT
        )
        .is_err());
    let second = service
        .client
        .call(
            invoke_environment(&handle, "second", snapshot("second")),
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    assert_eq!(second["status"], "error");
    assert_eq!(second["failure"], "transport");
    terminal(&service.client, &handle.event_id);
    fixture.assert_reaped();
}

fn native(document: &mut Value, code: &str, mixed: bool) {
    let python = document["spec"]["providers"]["policy"]["transport"]["argv"][0].clone();
    let code = format!(
        r#"import os, pathlib, signal, sys, time
pid = os.getpid()
start = pathlib.Path('/proc/self/stat').read_text().rsplit(')', 1)[1].split()[19]
pathlib.Path(str(pid) + '.pid').write_text(str(pid) + ' ' + start)
{code}
"#
    );
    document["spec"]["providers"]["raw"] = json!({
        "protocol": "native-hook/v1alpha1", "config": {},
        "timeout_ms": 5000, "max_output_bytes": 1048576,
        "transport": {"type": "stdio", "location": "agent", "argv": [python, "-I", "-c", code]}
    });
    for name in ["tool.before", "tool.after"] {
        let step = json!({"id": "raw", "provider": "raw", "native": {},
            "on_error": if name == "tool.before" { "block" } else { "report" }});
        if mixed {
            document["spec"]["events"][name]["steps"]
                .as_array_mut()
                .unwrap()
                .push(step);
        } else {
            document["spec"]["events"][name]["steps"] = json!([step]);
        }
    }
}

fn request(binding: &Binding, value: Value, bytes: &[u8]) -> Operation {
    Operation::OpenHookEvent {
        instance_id: binding.instance_id.clone(),
        event: value,
        native_input: bytes.to_vec(),
    }
}

fn lease(client: &Client, binding: &Binding, value: Value, bytes: &[u8]) -> EventHandle {
    serde_json::from_value(
        client
            .call(request(binding, value, bytes), Instant::now() + TIMEOUT)
            .unwrap(),
    )
    .unwrap()
}

fn terminal(client: &Client, key: &str) -> Value {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let value = audit(client, key);
        if value["terminal"] == true {
            return value;
        }
        assert!(Instant::now() < deadline, "native event did not terminate");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn raw_bytes_nonzero_and_after_status_are_preserved_without_provider_discovery() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    native(&mut document, "sys.stdout.buffer.write(sys.stdin.buffer.read()); sys.stderr.buffer.write(b'diagnostic\\xff'); sys.exit(2)", false);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    assert!(fixture.calls("describe").is_empty());
    assert!(fixture.calls("validate_config").is_empty());
    assert!(fs::read_dir(&fixture.0).unwrap().all(|entry| entry
        .unwrap()
        .path()
        .extension()
        .is_none_or(|ext| ext != "pid")));
    for name in ["tool.before", "tool.after"] {
        let input = [PRIVATE_MARKER.as_bytes(), &[0, 255, 10]].concat();
        let value = event(&binding, name, "unused");
        let handle = lease(&service.client, &binding, value.clone(), &input);
        let result = service
            .client
            .call(invoke(&handle, "raw"), Instant::now() + TIMEOUT)
            .unwrap();
        assert_eq!(result["kind"], "native");
        assert_eq!(result["status"], "ok");
        assert_eq!(result["native"]["stdout"], json!(input));
        assert_eq!(
            result["native"]["stderr"],
            json!(b"diagnostic\xff".to_vec())
        );
        assert_eq!(result["native"]["exit_code"], 2);
        assert_eq!(result["native"]["signal"], Value::Null);
        assert_eq!(result["failure_action"], Value::Null);
        assert!(result.get("effects").is_none());
        assert_eq!(result["call"]["method"], "native_hook");
        let evidence = terminal(&service.client, &handle.event_id);
        let text = serde_json::to_string(&evidence).unwrap();
        assert!(!text.contains(PRIVATE_MARKER));
        assert!(!text.contains("diagnostic"));
        assert!(service
            .client
            .call(request(&binding, value, &input), Instant::now() + TIMEOUT)
            .is_err());
        assert!(service
            .client
            .call(invoke(&handle, "raw"), Instant::now() + TIMEOUT)
            .is_err());
    }
    fixture.assert_reaped();
}

#[test]
fn concurrent_opens_share_one_mixed_event_and_reject_mismatched_inputs() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    native(
        &mut document,
        "sys.stdout.buffer.write(sys.stdin.buffer.read())",
        true,
    );
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let value = event(&binding, "tool.before", "block");
    let input = b"original native callback";
    let handles = thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| lease(&service.client, &binding, value.clone(), input)))
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let handle = &handles[0];
    assert!(handles
        .iter()
        .all(|other| other.event_id == handle.event_id));
    assert_eq!(handle.steps, ["check", "raw"]);
    assert!(service
        .client
        .call(
            request(&binding, value.clone(), b"different"),
            Instant::now() + TIMEOUT
        )
        .is_err());
    let mut other = value.clone();
    other["tool"]["input"] = json!({"changed": true});
    assert!(service
        .client
        .call(request(&binding, other, input), Instant::now() + TIMEOUT)
        .is_err());
    thread::scope(|scope| {
        let provider = scope.spawn(|| {
            service
                .client
                .call(invoke(handle, "check"), Instant::now() + TIMEOUT)
                .unwrap()
        });
        let native = scope.spawn(|| {
            service
                .client
                .call(invoke(handle, "raw"), Instant::now() + TIMEOUT)
                .unwrap()
        });
        assert_eq!(provider.join().unwrap()["effects"][0]["type"], "block");
        assert_eq!(native.join().unwrap()["native"]["stdout"], json!(input));
    });
    let evidence = terminal(&service.client, &handle.event_id);
    let completed: Vec<_> = evidence["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["record"]["phase"] == "completed")
        .collect();
    assert_eq!(completed.len(), 2);
    assert_eq!(
        completed[0]["record"]["call"]["host_event_id"],
        completed[1]["record"]["call"]["host_event_id"]
    );
    assert!(service
        .client
        .call(request(&binding, value, input), Instant::now() + TIMEOUT)
        .is_err());
    fixture.assert_reaped();
}

#[test]
fn native_signals_and_output_limit_faults_remain_distinct() {
    for (code, limit, signal) in [
        ("os.kill(os.getpid(), signal.SIGTERM)", 1024, true),
        ("sys.stdout.buffer.write(b'x' * 2048)", 1024, false),
    ] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        native(&mut document, code, false);
        document["spec"]["providers"]["raw"]["max_output_bytes"] = json!(limit);
        let service = Running::start(&fixture, &document);
        let binding = fixture.bind(&service.client);
        let handle = lease(
            &service.client,
            &binding,
            event(&binding, "tool.before", "unused"),
            b"{}",
        );
        let result = service
            .client
            .call(invoke(&handle, "raw"), Instant::now() + TIMEOUT)
            .unwrap();
        if signal {
            assert_eq!(result["status"], "ok");
            assert_eq!(result["native"]["signal"], 15);
            assert_eq!(result["native"]["exit_code"], Value::Null);
            assert_eq!(result["failure_action"], Value::Null);
        } else {
            assert_eq!(result["status"], "error");
            assert_eq!(result["native"], Value::Null);
            assert_eq!(result["failure"], "transport");
            assert_eq!(result["failure_action"], "block");
        }
        terminal(&service.client, &handle.event_id);
        fixture.assert_reaped();
    }
}

#[test]
fn release_cancels_only_owned_events_and_retains_terminal_audit() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    native(&mut document, "time.sleep(30)", false);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let other = fixture.bind(&service.client);
    let other_handle = lease(
        &service.client,
        &other,
        event(&other, "tool.after", "unused"),
        b"{}",
    );
    let handle = lease(
        &service.client,
        &binding,
        event(&binding, "tool.before", "unused"),
        b"{}",
    );
    thread::scope(|scope| {
        let pending = scope.spawn(|| {
            service
                .client
                .call(invoke(&handle, "raw"), Instant::now() + TIMEOUT)
        });
        let deadline = Instant::now() + TIMEOUT;
        while !fs::read_dir(&fixture.0).unwrap().any(|entry| {
            entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "pid")
        }) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            service
                .client
                .call(
                    Operation::ReleaseInstance {
                        instance_id: binding.instance_id.clone()
                    },
                    Instant::now() + TIMEOUT
                )
                .unwrap()["released"],
            true
        );
        let result = pending.join().unwrap().unwrap();
        assert_eq!(result["status"], "error");
        assert_eq!(result["failure"], "transport");
    });
    assert_eq!(
        terminal(&service.client, &handle.event_id)["terminal"],
        true
    );
    assert!(service
        .client
        .call(
            request(&binding, event(&binding, "tool.before", "unused"), b"{}"),
            Instant::now() + TIMEOUT
        )
        .is_err());
    let status = service
        .client
        .call(Operation::Status, Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(status["bindings"], 1);
    assert_eq!(status["events"], 1);
    service
        .client
        .call(
            Operation::ReleaseInstance {
                instance_id: other.instance_id,
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    assert_eq!(
        terminal(&service.client, &other_handle.event_id)["terminal"],
        true
    );
    fixture.assert_reaped();
}

#[test]
fn expired_lease_cannot_restart_and_large_raw_frames_remain_bounded() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    native(
        &mut document,
        "sys.stdout.buffer.write(sys.stdin.buffer.read())",
        false,
    );
    document["spec"]["execution"]["default_event_budget_ms"] = json!(100);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let value = event(&binding, "tool.before", "unused");
    let handle = lease(&service.client, &binding, value.clone(), b"{}");
    terminal(&service.client, &handle.event_id);
    assert!(service
        .client
        .call(request(&binding, value, b"{}"), Instant::now() + TIMEOUT)
        .is_err());
    let large = vec![255u8; aw_provider::MAX_MESSAGE_BYTES + 1];
    assert!(service
        .client
        .call(
            request(&binding, event(&binding, "tool.after", "unused"), &large),
            Instant::now() + TIMEOUT
        )
        .is_err());
    fixture.assert_reaped();
}

#[test]
fn maximum_raw_byte_arrays_round_trip_within_the_frame_limit() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    native(
        &mut document,
        "sys.stdout.buffer.write(sys.stdin.buffer.read())",
        false,
    );
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let input = vec![255u8; aw_provider::MAX_MESSAGE_BYTES];
    let handle = lease(
        &service.client,
        &binding,
        event(&binding, "tool.before", "unused"),
        &input,
    );
    let result = service
        .client
        .call(invoke(&handle, "raw"), Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(result["native"]["stdout"], json!(input));
    assert_eq!(result["native"]["exit_code"], 0);
    terminal(&service.client, &handle.event_id);
    fixture.assert_reaped();
}

#[test]
fn serial_native_callbacks_reuse_the_original_budget_and_once_only_claims() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    native(
        &mut document,
        "sys.stdin.buffer.read(); time.sleep(0.7)",
        false,
    );
    let mut second = document["spec"]["events"]["tool.before"]["steps"][0].clone();
    second["id"] = json!("second");
    document["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(second);
    document["spec"]["execution"]["default_event_budget_ms"] = json!(1200);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let value = event(&binding, "tool.before", "unused");
    let first = lease(&service.client, &binding, value.clone(), b"{}");
    assert_eq!(
        service
            .client
            .call(invoke(&first, "raw"), Instant::now() + TIMEOUT)
            .unwrap()["status"],
        "ok"
    );
    let second = lease(&service.client, &binding, value.clone(), b"{}");
    assert_eq!(first.event_id, second.event_id);
    assert!(service
        .client
        .call(invoke(&second, "raw"), Instant::now() + TIMEOUT)
        .is_err());
    let result = service
        .client
        .call(invoke(&second, "second"), Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(result["status"], "error");
    assert_eq!(result["failure"], "transport");
    assert_eq!(result["failure_action"], "block");
    terminal(&service.client, &first.event_id);
    assert!(service
        .client
        .call(request(&binding, value, b"{}"), Instant::now() + TIMEOUT)
        .is_err());
    fixture.assert_reaped();
}
