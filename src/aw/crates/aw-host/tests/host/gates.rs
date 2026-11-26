//! Lifecycle events and turn-stop decision gates share Host budgets and claims.

use super::*;
use aw_host::StepOutput;
use aw_provider::GateKind;
use std::path::Path;

fn gate_capabilities() -> aw_core::admission::AdapterCapabilities {
    let mut caps = capabilities();
    caps.events
        .insert("session.start".into(), vec!["observe".into()]);
    caps.events
        .insert("session.end".into(), vec!["observe".into()]);
    caps.events
        .insert("turn.stop".into(), vec!["observe".into(), "block".into()]);
    caps
}

fn gate_document(fixture: &Fixture) -> serde_json::Value {
    let mut document = fixture.document();
    document["spec"]["events"]["session.start"] = json!({
        "enabled": true,
        "steps": [{
            "id": "start-observe",
            "provider": "policy",
            "operation": "lifecycle",
            "effects": ["observe"],
            "on_error": "report"
        }]
    });
    document["spec"]["events"]["session.end"] = json!({
        "enabled": true,
        "steps": [{
            "id": "end-observe",
            "provider": "policy",
            "operation": "lifecycle",
            "effects": ["observe"],
            "on_error": "report"
        }]
    });
    document["spec"]["events"]["turn.stop"] = json!({
        "enabled": true,
        "steps": [
            {
                "id": "stop-first",
                "provider": "policy",
                "operation": "stop",
                "effects": ["observe", "block"],
                "on_error": "block"
            },
            {
                "id": "stop-second",
                "provider": "policy",
                "operation": "stop_second",
                "effects": ["observe"],
                "on_error": "report"
            }
        ]
    });
    document
}

fn lifecycle_payload(name: &str, scenario: &str) -> serde_json::Value {
    json!({
        "name": name,
        "agent": {"adapter": "qoder", "binding_id": "target", "instance_id": "inst-lifecycle"},
        "session_id": "sess-lifecycle",
        "native": {"scenario": scenario, "marker": "lifecycle"}
    })
}

fn turn_stop_payload(scenario: &str) -> serde_json::Value {
    json!({
        "name": "turn.stop",
        "agent": {"adapter": "qoder", "binding_id": "target", "instance_id": "inst-stop"},
        "session_id": "sess-stop",
        "turn_id": "turn-stop-1",
        "native": {"scenario": scenario}
    })
}

#[test]
fn lifecycle_events_observe_once_and_never_escalate_to_blocking() {
    let fixture = Fixture::new();
    let document = gate_document(&fixture);
    let host = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        "target",
        gate_capabilities(),
        fixture.context(),
        Instant::now() + TIMEOUT,
        &AtomicBool::new(false),
    )
    .unwrap();
    let cancelled = AtomicBool::new(false);

    for (name, step_id) in [
        ("session.start", "start-observe"),
        ("session.end", "end-observe"),
    ] {
        let payload = lifecycle_payload(name, "observe");
        let bound = host
            .hook_event(
                payload.clone(),
                b"{\"native\":true}".to_vec(),
                Instant::now() + TIMEOUT,
                &cancelled,
            )
            .unwrap();
        assert_eq!(bound.gate().event(), name);
        assert_eq!(bound.gate().kind(), GateKind::Lifecycle);
        assert!(bound.input_view(json!({}), b"{}".to_vec()).is_err());
        assert!(bound
            .completion_view(payload.clone(), b"{}".to_vec())
            .is_err());

        let invocation = bound.invoke(step_id).unwrap();
        assert_eq!(invocation.failure_action, None);
        let StepOutput::Provider(outcome) = invocation.result.unwrap() else {
            panic!("expected structured Provider outcome for {name}");
        };
        assert!(!outcome.requests_block());
        assert_eq!(
            outcome.as_value()["effects"],
            json!([{"type": "observe", "reason_code": "fixture_policy"}])
        );
        // Once-only step claim holds across lifecycle events.
        assert!(bound.invoke(step_id).is_err());

        let observed = fixture.call(&invocation.record.request_id);
        assert_eq!(
            observed["request"]["event"],
            fixture.provider_event(&payload)
        );
        assert!(observed["request"]["event"].get("tool").is_none());
        assert!(observed["request"]["event"].get("turn_id").is_none());

        // A Provider attempting to return `block` on a lifecycle event is a protocol
        // failure and can only produce `FailureAction::Report`.
        let forged = host
            .event(
                lifecycle_payload(name, "block"),
                Instant::now() + TIMEOUT,
                &cancelled,
            )
            .unwrap()
            .invoke(step_id)
            .unwrap();
        assert!(matches!(forged.result, Err(Failure::Protocol(_))));
        assert_eq!(forged.failure_action, Some(FailureAction::Report));
    }
    fixture.assert_reaped();
}

#[test]
fn turn_stop_decision_gate_enforces_claims_cancellation_and_deadlines() {
    let fixture = Fixture::new();
    let document = gate_document(&fixture);
    let host = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        "target",
        gate_capabilities(),
        fixture.context(),
        Instant::now() + TIMEOUT,
        &AtomicBool::new(false),
    )
    .unwrap();
    let cancelled = AtomicBool::new(false);

    let payload = turn_stop_payload("block");
    let bound = host
        .hook_event(
            payload.clone(),
            b"{\"native\":true}".to_vec(),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    assert_eq!(bound.gate().event(), "turn.stop");
    assert_eq!(bound.gate().kind(), GateKind::Decision);
    assert!(bound.input_view(json!({}), b"{}".to_vec()).is_err());
    assert!(bound
        .completion_view(payload.clone(), b"{}".to_vec())
        .is_err());
    assert_eq!(
        bound
            .steps()
            .map(|step| step.step_id.as_str())
            .collect::<Vec<_>>(),
        ["stop-first", "stop-second"]
    );

    let first = bound.invoke("stop-first").unwrap();
    let StepOutput::Provider(outcome) = first.result.unwrap() else {
        panic!("expected turn.stop Provider outcome");
    };
    assert!(outcome.requests_block());
    assert!(bound.invoke("stop-first").is_err());

    // Second step is observe-only; a forged block from the Provider fails with Report.
    let second = bound.invoke("stop-second").unwrap();
    assert!(matches!(second.result, Err(Failure::Protocol(_))));
    assert_eq!(second.failure_action, Some(FailureAction::Report));

    let observed = fixture.call(&first.record.request_id);
    assert_eq!(
        observed["request"]["event"],
        fixture.provider_event(&payload)
    );
    assert!(observed["request"]["event"].get("tool").is_none());
    assert!(observed["request"]["event"].get("input").is_none());

    // Pre-spawn cancellation and expired budgets preserve the configured failure action.
    let cancelled_event = host
        .event(
            turn_stop_payload("observe"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    cancelled.store(true, Ordering::Release);
    let attempt = cancelled_event.invoke("stop-first").unwrap();
    assert!(matches!(
        attempt.result,
        Err(Failure::Transport(aw_exec::Error::Cancelled))
    ));
    assert_eq!(attempt.failure_action, Some(FailureAction::Block));
    cancelled.store(false, Ordering::Release);

    let expired = host
        .event(
            turn_stop_payload("observe"),
            Instant::now() + Duration::from_millis(5),
            &cancelled,
        )
        .unwrap();
    thread::sleep(Duration::from_millis(10));
    let late = expired.invoke("stop-first").unwrap();
    assert!(matches!(
        late.result,
        Err(Failure::Transport(aw_exec::Error::DeadlineExceeded))
    ));
    assert_eq!(late.failure_action, Some(FailureAction::Block));

    // Projected-out tool events retain empty-step compatibility on standalone Hosts
    // even when the selected target's capabilities omit that projected-out event.
    let mut projected_doc = fixture.document();
    projected_doc["spec"]["agents"]["openclaw"] =
        json!({"adapter": "openclaw", "argv": ["/no-agent-is-started"]});
    projected_doc["spec"]["events"]["tool.before"]["steps"][0]["match"] =
        json!({"tools": ["native:openclaw:exec"]});
    projected_doc["spec"]["events"]["tool.after"]["steps"] = json!([]);
    let mut after_only = capabilities();
    after_only.events.remove("tool.before");
    let projected_host = Host::prepare(
        &serde_json::to_vec(&projected_doc).unwrap(),
        "target",
        after_only,
        fixture.context(),
        Instant::now() + TIMEOUT,
        &cancelled,
    )
    .unwrap();
    let projected_event = projected_host
        .event(
            event("tool.before", "allow"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    assert_eq!(projected_event.steps().count(), 0);
    assert!(matches!(
        projected_host.event(
            lifecycle_payload("session.end", "observe"),
            Instant::now() + TIMEOUT,
            &cancelled
        ),
        Err(Error::Invalid("event was not enabled and admitted"))
    ));
    assert!(matches!(
        projected_host.event(
            json!({
                "name": "model.before_request",
                "agent": {"adapter": "qoder", "binding_id": "target", "instance_id": null},
                "session_id": "sess",
                "native": {}
            }),
            Instant::now() + TIMEOUT,
            &cancelled
        ),
        Err(Error::Invalid("event was not enabled and admitted"))
    ));

    fixture.assert_reaped();
}

#[test]
fn turn_stop_shares_one_event_budget_across_steps_and_cancels_in_flight() {
    let fixture = Fixture::new();
    let mut document = gate_document(&fixture);
    document["spec"]["providers"]["policy"]["config"]["delays_ms"] =
        json!({"stop": 30, "stop_second": 800});
    document["spec"]["events"]["turn.stop"]["budget_ms"] = json!(2500);
    let host = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        "target",
        gate_capabilities(),
        fixture.context(),
        Instant::now() + TIMEOUT,
        &AtomicBool::new(false),
    )
    .unwrap();
    let cancelled = AtomicBool::new(false);

    // Two sequential steps on the same `turn.stop` Event share one depleting budget.
    let shared = host
        .event(
            turn_stop_payload("observe"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    let first = shared.invoke("stop-first").unwrap();
    assert!(first.result.is_ok());
    let second = shared.invoke("stop-second").unwrap();
    assert!(second.result.is_ok());
    let first_budget = fixture.call(&first.record.request_id)["request"]["budget_ms"]
        .as_u64()
        .unwrap();
    let second_budget = fixture.call(&second.record.request_id)["request"]["budget_ms"]
        .as_u64()
        .unwrap();
    assert!(
        second_budget < first_budget,
        "expected depleted shared budget: first={first_budget}, second={second_budget}"
    );

    // Step 1 retains startup margin while step 2 deliberately outlasts the remaining budget.
    let tight_deadline = Instant::now() + Duration::from_millis(450);
    let tight = host
        .event(turn_stop_payload("observe"), tight_deadline, &cancelled)
        .unwrap();
    assert!(tight.invoke("stop-first").unwrap().result.is_ok());
    let exhausted = tight.invoke("stop-second").unwrap();
    assert!(Instant::now() < tight_deadline + CLEANUP_ALLOWANCE);
    assert!(matches!(
        exhausted.result,
        Err(Failure::Transport(aw_exec::Error::DeadlineExceeded))
    ));
    assert_eq!(exhausted.failure_action, Some(FailureAction::Report));

    // Mid-flight cancellation terminates the running Provider process and preserves `Block`.
    let before_calls = fixture.calls("invoke").len();
    let mut waiting = turn_stop_payload("wait");
    waiting["native"]["release"] = json!("never");
    let active = AtomicBool::new(false);
    thread::scope(|scope| {
        let canceller = scope.spawn(|| {
            assert!(fixture.wait_for_calls(
                "invoke",
                before_calls + 1,
                Instant::now() + Duration::from_secs(3)
            ));
            active.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let in_flight = host
            .event(waiting, Instant::now() + TIMEOUT, &active)
            .unwrap()
            .invoke("stop-first")
            .unwrap();
        canceller.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3) + CLEANUP_ALLOWANCE);
        assert!(matches!(
            in_flight.result,
            Err(Failure::Transport(aw_exec::Error::Cancelled))
        ));
        assert_eq!(in_flight.failure_action, Some(FailureAction::Block));
    });

    fixture.assert_reaped();
}

#[test]
fn boolean_hooks_project_lifecycle_and_turn_stop_effects_without_handshake() {
    let fixture = Fixture::new();
    let mut document = gate_document(&fixture);
    let provider = &mut document["spec"]["providers"]["policy"];
    provider.as_object_mut().unwrap().remove("protocol");
    provider.as_object_mut().unwrap().remove("config");
    provider["transport"]["argv"] = json!([
        provider["transport"]["argv"][0],
        "-I",
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/boolean_hook.py"),
        fixture.0,
        LITERAL_ARGUMENT
    ]);
    for name in [
        "tool.before",
        "tool.after",
        "session.start",
        "session.end",
        "turn.stop",
    ] {
        for step in document["spec"]["events"][name]["steps"]
            .as_array_mut()
            .unwrap()
        {
            step.as_object_mut().unwrap().remove("operation");
        }
    }
    document["spec"]["events"]["turn.stop"]["steps"][0]["reason_code"] =
        json!("turn_stop_matched_12345");

    let host = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        "target",
        gate_capabilities(),
        fixture.context(),
        Instant::now() + TIMEOUT,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(host.preparation().is_empty());
    let cancelled = AtomicBool::new(false);

    let mut start = lifecycle_payload("session.start", "normal");
    start["native"]["token"] = json!("12345");
    let start_call = host
        .event(start.clone(), Instant::now() + TIMEOUT, &cancelled)
        .unwrap()
        .invoke("start-observe")
        .unwrap();
    let StepOutput::Provider(start_outcome) = start_call.result.unwrap() else {
        panic!("expected boolean hook lifecycle outcome");
    };
    assert!(!start_outcome.requests_block());
    assert_eq!(
        start_outcome.as_value()["effects"],
        json!([{"type": "observe"}])
    );

    for matched in [false, true] {
        let mut stop = turn_stop_payload("normal");
        stop["native"]["check"] = json!(if matched { "hit-12345" } else { "clean" });
        let stop_call = host
            .event(stop, Instant::now() + TIMEOUT, &cancelled)
            .unwrap()
            .invoke("stop-first")
            .unwrap();
        let StepOutput::Provider(stop_outcome) = stop_call.result.unwrap() else {
            panic!("expected boolean hook turn.stop outcome");
        };
        assert_eq!(stop_outcome.requests_block(), matched);
    }

    fixture.assert_reaped();
}
