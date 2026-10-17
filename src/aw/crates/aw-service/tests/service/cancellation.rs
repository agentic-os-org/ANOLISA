//! Correlation cleanup fences delayed opens and joins active Provider children.

use crate::common::{audit, event, invoke, second_step, Fixture, Running, TIMEOUT};
use aw_service::{Binding, Client, EventHandle, Operation};
use serde_json::{json, Value};
use std::{
    thread,
    time::{Duration, Instant},
};

fn cancel(binding: &Binding, value: &Value) -> Operation {
    Operation::CloseHookEvent {
        instance_id: binding.instance_id.clone(),
        event: value["name"].as_str().unwrap().into(),
        session_id: value["session_id"].as_str().unwrap().into(),
        call_id: value["tool"]["call_id"].as_str().unwrap().into(),
    }
}

fn open(binding: &Binding, value: &Value) -> Operation {
    Operation::OpenHookEvent {
        instance_id: binding.instance_id.clone(),
        event: value.clone(),
        native_input: b"{}".to_vec(),
    }
}

fn status(client: &Client) -> Value {
    client
        .call(Operation::Status, Instant::now() + TIMEOUT)
        .unwrap()
}

#[test]
fn cancellation_before_open_is_idempotent_and_scoped_to_the_native_correlation() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let binding = fixture.bind(&service.client);
    let value = event(&binding, "tool.before", "allow");
    for _ in 0..2 {
        service
            .client
            .call(cancel(&binding, &value), Instant::now() + TIMEOUT)
            .unwrap();
    }
    assert!(service
        .client
        .call(open(&binding, &value), Instant::now() + TIMEOUT)
        .is_err());
    assert_eq!(status(&service.client)["events"], 0);
    let other_binding = fixture.bind(&service.client);
    let mut other_session = value.clone();
    other_session["session_id"] = json!("other-session");
    let others = [
        (&binding, other_session),
        (&binding, event(&binding, "tool.after", "allow")),
        (
            &other_binding,
            event(&other_binding, "tool.before", "allow"),
        ),
    ];
    for (binding, value) in others {
        let handle: EventHandle = serde_json::from_value(
            service
                .client
                .call(open(binding, &value), Instant::now() + TIMEOUT)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(handle.instance_id, binding.instance_id);
        service
            .client
            .call(cancel(binding, &value), Instant::now() + TIMEOUT)
            .unwrap();
    }
    assert_eq!(status(&service.client)["events"], 0);
}

#[test]
fn cancelling_nonfinal_events_releases_capacity_without_renewing_their_claims() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["execution"]["default_event_budget_ms"] = json!(55000);
    second_step(&mut document);
    let mut last = document["spec"]["events"]["tool.after"]["steps"][0].clone();
    last["id"] = json!("last");
    document["spec"]["events"]["tool.after"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(last);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    for name in ["tool.before", "tool.after"] {
        for number in 0..34 {
            let mut value = event(&binding, name, "allow");
            value["tool"]["call_id"] = json!(format!("cancel-{number}"));
            let handle: EventHandle = serde_json::from_value(
                service
                    .client
                    .call(open(&binding, &value), Instant::now() + TIMEOUT)
                    .unwrap(),
            )
            .unwrap();
            let step = if name == "tool.before" {
                "check"
            } else {
                "record"
            };
            assert_eq!(
                service
                    .client
                    .call(invoke(&handle, step), Instant::now() + TIMEOUT)
                    .unwrap()["status"],
                "ok"
            );
            service
                .client
                .call(cancel(&binding, &value), Instant::now() + TIMEOUT)
                .unwrap();
            assert_eq!(status(&service.client)["events"], 0);
            assert_eq!(audit(&service.client, &handle.event_id)["terminal"], true);
            assert!(service
                .client
                .call(open(&binding, &value), Instant::now() + TIMEOUT)
                .is_err());
        }
    }
    fixture.assert_reaped();
}

#[test]
fn cancellation_joins_inflight_provider_before_acknowledging_close() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    second_step(&mut document);
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let value = event(&binding, "tool.before", "wait");
    let handle: EventHandle = serde_json::from_value(
        service
            .client
            .call(open(&binding, &value), Instant::now() + TIMEOUT)
            .unwrap(),
    )
    .unwrap();
    let client = service.client.clone();
    let running =
        thread::spawn(move || client.call(invoke(&handle, "check"), Instant::now() + TIMEOUT));
    fixture.wait_for_calls("invoke", 1);
    service
        .client
        .call(
            cancel(&binding, &value),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    assert_eq!(running.join().unwrap().unwrap()["status"], "error");
    assert_eq!(status(&service.client)["events"], 0);
    fixture.assert_reaped();
}
