//! Real service/Provider calls retire details while retaining a producer high-water mark.

use super::*;
use aw_service::{Binding, EventHandle};

fn register(binding: &Binding, sequence: u64, value: &Value) -> Operation {
    Operation::RegisterInvocation {
        instance_id: binding.instance_id.clone(),
        sequence,
        call_id: value["tool"]["call_id"].as_str().unwrap().into(),
        tool_name: value["tool"]["native_name"].as_str().unwrap().into(),
        input: value["tool"]["input"].clone(),
    }
}

fn finish(binding: &Binding, sequence: u64, call: &str) -> Operation {
    Operation::FinishInvocation {
        instance_id: binding.instance_id.clone(),
        sequence,
        call_id: call.into(),
    }
}

fn hook(binding: &Binding, value: Value) -> Operation {
    Operation::OpenHookEvent {
        instance_id: binding.instance_id.clone(),
        event: value,
        native_input: b"{}".to_vec(),
    }
}

#[test]
fn producer_calls_reclaim_both_phases_and_reject_late_or_changed_callbacks() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &fixture.document());
    let binding = fixture.bind(&service.client);
    service
        .client
        .call(
            Operation::OwnInstance {
                instance_id: binding.instance_id.clone(),
                pid: std::process::id(),
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    let call = |operation| service.client.call(operation, Instant::now() + TIMEOUT);
    call(Operation::EnableInvocations {
        instance_id: binding.instance_id.clone(),
    })
    .unwrap();
    let mut slow = event(&binding, "tool.before", "allow");
    slow["tool"]["call_id"] = json!("slow");
    call(register(&binding, 1, &slow)).unwrap();
    for sequence in 2..26 {
        let id = format!("call-{sequence}");
        let mut before = event(&binding, "tool.before", "allow");
        before["tool"]["call_id"] = json!(id);
        assert!(call(hook(&binding, before.clone())).is_err());
        call(register(&binding, sequence, &before)).unwrap();
        assert!(call(register(&binding, sequence, &before)).is_err());
        let mut changed = before.clone();
        changed["tool"]["input"] = json!({"changed":true});
        assert!(call(hook(&binding, changed)).is_err());
        let handle: EventHandle =
            serde_json::from_value(call(hook(&binding, before.clone())).unwrap()).unwrap();
        call(invoke(&handle, "check")).unwrap();
        assert!(call(invoke(&handle, "check")).is_err());
        let mut after = event(&binding, "tool.after", "observe");
        after["tool"]["call_id"] = json!(id);
        let completed: EventHandle =
            serde_json::from_value(call(hook(&binding, after.clone())).unwrap()).unwrap();
        call(invoke(&completed, "record")).unwrap();
        call(finish(&binding, sequence, &id)).unwrap();
        assert!(call(hook(&binding, before)).is_err());
        assert!(call(hook(&binding, after)).is_err());
        assert!(call(Operation::CloseHookEvent {
            instance_id: binding.instance_id.clone(),
            event: "tool.before".into(),
            session_id: "native-session".into(),
            call_id: id
        })
        .is_err());
        let status = call(Operation::Status).unwrap();
        assert_eq!(status["resources"]["usage"]["correlations"], 0);
        assert_eq!(status["resources"]["usage"]["invocations"], 1);
        assert_eq!(status["events"], 0);
    }
    // A slow older identity can still execute after thousands of later
    // registrations in the unit test and after real workers retire here.
    let handle: EventHandle = serde_json::from_value(call(hook(&binding, slow)).unwrap()).unwrap();
    call(invoke(&handle, "check")).unwrap();
    call(finish(&binding, 1, "slow")).unwrap();
    assert_eq!(fixture.calls("invoke").len(), 49);
    assert_eq!(
        call(Operation::Status).unwrap()["resources"]["usage"]["invocations"],
        0
    );
    fixture.assert_reaped();
}

#[test]
fn invocation_overload_is_observable_and_recovers_without_replaying_old_calls() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["daemon"]["limits"] = json!({"invocations_per_binding":1});
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let call = |operation| service.client.call(operation, Instant::now() + TIMEOUT);
    call(Operation::OwnInstance {
        instance_id: binding.instance_id.clone(),
        pid: std::process::id(),
    })
    .unwrap();
    call(Operation::EnableInvocations {
        instance_id: binding.instance_id.clone(),
    })
    .unwrap();
    let first = event(&binding, "tool.before", "allow");
    assert!(call(Operation::OpenEvent {
        instance_id: binding.instance_id.clone(),
        event: first.clone()
    })
    .unwrap_err()
    .to_string()
    .contains("invocation_correlated_event_required"));
    call(register(&binding, 1, &first)).unwrap();
    let mut second = first.clone();
    second["tool"]["call_id"] = json!("second");
    assert!(call(register(&binding, 2, &second))
        .unwrap_err()
        .to_string()
        .contains("invocation_limit"));
    assert!(call(hook(&binding, second.clone())).is_err());
    call(finish(&binding, 1, "native-call")).unwrap();
    assert!(call(register(&binding, 2, &second)).is_err());
    call(register(&binding, 3, &second)).unwrap();
    let handle: EventHandle =
        serde_json::from_value(call(hook(&binding, second.clone())).unwrap()).unwrap();
    call(invoke(&handle, "check")).unwrap();
    call(finish(&binding, 3, "second")).unwrap();
    let status = call(Operation::Status).unwrap();
    assert_eq!(status["resources"]["usage"]["invocations"], 0);
    assert_eq!(status["resources"]["usage"]["correlations"], 0);
    assert_eq!(status["resources"]["peak_invocations_per_binding"], 1);
    assert_eq!(
        status["resources"]["rejection_reasons"]["invocation_limit"],
        1
    );
    call(Operation::ReleaseInstance {
        instance_id: binding.instance_id.clone(),
    })
    .unwrap();
    assert!(call(register(&binding, 4, &second)).is_err());
    assert!(call(hook(&binding, second)).is_err());
    assert_eq!(fixture.calls("invoke").len(), 1);
    fixture.assert_reaped();
}

#[test]
fn saturated_connections_reserve_capacity_for_invocation_drain_and_status() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["daemon"]["limits"] = json!({"connections":1});
    let service = Running::start(&fixture, &document);
    let binding = fixture.bind(&service.client);
    let call = |operation| service.client.call(operation, Instant::now() + TIMEOUT);
    call(Operation::OwnInstance {
        instance_id: binding.instance_id.clone(),
        pid: std::process::id(),
    })
    .unwrap();
    call(Operation::EnableInvocations {
        instance_id: binding.instance_id.clone(),
    })
    .unwrap();
    let first = event(&binding, "tool.before", "allow");
    call(register(&binding, 1, &first)).unwrap();
    let held = UnixStream::connect(&service.socket).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let status = call(Operation::Status).unwrap();
        if status["resources"]["usage"]["connections"]
            .as_u64()
            .unwrap()
            >= 2
        {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let mut second = first.clone();
    second["tool"]["call_id"] = json!("second");
    assert!(call(register(&binding, 2, &second))
        .unwrap_err()
        .to_string()
        .contains("connection_limit"));
    call(finish(&binding, 1, "native-call")).unwrap();
    let status = call(Operation::Status).unwrap();
    assert_eq!(status["resources"]["usage"]["invocations"], 0);
    assert_eq!(
        status["resources"]["rejection_reasons"]["connection_limit"],
        1
    );
    drop(held);
    // Connection rejection happened before producer registration: the next
    // unused sequence remains available once the saturated connection closes.
    loop {
        let status = call(Operation::Status).unwrap();
        if status["resources"]["usage"]["connections"] == 1 {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    call(register(&binding, 2, &second)).unwrap();
    call(finish(&binding, 2, "second")).unwrap();
    assert!(fixture.calls("invoke").is_empty());
    fixture.assert_reaped();
}
