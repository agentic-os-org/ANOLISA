//! Generated policies exercise real normalization, Host selection and Provider processes.
#![cfg(target_os = "linux")]

#[path = "support/coverage.rs"]
mod support;

use aw_host::{FailureAction, StepOutput};
use serde_json::json;
use std::fs;
use support::Consumer;

#[test]
fn templates_scan_declared_tools_and_leave_other_native_tools_unchecked() {
    for adapter in ["qoder", "openclaw", "qoder-sdk", "qwenpaw"] {
        let consumer = Consumer::new();
        let document = consumer.document(adapter);
        let host = consumer.prepare(&document, adapter);
        assert_eq!(host.preparation().len(), 2);
        assert_eq!(consumer.scan_calls(), 0);
        for name in [
            consumer.covered_name(adapter),
            "Read",
            "mcp__local__query",
            "future_tool",
        ] {
            let event = consumer.event(adapter, name, json!({"command":"echo safe"}));
            let covered = name == consumer.covered_name(adapter);
            let result = consumer.invoke(&host, event);
            match &result.result {
                Ok(StepOutput::Provider(outcome)) if covered => {
                    assert_eq!(outcome.as_value()["effects"][0]["reason_code"], "code_pass");
                }
                Ok(StepOutput::Skipped) if !covered => assert!(result.record.process.is_none()),
                _ => panic!("unexpected coverage result for {adapter}/{name}"),
            }
            assert!(consumer.dispatch(&result, false));
            assert!(!consumer.dispatch(&result, true));
        }
        assert_eq!(consumer.scan_calls(), 1, "{adapter}");
        assert_eq!(consumer.handler_calls(), 4, "{adapter}");
        assert!(consumer.effect_exists());
    }
}

#[test]
fn selected_faults_block_before_handler_dispatch_and_preserve_failure_identity() {
    for adapter in ["qoder", "openclaw", "qoder-sdk", "qwenpaw"] {
        for fault in [
            "deny",
            "mapping",
            "input",
            "normalized-name",
            "identity",
            "timeout",
            "crash",
            "backend",
        ] {
            let consumer = Consumer::new();
            let mut document = consumer.document(adapter);
            if fault == "mapping" {
                document["spec"]["providers"]["security"]["config"]["tools"] =
                    json!({"unrelated":{"language":"bash","input_pointer":"/command"}});
            }
            let host = consumer.prepare(&document, adapter);
            let mut event = consumer.event(
                adapter,
                consumer.covered_name(adapter),
                json!({"command":"echo safe"}),
            );
            match fault {
                "deny" | "timeout" | "backend" => consumer.scanner_mode(fault),
                "input" => event["tool"]["input"] = json!({"command":17}),
                "normalized-name" => event["tool"]["name"] = json!("renamed"),
                "identity" => {
                    event["tool"].as_object_mut().unwrap().remove("native_name");
                }
                "crash" => consumer.crash_provider(),
                _ => {}
            }
            let result = consumer.invoke(&host, event);
            if fault == "deny" {
                let Ok(StepOutput::Provider(outcome)) = &result.result else {
                    panic!("expected policy denial")
                };
                assert!(outcome.requests_block());
                assert_eq!(result.failure_action, None);
            } else {
                assert!(result.result.is_err(), "{adapter}/{fault}");
                assert_eq!(result.failure_action, Some(FailureAction::Block));
            }
            assert_eq!(result.record.step_id.as_deref(), Some("scan-code"));
            assert!(result.record.event_id.is_some());
            assert!(!consumer.dispatch(&result, false), "{adapter}/{fault}");
            assert_eq!(consumer.handler_calls(), 0);
            assert!(!consumer.effect_exists());
            assert_eq!(
                consumer.scan_calls(),
                usize::from(matches!(fault, "deny" | "timeout" | "backend")),
                "{adapter}/{fault}"
            );
        }
    }
}

#[test]
fn generated_completion_observation_uses_the_same_declared_coverage() {
    for adapter in ["qoder", "openclaw"] {
        let consumer = Consumer::new();
        let host = consumer.prepare(&consumer.document(adapter), adapter);
        for name in [
            consumer.covered_name(adapter),
            "mcp__local__query",
            "future_tool",
        ] {
            let mut event = consumer.event(adapter, name, json!({"command":"echo safe"}));
            event["name"] = json!("tool.after");
            event["tool"]["result"] = json!("done");
            let result = consumer.invoke(&host, event);
            if name == consumer.covered_name(adapter) {
                let Ok(StepOutput::Provider(outcome)) = result.result else {
                    panic!("missing observation")
                };
                assert_eq!(
                    outcome.as_value()["effects"][0]["reason_code"],
                    "tool_observed"
                );
            } else {
                assert!(matches!(result.result, Ok(StepOutput::Skipped)));
                assert!(result.record.process.is_none());
            }
        }
        assert_eq!(consumer.scan_calls(), 0);
    }
}

#[test]
fn invalid_extraction_never_calls_scanner_and_python_uses_its_declared_language() {
    let consumer = Consumer::new();
    let mut document = consumer.document("qoder-sdk");
    let mapping = &mut document["spec"]["providers"]["security"]["config"]["tools"]["bash"];
    mapping["language"] = json!("python");
    mapping["input_pointer"] = json!("/source/code");
    let host = consumer.prepare(&document, "qoder-sdk");
    for input in [
        json!({}),
        json!({"source":{"code":null}}),
        json!({"source":{"code":[]}}),
    ] {
        let result = consumer.invoke(&host, consumer.event("qoder-sdk", "Bash", input));
        assert_eq!(result.failure_action, Some(FailureAction::Block));
        assert!(!consumer.dispatch(&result, false));
    }
    assert_eq!(consumer.scan_calls(), 0);
    let result = consumer.invoke(
        &host,
        consumer.event(
            "qoder-sdk",
            "Bash",
            json!({"source":{"code":"print('safe')"}}),
        ),
    );
    assert!(consumer.dispatch(&result, false));
    let args: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(consumer.root.join("scans.jsonl")).unwrap())
            .unwrap();
    let args = args.as_array().unwrap();
    let language = args.iter().position(|arg| arg == "--language").unwrap();
    assert_eq!(args[language + 1], "python");
    assert_eq!(consumer.scan_calls(), 1);
    assert_eq!(consumer.handler_calls(), 1);
}

#[test]
fn provider_example_covers_real_qoder_bash_with_fail_closed_defaults() {
    let consumer = Consumer::new();
    let host = consumer.prepare(&consumer.document("provider-example"), "qoder");
    consumer.scanner_mode("deny");
    let result = consumer.invoke(
        &host,
        consumer.event("qoder", "Bash", json!({"command":"unsafe"})),
    );
    let Ok(StepOutput::Provider(outcome)) = &result.result else {
        panic!("missing scan")
    };
    assert!(outcome.requests_block());
    assert!(!consumer.dispatch(&result, false));
    consumer.scanner_mode("backend");
    let result = consumer.invoke(
        &host,
        consumer.event("qoder", "Bash", json!({"command":"safe"})),
    );
    assert_eq!(result.failure_action, Some(FailureAction::Block));
    assert!(!consumer.dispatch(&result, false));
    assert_eq!(consumer.handler_calls(), 0);
    assert!(!consumer.effect_exists());
    let result = consumer.invoke(
        &host,
        consumer.event("qoder", "mcp__local__query", json!({})),
    );
    assert!(matches!(result.result, Ok(StepOutput::Skipped)));
    assert!(consumer.dispatch(&result, false));
    assert_eq!(consumer.scan_calls(), 2);
}
