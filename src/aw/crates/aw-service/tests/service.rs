//! A real local client, service and Provider exchange without an Agent or model.
#![cfg(target_os = "linux")]

mod common;

use aw_service::{Client, Operation, Server};
use common::{
    audit, close, event, invoke, open, second_step, Fixture, Running, PRIVATE_MARKER, TIMEOUT,
};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn before_allow_block_and_after_observe_are_durable_candidates() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let binding = fixture.bind(&service.client);
    assert_eq!(fixture.calls("describe").len(), 1);
    assert_eq!(fixture.calls("validate_config").len(), 1);
    assert_eq!(audit(&service.client, &binding.audit_key)["terminal"], true);
    for (name, scenario, step, expected) in [
        ("tool.before", "allow", "check", json!([])),
        (
            "tool.before",
            "block",
            "check",
            json!([{"type":"block","reason_code":"fixture_policy"}]),
        ),
        (
            "tool.after",
            "observe",
            "record",
            json!([{"type":"observe","reason_code":"fixture_policy"}]),
        ),
    ] {
        let handle = open(&service.client, &binding, event(&binding, name, scenario));
        let result = service
            .client
            .call(invoke(&handle, step), Instant::now() + TIMEOUT)
            .unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(result["effects"], expected);
        assert_eq!(result["failure_action"], Value::Null);
        assert_eq!(close(&service.client, &handle)["closed"], true);
        let evidence = audit(&service.client, &handle.event_id);
        assert_eq!(evidence["terminal"], true);
        let records = evidence["records"].as_array().unwrap();
        let retained = &records[0]["record"]["plan"]["event"];
        assert_eq!(retained["name"], name);
        assert_eq!(retained["session_id"], "native-session");
        assert_eq!(retained["tool"]["name"], "arbitrary_tool");
        assert_eq!(retained["tool"]["native_name"], "custom:工具");
        assert_eq!(retained["tool"]["call_id"], "native-call");
        assert!(records
            .iter()
            .any(|entry| entry["record"]["phase"] == "started"));
        assert!(records
            .iter()
            .any(|entry| entry["record"]["phase"] == "completed"));
        assert!(!serde_json::to_string(&evidence)
            .unwrap()
            .contains(PRIVATE_MARKER));
    }
    let status = service
        .client
        .call(Operation::Status, Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(status["guarantee"], "native_hook");
    assert_eq!(status["adoption"], "unverified");
    fixture.assert_reaped();
}

#[test]
fn binding_pins_explicit_context_and_preserves_native_payload() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let binding = fixture.bind(&service.client);
    let value = event(&binding, "tool.after", "observe");
    let handle = open(&service.client, &binding, value.clone());
    service
        .client
        .call(invoke(&handle, "record"), Instant::now() + TIMEOUT)
        .unwrap();
    close(&service.client, &handle);
    let calls = fixture.calls("invoke");
    assert_eq!(calls[0]["request"]["event"], value);
    assert_eq!(calls[0]["cwd"], fixture.0.to_str().unwrap());
    assert_eq!(
        calls[0]["environment"]["AW_SERVICE_MARKER"],
        "explicit-context"
    );
    assert!(calls[0]["environment"].get("HOME").is_none());
    assert_eq!(
        calls[0]["request"]["config_revision"],
        binding.identity.config_revision
    );
    service
        .client
        .call(
            Operation::Unbind {
                instance_id: binding.instance_id,
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    assert_eq!(
        service
            .client
            .call(Operation::Status, Instant::now() + TIMEOUT)
            .unwrap()["bindings"],
        0
    );
}

#[test]
fn caller_parallel_steps_reach_a_two_party_barrier() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    second_step(&mut document);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let handle = open(
        &service.client,
        &binding,
        event(&binding, "tool.before", "barrier"),
    );
    thread::scope(|scope| {
        let first = scope.spawn(|| {
            service
                .client
                .call(invoke(&handle, "check"), Instant::now() + TIMEOUT)
        });
        let second = scope.spawn(|| {
            service
                .client
                .call(invoke(&handle, "second"), Instant::now() + TIMEOUT)
        });
        assert_eq!(first.join().unwrap().unwrap()["status"], "ok");
        assert_eq!(second.join().unwrap().unwrap()["status"], "ok");
    });
    assert!(fixture.0.join("check.overlap").exists());
    assert!(fixture.0.join("second.overlap").exists());
    close(&service.client, &handle);
    fixture.assert_reaped();
}

#[test]
fn serial_steps_share_the_original_event_deadline() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    second_step(&mut document);
    document["spec"]["events"]["tool.before"]["budget_ms"] = json!(1200);
    document["spec"]["providers"]["policy"]["config"]["delays_ms"] =
        json!({"check": 700, "second": 700});
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let handle = open(
        &service.client,
        &binding,
        event(&binding, "tool.before", "observe"),
    );
    let first = service
        .client
        .call(invoke(&handle, "check"), Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(first["status"], "ok");
    let second = service
        .client
        .call(invoke(&handle, "second"), Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(second["status"], "error");
    assert_eq!(second["effects"], json!([]));
    assert_eq!(second["failure_action"], "block");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let evidence = audit(&service.client, &handle.event_id);
        if evidence["terminal"] == true {
            break;
        }
        assert!(Instant::now() < deadline, "event expiry was not recorded");
        thread::sleep(Duration::from_millis(5));
    }
    let calls = fixture.calls("invoke");
    assert_eq!(calls.len(), 2);
    let first_budget = calls
        .iter()
        .find(|call| call["request"]["operation"] == "check")
        .unwrap()["request"]["budget_ms"]
        .as_u64()
        .unwrap();
    let second_budget = calls
        .iter()
        .find(|call| call["request"]["operation"] == "second")
        .unwrap()["request"]["budget_ms"]
        .as_u64()
        .unwrap();
    assert!(second_budget < first_budget);
    fixture.assert_reaped();
}

#[test]
fn an_event_step_cannot_be_replayed() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let binding = fixture.bind(&service.client);
    let handle = open(
        &service.client,
        &binding,
        event(&binding, "tool.before", "allow"),
    );
    service
        .client
        .call(invoke(&handle, "check"), Instant::now() + TIMEOUT)
        .unwrap();
    let error = service
        .client
        .call(invoke(&handle, "check"), Instant::now() + TIMEOUT)
        .unwrap_err();
    assert!(error.to_string().contains("step_unknown_or_claimed"));
    assert_eq!(fixture.calls("invoke").len(), 1);
    close(&service.client, &handle);
}

#[test]
fn instance_identity_prevents_cross_binding_calls_and_early_unbind() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let first = fixture.bind(&service.client);
    let second = fixture.bind(&service.client);
    let handle = open(
        &service.client,
        &first,
        event(&first, "tool.before", "allow"),
    );
    let mismatched = Operation::InvokeStep {
        event_id: handle.event_id.clone(),
        instance_id: second.instance_id.clone(),
        step_id: "check".into(),
    };
    assert!(service
        .client
        .call(mismatched, Instant::now() + TIMEOUT)
        .is_err());
    assert!(service
        .client
        .call(
            Operation::OpenEvent {
                instance_id: second.instance_id,
                event: event(&first, "tool.before", "allow")
            },
            Instant::now() + TIMEOUT
        )
        .is_err());
    assert!(service
        .client
        .call(
            Operation::Unbind {
                instance_id: first.instance_id.clone()
            },
            Instant::now() + TIMEOUT
        )
        .is_err());
    assert_eq!(fixture.calls("invoke").len(), 0);
    close(&service.client, &handle);
    service
        .client
        .call(
            Operation::Unbind {
                instance_id: first.instance_id,
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
}

#[test]
fn malformed_provider_response_remains_execution_failure() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let binding = fixture.bind(&service.client);
    let handle = open(
        &service.client,
        &binding,
        event(&binding, "tool.before", "malformed"),
    );
    let result = service
        .client
        .call(invoke(&handle, "check"), Instant::now() + TIMEOUT)
        .unwrap();
    assert_eq!(result["status"], "error");
    assert_eq!(result["failure"], "protocol");
    assert_eq!(result["failure_action"], "block");
    assert_eq!(result["effects"], json!([]));
    assert!(!result.to_string().contains(PRIVATE_MARKER));
    close(&service.client, &handle);
    let evidence = audit(&service.client, &handle.event_id);
    assert!(!evidence.to_string().contains(PRIVATE_MARKER));
    assert!(evidence["records"]
        .as_array()
        .unwrap()
        .iter()
        .any(|record| record["record"]["policy_block"] == false
            && record["record"]["failure"] == "protocol"));
}

#[test]
fn closing_an_event_cancels_running_work_before_acknowledgement() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let binding = fixture.bind(&service.client);
    let handle = open(
        &service.client,
        &binding,
        event(&binding, "tool.before", "wait"),
    );
    thread::scope(|scope| {
        let request = scope.spawn(|| {
            service
                .client
                .call(invoke(&handle, "check"), Instant::now() + TIMEOUT)
        });
        fixture.wait_for_calls("invoke", 1);
        assert_eq!(close(&service.client, &handle)["closed"], true);
        fixture.assert_reaped();
        let result = request.join().unwrap().unwrap();
        assert_eq!(result["status"], "error");
        assert_eq!(result["effects"], json!([]));
    });
    assert_eq!(audit(&service.client, &handle.event_id)["terminal"], true);
}

#[test]
fn shorter_invocation_rpc_deadline_cancels_the_shared_event() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let binding = fixture.bind(&service.client);
    let handle = open(
        &service.client,
        &binding,
        event(&binding, "tool.before", "wait"),
    );
    thread::scope(|scope| {
        let request = scope.spawn(|| {
            service.client.call(
                invoke(&handle, "check"),
                Instant::now() + Duration::from_secs(1),
            )
        });
        fixture.wait_for_calls("invoke", 1);
        assert!(request.join().unwrap().is_err());
    });
    // No explicit close or service cancellation participates in this cleanup.
    let cleanup_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let evidence = service
            .client
            .call(
                Operation::Audit {
                    key: handle.event_id.clone(),
                },
                cleanup_deadline,
            )
            .unwrap();
        if evidence["terminal"] == true {
            let last = evidence["records"].as_array().unwrap().last().unwrap();
            assert_eq!(last["record"]["status"], "cancelled");
            break;
        }
        assert!(
            Instant::now() < cleanup_deadline,
            "RPC timeout did not close the event"
        );
        thread::sleep(Duration::from_millis(5));
    }
    fixture.assert_reaped();
    assert_eq!(
        service
            .client
            .call(Operation::Status, Instant::now() + TIMEOUT)
            .unwrap()["events"],
        0
    );
}

#[test]
fn stop_joins_provider_and_descendant_processes() {
    let fixture = Fixture::new();
    let mut service = Running::start(&fixture, &fixture.document());
    let binding = fixture.bind(&service.client);
    let handle = open(
        &service.client,
        &binding,
        event(&binding, "tool.before", "descendant"),
    );
    let client = service.client.clone();
    thread::scope(|scope| {
        let request =
            scope.spawn(|| client.call(invoke(&handle, "check"), Instant::now() + TIMEOUT));
        let deadline = Instant::now() + TIMEOUT;
        while !fixture.0.join("descendant-ready").exists() {
            assert!(Instant::now() < deadline, "descendant did not start");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            service
                .client
                .call(Operation::Stop, Instant::now() + TIMEOUT)
                .unwrap()["stopping"],
            true
        );
        service.finish().unwrap();
        fixture.assert_reaped();
        let result = request.join().unwrap().unwrap();
        assert_eq!(result["status"], "error");
        assert_eq!(result["effects"], json!([]));
    });
}

#[test]
fn failed_durable_claim_prevents_provider_dispatch() {
    let fixture = Fixture::new();
    let mut service = Running::start(&fixture, &fixture.document());
    let journal = fixture.state().join("journal");
    let retained = fixture.state().join("retained-journal");
    fs::rename(&journal, &retained).unwrap();
    fs::write(&journal, b"not a directory").unwrap();
    let result = service
        .client
        .call(fixture.bind_operation(), Instant::now() + TIMEOUT);
    fs::remove_file(&journal).unwrap();
    fs::rename(&retained, &journal).unwrap();
    assert!(result.is_err());
    assert!(service.finish().is_err());
    assert_eq!(fixture.calls("describe").len(), 0);
    assert_eq!(fixture.calls("validate_config").len(), 0);
    assert_eq!(fixture.calls("invoke").len(), 0);
}

#[test]
fn preparation_failure_returns_readable_audit_without_binding() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["providers"]["policy"]["config"]["reject"] = json!(true);
    let service = Running::start(&fixture, &document);
    let error = service
        .client
        .call(fixture.bind_operation(), Instant::now() + TIMEOUT)
        .unwrap_err();
    let aw_service::Error::Attempt { code, audit_key } = error else {
        panic!("expected a queryable preparation failure");
    };
    assert_eq!(code, "preparation_failed");
    let evidence = audit(&service.client, &audit_key);
    assert_eq!(evidence["terminal"], true);
    let completed = &evidence["records"].as_array().unwrap().last().unwrap()["record"];
    assert_eq!(completed["status"], "error");
    let calls = completed["calls"].to_string();
    assert!(calls.contains("describe"));
    assert!(calls.contains("validate_config"));
    assert!(!evidence.to_string().contains(PRIVATE_MARKER));
    assert_eq!(fixture.calls("describe").len(), 1);
    assert_eq!(fixture.calls("validate_config").len(), 1);
    assert_eq!(fixture.calls("invoke").len(), 0);
    assert_eq!(
        service
            .client
            .call(Operation::Status, Instant::now() + TIMEOUT)
            .unwrap()["bindings"],
        0
    );
    fixture.assert_reaped();
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}

#[test]
fn request_rejects_ambiguous_json_before_connecting() {
    let Operation::OpenEvent { event, .. } = Operation::from_json(
        br#"{"method":"open_event","instance_id":"fixture","event":{"n":18446744073709551615,"min":-9223372036854775808,"float":1.25,"\u4e2d":"18446744073709551617"}}"#,
    )
    .unwrap() else {
        panic!("expected open_event");
    };
    assert_eq!(event["n"].as_u64(), Some(u64::MAX));
    assert_eq!(event["min"].as_i64(), Some(i64::MIN));
    assert_eq!(event["float"], json!(1.25));
    assert_eq!(event["中"], "18446744073709551617");
    let fixture = Fixture::new();
    let input = fixture.0.join("operation.json");
    let errors = fixture.0.join("request.stderr");
    for (event, expected) in [
        (r#"{"n":18446744073709551617}"#, "integer is outside"),
        (r#"{"n":-9223372036854775809}"#, "integer is outside"),
        (r#"{"n":1,"\u006e":2}"#, "without duplicate fields"),
    ] {
        fs::write(
            &input,
            format!(r#"{{"method":"open_event","instance_id":"fixture","event":{event}}}"#),
        )
        .unwrap();
        let mut child = ChildGuard(
            Command::new(env!("CARGO_BIN_EXE_aw"))
                .args(["request", "--socket"])
                .arg(fixture.0.join("missing.sock"))
                .stdin(fs::File::open(&input).unwrap())
                .stdout(Stdio::null())
                .stderr(fs::File::create(&errors).unwrap())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "request did not exit");
            thread::sleep(Duration::from_millis(5));
        };
        assert!(!status.success());
        let error = fs::read_to_string(&errors).unwrap();
        assert!(error.contains(expected), "{event}: {error}");
    }
}

#[test]
fn restart_rejects_stale_clients_and_keeps_crashed_event_incomplete() {
    let fixture = Fixture::new();
    let document = fixture.document();
    let configuration = fixture.0.join("config.json");
    fs::write(&configuration, serde_json::to_vec(&document).unwrap()).unwrap();
    let binary = env!("CARGO_BIN_EXE_aw");
    let state = fixture.state();
    let mut child = ChildGuard(
        Command::new(binary)
            .args(["serve", "--config"])
            .arg(&configuration)
            .arg("--state-dir")
            .arg(&state)
            .current_dir(&fixture.0)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(fixture.0.join("service.stderr")).unwrap())
            .spawn()
            .unwrap(),
    );
    fs::write(fixture.0.join("service-process.json"), json!({"command": [binary, "serve", "--config", configuration, "--state-dir", state], "cwd": fixture.0,
        "pid": child.0.id(), "ports": [], "log": fixture.0.join("service.stderr"), "timeout_seconds": 10,
        "stop": format!("kill -TERM -- -{}", child.0.id())}).to_string()).unwrap();
    let socket = state.join("aw.sock");
    let deadline = Instant::now() + TIMEOUT;
    let old_client = loop {
        if let Ok(client) = Client::connect(&socket, Instant::now() + Duration::from_millis(100)) {
            break client;
        }
        assert!(
            Instant::now() < deadline,
            "service child did not become ready"
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "service child exited"
        );
        thread::sleep(Duration::from_millis(10));
    };
    let binding = fixture.bind(&old_client);
    let handle = open(
        &old_client,
        &binding,
        event(&binding, "tool.before", "allow"),
    );
    assert_eq!(audit(&old_client, &handle.event_id)["terminal"], false);
    // Kill only after preparation finishes and before invocation: no Provider child
    // exists, while a durable event reservation remains interrupted on disk.
    fixture.assert_reaped();
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(Server::bind(serde_json::to_vec(&document).unwrap(), &state).is_err());
    fs::remove_file(&socket).unwrap();
    let service = Running::start(&fixture, &document);
    assert_ne!(
        service.client.identity().generation,
        old_client.identity().generation
    );
    assert_eq!(
        service.client.identity().config_revision,
        old_client.identity().config_revision
    );
    assert!(old_client
        .call(Operation::Status, Instant::now() + TIMEOUT)
        .unwrap_err()
        .to_string()
        .contains("stale_service"));
    let evidence = audit(&service.client, &handle.event_id);
    assert_eq!(evidence["terminal"], false);
    assert_eq!(evidence["records"].as_array().unwrap().len(), 1);
    assert!(service
        .client
        .call(invoke(&handle, "check"), Instant::now() + TIMEOUT)
        .is_err());
    assert_eq!(fixture.calls("describe").len(), 1);
    assert_eq!(fixture.calls("invoke").len(), 0);
}
