//! Invoke-only hooks use YAML effects while retaining Host budgets and failure rules.
#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "support/fixture.rs"]
mod fixture;

use aw_host::{Failure, FailureAction, Host, Method, StepOutput};
use fixture::{capabilities, event, Fixture, LITERAL_ARGUMENT, TIMEOUT};
use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

fn document(fixture: &Fixture) -> Value {
    let mut value = fixture.document();
    let provider = &mut value["spec"]["providers"]["policy"];
    provider.as_object_mut().unwrap().remove("protocol");
    provider.as_object_mut().unwrap().remove("config");
    provider["transport"]["argv"][2] =
        json!(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/boolean_hook.py"));
    provider["transport"]["argv"]
        .as_array_mut()
        .unwrap()
        .remove(4);
    for name in ["tool.before", "tool.after"] {
        value["spec"]["events"][name]["steps"][0]
            .as_object_mut()
            .unwrap()
            .remove("operation");
    }
    value
}

#[test]
fn default_and_explicit_hooks_skip_handshake_and_project_each_steps_effects() {
    for explicit in [false, true] {
        let fixture = Fixture::new();
        let mut value = document(&fixture);
        if explicit {
            value["spec"]["providers"]["policy"]["protocol"] = json!(aw_config::HOOK_PROTOCOL);
            value["spec"]["providers"]["policy"]["config"] = json!({});
            for name in ["tool.before", "tool.after"] {
                value["spec"]["events"][name]["steps"][0]["operation"] = json!("invoke");
            }
        }
        value["spec"]["events"]["tool.before"]["steps"][0]["reason_code"] =
            json!("parameter_contains_12345");
        let host = fixture.prepare(&value);
        assert!(host.preparation().is_empty());
        assert!(!fixture.0.join("hook-event.json").exists());
        for (name, step, matched) in [
            ("tool.before", "check", false),
            ("tool.before", "check", true),
            ("tool.after", "record", true),
        ] {
            let cancelled = AtomicBool::new(false);
            let mut input = event(name, "normal");
            input["tool"]["input"] = if matched {
                json!({"nested":[{"value":"12345"}]})
            } else {
                json!({"value":12345})
            };
            let call = host
                .event(input.clone(), Instant::now() + TIMEOUT, &cancelled)
                .unwrap();
            let invocation = call.invoke(step).unwrap();
            assert_eq!(invocation.record.method, Method::Invoke);
            let StepOutput::Provider(outcome) = invocation.result.unwrap() else {
                panic!("expected projected effects")
            };
            assert_eq!(outcome.requests_block(), matched && name == "tool.before");
            let effects = if !matched {
                json!([])
            } else if name == "tool.before" {
                json!([{"type":"observe","reason_code":"parameter_contains_12345"},{"type":"block","reason_code":"parameter_contains_12345"}])
            } else {
                json!([{"type":"observe"}])
            };
            assert_eq!(outcome.as_value()["effects"], effects);
            assert!(outcome.as_value()["input_digest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:"));
            let observed: Value =
                serde_json::from_slice(&fs::read(fixture.0.join("hook-event.json")).unwrap())
                    .unwrap();
            assert_eq!(observed["request"], input);
            assert_eq!(observed["argv"], json!([LITERAL_ARGUMENT]));
            assert!(call.invoke(step).is_err());
        }
        fixture.assert_reaped();
    }
}

#[test]
fn invalid_booleans_nonzero_exit_and_timeout_preserve_the_failure_action() {
    let fixture = Fixture::new();
    let mut value = document(&fixture);
    value["spec"]["providers"]["policy"]["timeout_ms"] = json!(100);
    let host = fixture.prepare(&value);
    let started = Instant::now();
    for scenario in [
        "null",
        "\"true\"",
        "true false",
        &"X".repeat(33),
        "nonzero",
        "wait",
    ] {
        let cancelled = AtomicBool::new(false);
        let mut input = event("tool.before", scenario);
        input["tool"]["input"] = json!("12345");
        let invocation = host
            .event(input, Instant::now() + TIMEOUT, &cancelled)
            .unwrap()
            .invoke("check")
            .unwrap();
        assert!(invocation.result.is_err(), "{scenario}");
        assert_eq!(invocation.failure_action, Some(FailureAction::Block));
    }
    assert!(started.elapsed() < Duration::from_secs(3));
    fixture.assert_reaped();
}

#[test]
fn early_stdin_close_cannot_turn_incomplete_input_into_a_match_or_a_miss() {
    for result in ["true", "false"] {
        let fixture = Fixture::new();
        let mut value = document(&fixture);
        value["spec"]["providers"]["policy"]["transport"]["argv"] =
            json!(["/bin/sh", "-c", format!("exec 0<&-; printf {result}")]);
        let host = fixture.prepare(&value);
        let cancelled = AtomicBool::new(false);
        let mut input = event("tool.before", "normal");
        input["tool"]["input"] = json!("I".repeat(512 * 1024));
        let invocation = host
            .event(input, Instant::now() + TIMEOUT, &cancelled)
            .unwrap()
            .invoke("check")
            .unwrap();
        assert!(matches!(invocation.result, Err(Failure::IncompleteInput)));
        assert_eq!(invocation.failure_action, Some(FailureAction::Block));
    }
}

#[test]
fn adapter_restrictions_and_invalid_events_fail_before_the_script_runs() {
    let fixture = Fixture::new();
    let value = document(&fixture);
    let cancelled = AtomicBool::new(false);
    let mut adapter = capabilities();
    adapter
        .events
        .get_mut("tool.before")
        .unwrap()
        .retain(|effect| effect != "block");
    assert!(Host::prepare(
        &serde_json::to_vec(&value).unwrap(),
        "target",
        adapter,
        fixture.context(),
        Instant::now() + TIMEOUT,
        &cancelled
    )
    .is_err());
    let host = fixture.prepare(&value);
    let mut input = event("tool.before", "normal");
    input["tool"]["result"] = json!({"invalid":"before result"});
    let invocation = host
        .event(input, Instant::now() + TIMEOUT, &cancelled)
        .unwrap()
        .invoke("check")
        .unwrap();
    assert!(matches!(invocation.result, Err(Failure::Protocol(_))));
    assert!(!fixture.0.join("hook-event.json").exists());
    let call = host
        .event(
            event("tool.before", "normal"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        call.invoke("check").unwrap().result,
        Err(Failure::Transport(aw_exec::Error::Cancelled))
    ));
    assert!(!fixture.0.join("hook-event.json").exists());
}

#[test]
fn default_hook_does_not_skip_an_explicit_providers_handshake() {
    let fixture = Fixture::new();
    let mut value = document(&fixture);
    value["spec"]["providers"]["structured"] =
        fixture.document()["spec"]["providers"]["policy"].clone();
    value["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id":"structured-check","provider":"structured","operation":"check",
            "effects":["block"],"on_error":"block"
        }));
    let host = fixture.prepare(&value);
    assert_eq!(host.preparation().len(), 2);
    assert!(host
        .preparation()
        .iter()
        .all(|record| record.provider == "structured"));
    assert!(!fixture.0.join("hook-event.json").exists());
    let cancelled = AtomicBool::new(false);
    let mut input = event("tool.before", "normal");
    input["tool"]["input"] = json!(["12345"]);
    let call = host
        .event(input, Instant::now() + TIMEOUT, &cancelled)
        .unwrap();
    let StepOutput::Provider(hook) = call.invoke("check").unwrap().result.unwrap() else {
        panic!("expected hook outcome")
    };
    let StepOutput::Provider(structured) = call.invoke("structured-check").unwrap().result.unwrap()
    else {
        panic!("expected structured outcome")
    };
    assert!(hook.requests_block());
    assert!(!structured.requests_block());
    assert_eq!(fixture.calls("describe").len(), 1);
    assert_eq!(fixture.calls("validate_config").len(), 1);
    assert_eq!(fixture.calls("invoke").len(), 1);
    fixture.assert_reaped();
}

#[test]
fn accepted_event_size_and_depth_limits_reach_boolean_hooks() {
    let fixture = Fixture::new();
    let host = fixture.prepare(&document(&fixture));
    let cancelled = AtomicBool::new(false);
    let mut inputs = Vec::new();
    for size in [
        aw_provider::MAX_MESSAGE_BYTES - 1,
        aw_provider::MAX_MESSAGE_BYTES,
    ] {
        let mut input = event("tool.before", "normal");
        input["tool"]["input"] = json!("");
        let overhead = serde_json::to_vec(&input).unwrap().len();
        input["tool"]["input"] = json!(format!("{}12345", "I".repeat(size - overhead - 5)));
        assert_eq!(serde_json::to_vec(&input).unwrap().len(), size);
        inputs.push(input);
    }
    let mut input = event("tool.before", "normal");
    let mut nested = json!("12345");
    for _ in 0..aw_provider::MAX_DEPTH - 2 {
        nested = json!([nested]);
    }
    input["tool"]["input"] = nested;
    inputs.push(input);
    for input in inputs {
        let call = host
            .event(input.clone(), Instant::now() + TIMEOUT, &cancelled)
            .unwrap();
        let invocation = call.invoke("check").unwrap();
        let StepOutput::Provider(outcome) = invocation.result.unwrap() else {
            panic!("expected projected hook outcome")
        };
        assert!(outcome.requests_block());
        assert!(invocation.failure_action.is_none());
        let observed: Value =
            serde_json::from_slice(&fs::read(fixture.0.join("hook-event.json")).unwrap()).unwrap();
        assert_eq!(observed["request"], input);
    }
    fixture.assert_reaped();
}

#[test]
fn events_above_the_size_or_depth_limit_never_run_the_boolean_script() {
    let fixture = Fixture::new();
    let host = fixture.prepare(&document(&fixture));
    let cancelled = AtomicBool::new(false);
    let mut input = event("tool.before", "normal");
    input["tool"]["input"] = json!("I".repeat(aw_provider::MAX_MESSAGE_BYTES));
    assert!(host
        .event(input, Instant::now() + TIMEOUT, &cancelled)
        .is_err());
    let mut input = event("tool.before", "normal");
    let mut nested = json!("12345");
    for _ in 0..aw_provider::MAX_DEPTH - 1 {
        nested = json!([nested]);
    }
    input["tool"]["input"] = nested;
    assert!(host
        .event(input, Instant::now() + TIMEOUT, &cancelled)
        .is_err());
    assert!(!fixture.0.join("hook-event.json").exists());
}
