//! Tool-selector misses retain event claims without invoking any command protocol.
#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "support/fixture.rs"]
mod fixture;

use aw_host::{Failure, FailureAction, Method, StepOutput};
use fixture::{event, Fixture, TIMEOUT};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

fn document(fixture: &Fixture, protocol: &str) -> Value {
    let mut document = fixture.document();
    let provider = &mut document["spec"]["providers"]["policy"];
    provider["protocol"] = json!(protocol);
    if protocol != aw_provider::VERSION {
        provider["config"] = json!({});
        provider["transport"]["argv"] = json!([
            provider["transport"]["argv"][0],
            "-I",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/boolean_hook.py"),
            fixture.0
        ]);
        for name in ["tool.before", "tool.after"] {
            let step = &mut document["spec"]["events"][name]["steps"][0];
            step.as_object_mut().unwrap().remove("operation");
            if protocol == "native-hook/v1alpha1" {
                step.as_object_mut().unwrap().remove("effects");
                step["native"] = json!({});
            }
        }
    }
    document
}

fn miss_event(name: &str) -> Value {
    let mut value = event(name, "block");
    value["tool"]["name"] = json!("file.read");
    value["tool"]["native_name"] = json!("read_file");
    value
}

#[test]
fn event_and_step_misses_skip_all_protocols_and_consume_once_claims() {
    for protocol in [
        aw_provider::VERSION,
        aw_config::HOOK_PROTOCOL,
        "native-hook/v1alpha1",
    ] {
        for selector_location in ["event", "step"] {
            let fixture = Fixture::new();
            let mut document = document(&fixture, protocol);
            for name in ["tool.before", "tool.after"] {
                let event = &mut document["spec"]["events"][name];
                let selected = if selector_location == "event" {
                    event
                } else {
                    &mut event["steps"][0]
                };
                selected["match"] = json!({"tools":["bash"]});
            }
            let host = fixture.prepare(&document);
            assert_eq!(
                host.preparation().len(),
                if protocol == aw_provider::VERSION {
                    2
                } else {
                    0
                }
            );
            let cancelled = AtomicBool::new(false);
            for (name, step) in [("tool.before", "check"), ("tool.after", "record")] {
                let event = host
                    .hook_event(
                        miss_event(name),
                        b"{}".to_vec(),
                        Instant::now() + TIMEOUT,
                        &cancelled,
                    )
                    .unwrap();
                assert_eq!(
                    event
                        .steps()
                        .map(|step| step.step_id.as_str())
                        .collect::<Vec<_>>(),
                    [step]
                );
                let result = event.invoke(step).unwrap();
                assert!(matches!(result.result, Ok(StepOutput::Skipped)));
                assert_eq!(result.failure_action, None);
                assert!(result.record.process.is_none());
                assert_eq!(result.record.event_id.as_deref(), Some(event.id()));
                assert_eq!(result.record.step_id.as_deref(), Some(step));
                assert_eq!(
                    result.record.method,
                    if protocol == "native-hook/v1alpha1" {
                        Method::NativeHook
                    } else {
                        Method::Invoke
                    }
                );
                assert!(event.invoke(step).is_err());
            }
            assert!(fixture.calls("invoke").is_empty());
            assert!(!fixture.0.join("hook-event.json").exists());
            fixture.assert_reaped();
        }
    }
}

#[test]
fn skipped_predecessor_does_not_hide_a_later_denial() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    fixture::add_second_step(&mut document);
    document["spec"]["events"]["tool.before"]["steps"][0]["match"] = json!({"tools":["bash"]});
    let host = fixture.prepare(&document);
    let cancelled = AtomicBool::new(false);
    let event = host
        .event(
            miss_event("tool.before"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    assert_eq!(event.steps().count(), 2);
    assert!(matches!(
        event.invoke("check").unwrap().result,
        Ok(StepOutput::Skipped)
    ));
    let StepOutput::Provider(outcome) = event.invoke("second").unwrap().result.unwrap() else {
        panic!("expected matching Provider invocation")
    };
    assert!(outcome.requests_block());
    assert_eq!(fixture.calls("invoke").len(), 1);
    fixture.assert_reaped();
}

#[test]
fn malformed_misses_remain_protocol_failures_without_starting_commands() {
    for protocol in [
        aw_provider::VERSION,
        aw_config::HOOK_PROTOCOL,
        "native-hook/v1alpha1",
    ] {
        let fixture = Fixture::new();
        let mut document = document(&fixture, protocol);
        document["spec"]["events"]["tool.before"]["match"] = json!({"tools":["bash"]});
        let host = fixture.prepare(&document);
        let cancelled = AtomicBool::new(false);
        for (pointer, replacement) in [
            ("/tool/result", json!({"premature":true})),
            ("/tool/native_name", json!(null)),
            ("/tool/call_id", json!(42)),
            ("/session_id", json!([])),
            ("/native", json!(null)),
        ] {
            let mut input = miss_event("tool.before");
            *input.pointer_mut(pointer).unwrap() = replacement;
            let event = host
                .hook_event(input, b"{}".to_vec(), Instant::now() + TIMEOUT, &cancelled)
                .unwrap();
            let result = event.invoke("check").unwrap();
            assert!(
                matches!(result.result, Err(Failure::Protocol(_))),
                "{protocol} {pointer}"
            );
            assert_eq!(result.failure_action, Some(FailureAction::Block));
            assert!(result.record.process.is_none());
            assert!(event.invoke("check").is_err());
        }
        assert!(fixture.calls("invoke").is_empty());
        assert!(!fixture.0.join("hook-event.json").exists());
        fixture.assert_reaped();
    }
}

#[test]
fn cancellation_and_expiry_cannot_become_successful_skips() {
    for protocol in [
        aw_provider::VERSION,
        aw_config::HOOK_PROTOCOL,
        "native-hook/v1alpha1",
    ] {
        let fixture = Fixture::new();
        let mut document = document(&fixture, protocol);
        document["spec"]["events"]["tool.before"]["match"] = json!({"tools":["bash"]});
        let host = fixture.prepare(&document);
        for cancel in [true, false] {
            let cancelled = AtomicBool::new(false);
            let event = host
                .hook_event(
                    miss_event("tool.before"),
                    b"{}".to_vec(),
                    Instant::now()
                        + if cancel {
                            TIMEOUT
                        } else {
                            Duration::from_millis(20)
                        },
                    &cancelled,
                )
                .unwrap();
            if cancel {
                cancelled.store(true, Ordering::Release);
            } else {
                thread::sleep(Duration::from_millis(25));
            }
            let result = event.invoke("check").unwrap();
            assert!(
                matches!(result.result, Err(Failure::Transport(aw_exec::Error::Cancelled)) if cancel)
                    || matches!(result.result, Err(Failure::Transport(aw_exec::Error::DeadlineExceeded)) if !cancel)
            );
            assert_eq!(result.failure_action, Some(FailureAction::Block));
            assert!(result.record.process.is_none());
            assert!(event.invoke("check").is_err());
        }
        assert!(fixture.calls("invoke").is_empty());
        assert!(!fixture.0.join("hook-event.json").exists());
        fixture.assert_reaped();
    }
}
