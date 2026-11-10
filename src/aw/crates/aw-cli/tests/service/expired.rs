//! Expiry before dispatch preserves policy failure actions without keeping workers alive.

use crate::common::{audit, close, event, invoke, open, second_step, Fixture, Running, TIMEOUT};
use aw_service::{Binding, Client, EventHandle, Operation};
use serde_json::{json, Value};
use std::{
    thread,
    time::{Duration, Instant},
};

fn deadline() -> Instant {
    Instant::now() + TIMEOUT
}
fn expired(client: &Client, handle: &EventHandle) -> Value {
    let until = deadline();
    loop {
        let evidence = audit(client, &handle.event_id);
        if evidence["terminal"] == true {
            assert_eq!(
                evidence["records"].as_array().unwrap().last().unwrap()["record"]["status"],
                "expired"
            );
            // Registry retirement follows the durable terminal write.
            while client.call(Operation::Status, deadline()).unwrap()["events"] != 0 {
                assert!(Instant::now() < until);
                thread::sleep(Duration::from_millis(1));
            }
            return evidence;
        }
        assert!(Instant::now() < until, "event did not expire");
        thread::sleep(Duration::from_millis(1));
    }
}
fn unknown(result: Result<Value, aw_service::Error>) {
    assert!(matches!(result, Err(aw_service::Error::Rejected(code)) if code == "unknown_event"));
}

#[test]
fn expired_events_preserve_event_and_default_budget_failure_actions() {
    for level in ["event", "default"] {
        for action in ["block", "report"] {
            let fixture = Fixture::new();
            let mut document = fixture.document();
            if level == "event" {
                document["spec"]["events"]["tool.before"]["budget_ms"] = json!(1);
            } else {
                document["spec"]["execution"]["default_event_budget_ms"] = json!(1);
            }
            document["spec"]["events"]["tool.before"]["steps"][0]["on_error"] = json!(action);
            let service = Running::start(&fixture, &document);
            let binding = fixture.bind(&service.client);
            let handle = open(
                &service.client,
                &binding,
                event(&binding, "tool.before", "allow"),
            );
            let evidence = expired(&service.client, &handle);
            let result = service
                .client
                .call(invoke(&handle, "check"), deadline())
                .unwrap();
            assert_eq!(result["status"], "error");
            assert_eq!(result["failure"], "transport");
            assert_eq!(result["failure_detail"], "deadline_exceeded");
            assert_eq!(result["failure_action"], action);
            assert_eq!(result["effects"], json!([]));
            assert_eq!(result["call"]["process"], Value::Null);
            let terminal = &evidence["records"].as_array().unwrap().last().unwrap()["record"];
            assert_eq!(terminal["expired_failure_actions"]["check"], action);
            assert!(!evidence["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["record"]["phase"] == "started"));
            assert!(fixture.calls("invoke").is_empty());
            assert!(
                service
                    .client
                    .call(invoke(&handle, "check"), deadline())
                    .is_err(),
                "expired attempts remain once-only"
            );
            assert_eq!(close(&service.client, &handle)["closed"], true);
            unknown(service.client.call(invoke(&handle, "check"), deadline()));
        }
    }
}

#[test]
fn expired_handles_are_bounded_instance_scoped_and_reclaimed_by_close_or_unbind() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["daemon"]["limits"] = json!({"events":1});
    document["spec"]["events"]["tool.before"]["budget_ms"] = json!(1);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let other = fixture.bind(&service.client);
    let handle = open(
        &service.client,
        &binding,
        event(&binding, "tool.before", "allow"),
    );
    expired(&service.client, &handle);
    let usage =
        service.client.call(Operation::Status, deadline()).unwrap()["resources"]["usage"].clone();
    assert_eq!(usage["event_slots"], 1);
    assert_eq!(usage["expired_events"], 1);
    let mut wrong = handle.clone();
    wrong.instance_id = other.instance_id.clone();
    unknown(service.client.call(invoke(&wrong, "check"), deadline()));
    unknown(service.client.call(
        Operation::CloseEvent {
            event_id: handle.event_id.clone(),
            instance_id: other.instance_id.clone(),
        },
        deadline(),
    ));
    assert!(service
        .client
        .call(invoke(&handle, "absent"), deadline())
        .is_err());
    let request = || Operation::OpenEvent {
        instance_id: binding.instance_id.clone(),
        event: event(&binding, "tool.before", "allow"),
    };
    assert!(
        matches!(service.client.call(request(), deadline()), Err(aw_service::Error::Rejected(code)) if code == "event_limit")
    );
    close(&service.client, &handle);
    let next = open(
        &service.client,
        &binding,
        event(&binding, "tool.before", "allow"),
    );
    expired(&service.client, &next);
    service
        .client
        .call(
            Operation::Unbind {
                instance_id: binding.instance_id.clone(),
            },
            deadline(),
        )
        .unwrap();
    unknown(service.client.call(invoke(&next, "check"), deadline()));
    let remaining = open(
        &service.client,
        &other,
        event(&other, "tool.before", "allow"),
    );
    expired(&service.client, &remaining);
    close(&service.client, &remaining);
}

fn native(client: &Client, binding: &Binding, call: &str) -> (EventHandle, Value) {
    let mut value = event(binding, "tool.before", "allow");
    value["tool"]["call_id"] = json!(call);
    let handle = serde_json::from_value(
        client
            .call(
                Operation::OpenHookEvent {
                    instance_id: binding.instance_id.clone(),
                    event: value.clone(),
                    native_input: b"{}".to_vec(),
                },
                deadline(),
            )
            .unwrap(),
    )
    .unwrap();
    (handle, value)
}

#[test]
fn expired_native_results_and_correlated_close_release_retained_capacity() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["daemon"]["limits"] = json!({"events":1});
    document["spec"]["events"]["tool.before"]["budget_ms"] = json!(100);
    second_step(&mut document);
    document["spec"]["providers"]["raw"] = json!({"protocol":"native-hook/v1alpha1", "config":{},
        "timeout_ms":5000, "max_output_bytes":1024,
        "transport":{"type":"stdio","location":"agent","argv":["/usr/bin/touch",fixture.0.join("native-spawned")]}});
    document["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"raw","provider":"raw","native":{},"on_error":"block"}));
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    for index in 0..4 {
        let (handle, value) = native(&service.client, &binding, &format!("call-{index}"));
        expired(&service.client, &handle);
        if index % 2 == 0 {
            for step in ["check", "second", "raw"] {
                let result = service
                    .client
                    .call(invoke(&handle, step), deadline())
                    .unwrap();
                assert_eq!(result["status"], "error");
            }
            unknown(service.client.call(invoke(&handle, "check"), deadline()));
        } else {
            service
                .client
                .call(
                    Operation::CloseHookEvent {
                        instance_id: binding.instance_id.clone(),
                        event: "tool.before".into(),
                        session_id: value["session_id"].as_str().unwrap().into(),
                        call_id: value["tool"]["call_id"].as_str().unwrap().into(),
                    },
                    deadline(),
                )
                .unwrap();
            unknown(service.client.call(invoke(&handle, "check"), deadline()));
        }
    }
    assert!(fixture.calls("invoke").is_empty());
    assert!(!fixture.0.join("native-spawned").exists());
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
