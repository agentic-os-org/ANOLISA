//! Three-category gate execution contracts and newly admitted Hook envelopes.

use aw_provider::{EventGate, GateKind, MutationSlot, Protocol, Reply, VERSION};
use serde_json::{json, Value};

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn envelope(event: Value, allowed_effects: Value) -> Value {
    json!({
        "api_version": VERSION,
        "method": "invoke",
        "request_id": "req-gate-1",
        "operation": "invoke",
        "config_revision": "c".repeat(64),
        "budget_ms": 500,
        "allowed_effects": allowed_effects,
        "config": {},
        "event": event,
    })
}

fn response(digest: &Value, effects: Value) -> Value {
    json!({
        "api_version": VERSION,
        "request_id": "req-gate-1",
        "status": "ok",
        "input_digest": digest,
        "effects": effects,
    })
}

fn session_event(name: &str) -> Value {
    json!({
        "name": name,
        "agent": {"adapter": "qoder", "binding_id": "work", "instance_id": "inst-1"},
        "session_id": "sess-1",
        "native": {"source": "fixture"}
    })
}

fn turn_stop_event() -> Value {
    json!({
        "name": "turn.stop",
        "agent": {"adapter": "pi", "binding_id": "worker", "instance_id": "inst-2"},
        "session_id": "sess-2",
        "turn_id": "turn-7",
        "native": {"reason": "end_turn"}
    })
}

#[test]
fn gate_classification_covers_implemented_hooks_and_matches_config_rules() {
    assert_eq!(EventGate::ALL.len(), 7);
    for (event, kind, has_tool, native_hook, effects, actions) in [
        (
            "session.start",
            GateKind::Lifecycle,
            false,
            false,
            &["observe"][..],
            &["report"][..],
        ),
        (
            "session.end",
            GateKind::Lifecycle,
            false,
            false,
            &["observe"][..],
            &["report"][..],
        ),
        (
            "input.submit",
            GateKind::Decision,
            false,
            false,
            &["observe", "block"][..],
            &["report", "block"][..],
        ),
        (
            "turn.stop",
            GateKind::Decision,
            false,
            false,
            &["observe", "block"][..],
            &["report", "block"][..],
        ),
        (
            "security.violation",
            GateKind::Decision,
            true,
            false,
            &["observe", "block"][..],
            &["report", "block"][..],
        ),
        (
            "tool.before",
            GateKind::Mutation(MutationSlot::PreToolInput),
            true,
            true,
            &["observe", "block", "replace_input"][..],
            &["report", "block"][..],
        ),
        (
            "tool.after",
            GateKind::Mutation(MutationSlot::PostToolResult),
            true,
            true,
            &["observe", "replace_result"][..],
            &["report", "withhold_result"][..],
        ),
    ] {
        let gate = EventGate::for_event(event).unwrap();
        assert_eq!(gate.event(), event);
        assert_eq!(gate.kind(), kind);
        assert_eq!(gate.has_tool_payload(), has_tool);
        assert_eq!(gate.allows_native_hook(), native_hook);
        for candidate in ["observe", "block", "replace_input", "replace_result", "ask"] {
            assert_eq!(
                gate.allows_effect(candidate),
                effects.contains(&candidate),
                "{event}/{candidate}"
            );
        }
        for action in ["report", "block", "withhold_result", "unknown"] {
            assert_eq!(
                gate.allows_failure_action(action),
                actions.contains(&action),
                "{event}/{action}"
            );
        }
        assert_eq!(
            gate.allows_extra_failure_action("withhold_result"),
            event == "tool.after"
        );
    }

    for unimplemented in [
        "permission.request",
        "compact.before",
        "compact.after",
        "subagent.start",
        "subagent.stop",
        "model.before_request",
        "runtime.observed",
        "runtime.exited",
        "coverage.changed",
        "unknown.event",
    ] {
        assert!(
            EventGate::for_event(unimplemented).is_none(),
            "{unimplemented}"
        );
    }

    let validator = aw_config::Validator::new().unwrap();
    let base = validator
        .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    for gate in EventGate::ALL {
        for effect in ["observe", "block", "replace_input", "replace_result", "ask"] {
            let mut doc = base.clone();
            doc["spec"]["providers"]["p"] = json!({
                "protocol": VERSION,
                "transport": {"type": "stdio", "location": "agent", "argv": ["/absent"]},
                "timeout_ms": 100,
                "max_output_bytes": 1024,
                "config": {}
            });
            doc["spec"]["events"][gate.event()] = json!({
                "enabled": true,
                "steps": [{
                    "id": "step",
                    "provider": "p",
                    "operation": "op",
                    "effects": [effect],
                    "on_error": "report"
                }]
            });
            assert_eq!(
                validator.parse(&bytes(&doc)).is_ok(),
                gate.allows_effect(effect),
                "config parity for {}/{effect}",
                gate.event()
            );
        }
    }
}

#[test]
fn lifecycle_events_enforce_session_identity_and_forbid_control_effects() {
    let protocol = Protocol::new().unwrap();
    for name in ["session.start", "session.end"] {
        let ev = session_event(name);
        protocol.validate_event(&ev).unwrap();
        let req = protocol
            .bind_invocation(envelope(ev.clone(), json!(["observe"])))
            .unwrap();
        let hook = protocol
            .bind_hook_invocation(envelope(ev.clone(), json!(["observe"])))
            .unwrap();
        assert_eq!(
            req.as_value()["input_digest"],
            hook.as_value()["input_digest"]
        );

        for effects in [
            json!([]),
            json!([{"type": "observe"}]),
            json!([{"type": "observe", "reason_code": "session_recorded"}]),
        ] {
            let reply = response(&req.as_value()["input_digest"], effects);
            let Reply::Invocation(outcome) = protocol.check_response(&req, &bytes(&reply)).unwrap()
            else {
                panic!("expected invocation outcome")
            };
            assert!(!outcome.requests_block());
        }

        for disallowed in ["block", "replace_input", "replace_result", "ask"] {
            assert!(
                protocol
                    .bind_invocation(envelope(ev.clone(), json!([disallowed])))
                    .is_err(),
                "{name} request {disallowed}"
            );
            assert!(
                protocol
                    .bind_hook_invocation(envelope(ev.clone(), json!([disallowed])))
                    .is_err(),
                "{name} hook {disallowed}"
            );
        }

        for forged in [
            json!([{"type": "block"}]),
            json!([{"type": "replace_input", "input": {}}]),
            json!([{"type": "replace_result", "text": "forged"}]),
            json!([{"type": "observe", "text": "hidden"}]),
        ] {
            let reply = response(&req.as_value()["input_digest"], forged.clone());
            assert!(
                protocol.check_response(&req, &bytes(&reply)).is_err(),
                "{name} response {forged}"
            );
        }

        for invalid in [
            {
                let mut v = ev.clone();
                v["session_id"] = Value::Null;
                v
            },
            {
                let mut v = ev.clone();
                v["session_id"] = json!("");
                v
            },
            {
                let mut v = ev.clone();
                v["turn_id"] = json!("turn-1");
                v
            },
            {
                let mut v = ev.clone();
                v["input"] = json!({"text": "unexpected"});
                v
            },
            {
                let mut v = ev.clone();
                v["tool"] = json!({
                    "name": "bash", "native_name": "Bash", "call_id": null,
                    "input": {}, "result": null
                });
                v
            },
            {
                let mut v = ev.clone();
                v["unknown"] = json!(true);
                v
            },
        ] {
            assert!(protocol.validate_event(&invalid).is_err(), "{invalid}");
            assert!(protocol
                .bind_invocation(envelope(invalid, json!(["observe"])))
                .is_err());
        }
    }
}

#[test]
fn turn_stop_decision_gate_binds_session_and_turn_and_rejects_mutations() {
    let protocol = Protocol::new().unwrap();
    let ev = turn_stop_event();
    protocol.validate_event(&ev).unwrap();
    let req = protocol
        .bind_invocation(envelope(ev.clone(), json!(["observe", "block"])))
        .unwrap();
    let hook = protocol
        .bind_hook_invocation(envelope(ev.clone(), json!(["observe", "block"])))
        .unwrap();
    assert_eq!(
        req.as_value()["input_digest"],
        hook.as_value()["input_digest"]
    );

    for field in ["session_id", "turn_id"] {
        let mut tampered = req.as_value().clone();
        tampered["event"][field] = json!("other-id");
        assert!(
            protocol.parse_request(&bytes(&tampered)).is_err(),
            "{field}"
        );
    }

    let block_reply = response(
        &req.as_value()["input_digest"],
        json!([{"type": "block", "reason_code": "incomplete_verification", "text": "Run tests before stopping"}]),
    );
    let Reply::Invocation(outcome) = protocol.check_response(&req, &bytes(&block_reply)).unwrap()
    else {
        panic!("expected block outcome")
    };
    assert!(outcome.requests_block());

    let allow_reply = response(&req.as_value()["input_digest"], json!([]));
    let Reply::Invocation(outcome) = protocol.check_response(&req, &bytes(&allow_reply)).unwrap()
    else {
        panic!("expected neutral outcome")
    };
    assert!(!outcome.requests_block());

    for disallowed in ["replace_input", "replace_result", "ask"] {
        assert!(protocol
            .bind_invocation(envelope(ev.clone(), json!([disallowed])))
            .is_err());
    }
    for forged in [
        json!([{"type": "replace_input", "input": {}}]),
        json!([{"type": "replace_result", "text": "forged"}]),
    ] {
        let reply = response(&req.as_value()["input_digest"], forged);
        assert!(protocol.check_response(&req, &bytes(&reply)).is_err());
    }

    for invalid in [
        {
            let mut v = ev.clone();
            v["session_id"] = Value::Null;
            v
        },
        {
            let mut v = ev.clone();
            v.as_object_mut().unwrap().remove("turn_id");
            v
        },
        {
            let mut v = ev.clone();
            v["turn_id"] = json!("");
            v
        },
        {
            let mut v = ev.clone();
            v["input"] = json!({"text": "not-an-input-event"});
            v
        },
        {
            let mut v = ev;
            v["tool"] = json!({
                "name": "bash", "native_name": "Bash", "call_id": null,
                "input": {}, "result": null
            });
            v
        },
    ] {
        assert!(protocol.validate_event(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn unimplemented_vocabulary_events_remain_describable_but_not_invocable() {
    let protocol = Protocol::new().unwrap();
    let describe = protocol
        .parse_request(&bytes(&json!({
            "api_version": VERSION, "method": "describe", "request_id": "d-1"
        })))
        .unwrap();
    let reply = json!({
        "api_version": VERSION,
        "request_id": "d-1",
        "status": "ok",
        "operations": [{
            "name": "future",
            "events": ["session.start", "turn.stop", "session.end", "model.before_request"],
            "effects": ["observe", "block"]
        }]
    });
    assert!(matches!(
        protocol.check_response(&describe, &bytes(&reply)),
        Ok(Reply::Description(_))
    ));

    for unimplemented in [
        "model.before_request",
        "permission.request",
        "runtime.observed",
    ] {
        let ev = json!({
            "name": unimplemented,
            "agent": {"adapter": "qoder", "binding_id": "work", "instance_id": null},
            "session_id": "sess-1",
            "native": {}
        });
        assert!(protocol.validate_event(&ev).is_err(), "{unimplemented}");
        assert!(protocol
            .bind_invocation(envelope(ev, json!(["observe"])))
            .is_err());
    }
}
