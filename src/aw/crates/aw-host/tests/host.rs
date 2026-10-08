//! Real Provider methods exercise immutable preparation and event execution.
#![cfg(target_os = "linux")]

#[path = "support/fixture.rs"]
mod fixture;

#[path = "host/timing.rs"]
mod timing;

use aw_host::{Error, Failure, FailureAction, Host, Method};
use fixture::{
    add_second_step, capabilities, event, Fixture, CLEANUP_ALLOWANCE, LITERAL_ARGUMENT, TIMEOUT,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

#[test]
fn preparation_deduplicates_enabled_providers_and_skips_disabled_references() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    let mut disabled = document["spec"]["providers"]["policy"].clone();
    disabled["transport"]["argv"] = json!(["/disabled-provider-must-not-run"]);
    document["spec"]["providers"]["disabled"] = disabled;
    document["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "disabled", "enabled": false, "provider": "disabled", "operation": "later",
            "effects": ["replace_input"], "on_error": "report"
        }));
    document["spec"]["events"]["session.start"] = json!({
        "enabled": false, "steps": [{
            "id": "disabled-event", "provider": "disabled", "operation": "later",
            "effects": ["observe"], "on_error": "report"
        }]
    });
    let host = fixture.prepare(&document);
    assert_eq!(host.steps().len(), 2);
    assert_eq!(host.preparation().len(), 2);
    assert_eq!(host.preparation()[0].method, Method::Describe);
    assert_eq!(host.preparation()[1].method, Method::ValidateConfig);
    assert_ne!(
        host.preparation()[0].request_id,
        host.preparation()[1].request_id
    );
    for record in host.preparation() {
        assert_eq!(record.provider, "policy");
        assert!(record.event_id.is_none());
        assert!(record.step_id.is_none());
        assert!(record.process.as_ref().unwrap().status.success());
    }
    assert_eq!(fixture.calls("describe").len(), 1);
    assert_eq!(fixture.calls("validate_config").len(), 1);
    fixture.assert_reaped();
}

#[test]
fn unsupported_configuration_is_rejected_before_any_provider_side_effect() {
    let fixture = Fixture::new();
    for (pointer, replacement) in [
        (
            "/spec/events/tool.before/steps/0/effects",
            json!(["replace_input"]),
        ),
        (
            "/spec/events/tool.after/steps/0/on_error",
            json!("withhold_result"),
        ),
    ] {
        let mut document = fixture.document();
        *document.pointer_mut(pointer).unwrap() = replacement;
        let result = Host::prepare(
            &serde_json::to_vec(&document).unwrap(),
            "target",
            capabilities(),
            fixture.context(),
            Instant::now() + TIMEOUT,
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(Error::Admission(_))));
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    }
}

#[test]
fn failed_preparation_reports_the_method_and_preserves_native_status() {
    let fixture = Fixture::new();
    for (scenario, expected_method) in [
        ("describe-wrong-id", Method::Describe),
        ("describe-invalid-json", Method::Describe),
        ("describe-nonzero", Method::Describe),
        ("validate_config-provider-error", Method::ValidateConfig),
    ] {
        let mut document = fixture.document();
        document["spec"]["providers"]["policy"]["transport"]["argv"][4] = json!(scenario);
        let error = Host::prepare(
            &serde_json::to_vec(&document).unwrap(),
            "target",
            capabilities(),
            fixture.context(),
            Instant::now() + TIMEOUT,
            &AtomicBool::new(false),
        )
        .err()
        .unwrap();
        assert!(!error.to_string().contains("private-stdout"));
        let Error::Preparation(preparation) = error else {
            panic!("expected preparation history")
        };
        assert_eq!(
            preparation.completed.len(),
            usize::from(expected_method == Method::ValidateConfig)
        );
        let Error::Call(call) = preparation.cause else {
            panic!("expected preparation call failure")
        };
        assert_eq!(call.record.method, expected_method);
        let status = call.record.process.as_ref().unwrap().status;
        match scenario {
            "describe-nonzero" => {
                assert!(matches!(call.failure, Failure::Exit));
                assert_eq!(status.code(), Some(17));
            }
            "validate_config-provider-error" => {
                assert!(
                    matches!(call.failure, Failure::Provider { code } if code == "fixture.private_failure")
                );
                assert!(status.success());
            }
            _ => assert!(matches!(call.failure, Failure::Protocol(_))),
        }
        fixture.assert_reaped();
    }
    let mut document = fixture.document();
    document["spec"]["events"]["tool.before"]["steps"][0]["operation"] = json!("not-declared");
    let error = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        "target",
        capabilities(),
        fixture.context(),
        Instant::now() + TIMEOUT,
        &AtomicBool::new(false),
    )
    .err()
    .unwrap();
    let Error::Preparation(preparation) = error else {
        panic!("expected final admission history")
    };
    assert_eq!(preparation.completed.len(), 2);
    assert_eq!(preparation.completed[0].method, Method::Describe);
    assert_eq!(preparation.completed[1].method, Method::ValidateConfig);
    assert!(matches!(preparation.cause, Error::Admission(_)));
    fixture.assert_reaped();
}

#[test]
fn tool_payloads_and_private_json_preserve_unicode_decimals_and_native_types() {
    let fixture = Fixture::new();
    let document = fixture.document();
    let host = fixture.prepare(&document);
    let cancelled = AtomicBool::new(false);
    for (name, step, result) in [
        ("tool.before", "check", json!(null)),
        (
            "tool.after",
            "record",
            json!("{\"still\":\"a string 结果\"}"),
        ),
        ("tool.after", "record", json!({"输出": [0.5, null, false]})),
    ] {
        let mut value = event(name, "observe");
        value["tool"]["result"] = result;
        value["tool"]["input"]["integer"] = json!(9007199254740993_u64);
        let selected = host
            .event(value.clone(), Instant::now() + TIMEOUT, &cancelled)
            .unwrap();
        let invocation = selected.invoke(step).unwrap();
        assert!(invocation.result.is_ok());
        let observed = fixture.call(&invocation.record.request_id);
        assert_eq!(observed["request"]["event"], value);
        assert_eq!(
            observed["request"]["config"],
            document["spec"]["providers"]["policy"]["config"]
        );
        assert_eq!(
            invocation
                .record
                .process
                .as_ref()
                .unwrap()
                .input_bytes_written as u64,
            observed["input_bytes"].as_u64().unwrap()
        );
        fixture.assert_reaped();
    }
}

#[test]
fn successful_allow_block_and_after_observe_remain_candidate_effects() {
    let fixture = Fixture::new();
    let host = fixture.prepare(&fixture.document());
    let cancelled = AtomicBool::new(false);
    for (name, step, scenario, blocked, effect_count) in [
        ("tool.before", "check", "allow", false, 0),
        ("tool.before", "check", "block", true, 1),
        ("tool.after", "record", "observe", false, 1),
    ] {
        let selected = host
            .event(event(name, scenario), Instant::now() + TIMEOUT, &cancelled)
            .unwrap();
        let invocation = selected.invoke(step).unwrap();
        assert!(invocation.failure_action.is_none());
        let outcome = invocation.result.unwrap();
        assert_eq!(outcome.requests_block(), blocked);
        assert_eq!(
            outcome.as_value()["effects"].as_array().unwrap().len(),
            effect_count
        );
        assert_eq!(invocation.record.method, Method::Invoke);
        assert_eq!(invocation.record.event_id.as_deref(), Some(selected.id()));
        assert_eq!(invocation.record.step_id.as_deref(), Some(step));
        assert_eq!(
            invocation.record.process.as_ref().unwrap().stderr,
            b"fixture diagnostic\xff"
        );
    }
    fixture.assert_reaped();
}

#[test]
fn malformed_or_uncorrelated_responses_fail_without_becoming_policy_blocks() {
    let fixture = Fixture::new();
    let host = fixture.prepare(&fixture.document());
    let cancelled = AtomicBool::new(false);
    for scenario in [
        "wrong-id",
        "wrong-digest",
        "extra-effects",
        "invalid-json",
        "extra-document",
    ] {
        let selected = host
            .event(
                event("tool.before", scenario),
                Instant::now() + TIMEOUT,
                &cancelled,
            )
            .unwrap();
        let invocation = selected.invoke("check").unwrap();
        assert!(
            matches!(invocation.result, Err(Failure::Protocol(_))),
            "scenario {scenario}"
        );
        assert_eq!(invocation.failure_action, Some(FailureAction::Block));
        assert!(invocation.record.process.as_ref().unwrap().status.success());
    }
    let selected = host
        .event(
            event("tool.after", "block"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    let invocation = selected.invoke("record").unwrap();
    assert!(matches!(invocation.result, Err(Failure::Protocol(_))));
    assert_eq!(invocation.failure_action, Some(FailureAction::Report));
    fixture.assert_reaped();
}

#[test]
fn valid_stdout_cannot_turn_a_nonzero_exit_into_success() {
    let fixture = Fixture::new();
    let host = fixture.prepare(&fixture.document());
    let cancelled = AtomicBool::new(false);
    let selected = host
        .event(
            event("tool.before", "nonzero"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    let invocation = selected.invoke("check").unwrap();
    assert!(matches!(invocation.result, Err(Failure::Exit)));
    assert_eq!(invocation.failure_action, Some(FailureAction::Block));
    let process = invocation.record.process.unwrap();
    assert_eq!(process.status.code(), Some(17));
    assert!(process.stdout_bytes > 0);
    fixture.assert_reaped();
}

#[test]
fn provider_error_keeps_its_code_separate_from_configured_failure_action() {
    let fixture = Fixture::new();
    for (name, step, on_error, action) in [
        ("tool.before", "check", "block", FailureAction::Block),
        ("tool.before", "check", "report", FailureAction::Report),
        ("tool.after", "record", "report", FailureAction::Report),
    ] {
        let mut document = fixture.document();
        document["spec"]["events"][name]["steps"][0]["on_error"] = json!(on_error);
        let host = fixture.prepare(&document);
        let cancelled = AtomicBool::new(false);
        let selected = host
            .event(
                event(name, "provider-error"),
                Instant::now() + TIMEOUT,
                &cancelled,
            )
            .unwrap();
        let invocation = selected.invoke(step).unwrap();
        assert_eq!(invocation.failure_action, Some(action));
        let failure = invocation.result.err().unwrap();
        assert!(!failure.to_string().contains("fixture.private_failure"));
        assert!(matches!(failure, Failure::Provider { code } if code == "fixture.private_failure"));
        assert!(invocation.record.process.as_ref().unwrap().status.success());
    }
    fixture.assert_reaped();
}

#[test]
fn stdout_and_stderr_limits_are_independent_transport_failures() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["providers"]["policy"]["max_output_bytes"] = json!(1024);
    let mut context = fixture.context();
    context.stderr_bytes = 1024;
    let cancelled = AtomicBool::new(false);
    let host = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        "target",
        capabilities(),
        context,
        Instant::now() + TIMEOUT,
        &cancelled,
    )
    .unwrap();
    for (scenario, expected) in [
        ("stdout-limit", aw_exec::Stream::Stdout),
        ("stderr-limit", aw_exec::Stream::Stderr),
    ] {
        let selected = host
            .event(
                event("tool.before", scenario),
                Instant::now() + TIMEOUT,
                &cancelled,
            )
            .unwrap();
        let invocation = selected.invoke("check").unwrap();
        assert!(
            matches!(invocation.result, Err(Failure::Transport(aw_exec::Error::OutputLimit { stream, limit: 1024 })) if stream == expected)
        );
        assert!(invocation.record.process.is_none());
        fixture.assert_reaped();
    }
}

#[test]
fn retained_document_revision_command_and_environment_match_every_method() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    // JSON Schema integer fields also admit integral decimal/exponent values.
    document["spec"]["execution"]["default_event_budget_ms"] = json!(5000.0);
    document["spec"]["events"]["tool.before"]["budget_ms"] = json!(4000.0);
    document["spec"]["providers"]["policy"]["timeout_ms"] = json!(5000.0);
    document["spec"]["providers"]["policy"]["max_output_bytes"] = json!(65536.0);
    let original_config = document["spec"]["providers"]["policy"]["config"].clone();
    let mut bytes = serde_json::to_vec_pretty(&document).unwrap();
    bytes.push(b'\n');
    let expected_revision = format!("{:x}", Sha256::digest(&bytes));
    let mut environment = fixture.context().environment;
    let mut context = fixture.context();
    context.environment = environment.clone();
    let cancelled = AtomicBool::new(false);
    let host = Host::prepare(
        &bytes,
        "target",
        capabilities(),
        context,
        Instant::now() + TIMEOUT,
        &cancelled,
    )
    .unwrap();
    bytes.fill(b'x');
    environment.insert("AW_HOST_MARKER".into(), "changed".into());
    document["spec"]["providers"]["policy"]["config"] = json!({"changed": true});
    document["spec"]["providers"]["policy"]["transport"]["argv"] = json!(["/changed-program"]);
    assert_eq!(host.revision(), expected_revision);
    assert_eq!(host.target(), "target");
    assert_eq!(host.capabilities().version, "fixture-version");
    assert_eq!(host.capabilities().entrypoint, "fixture-entrypoint");
    let selected = host
        .event(
            event("tool.before", "allow"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap();
    let invocation = selected.invoke("check").unwrap();
    assert!(invocation.result.is_ok());
    for record in host
        .preparation()
        .iter()
        .chain(std::iter::once(&invocation.record))
    {
        assert_eq!(record.config_revision, expected_revision);
        assert_eq!(record.binding_id, "target");
        let observed = fixture.call(&record.request_id);
        assert_eq!(observed["cwd"], fixture.0.to_str().unwrap());
        assert_eq!(observed["arguments"], json!([LITERAL_ARGUMENT]));
        assert_eq!(
            observed["environment"],
            json!({"LC_ALL": "C", "AW_HOST_MARKER": "retained-context"})
        );
        if record.method != Method::Describe {
            assert_eq!(observed["request"]["config"], original_config);
        }
        if record.method == Method::Invoke {
            assert_eq!(observed["request"]["config_revision"], expected_revision);
        }
    }
    fixture.assert_reaped();
}

#[test]
fn mismatched_or_invalid_event_data_is_rejected_before_invocation() {
    let fixture = Fixture::new();
    let host = fixture.prepare(&fixture.document());
    let cancelled = AtomicBool::new(false);
    for (field, value) in [("adapter", "openclaw"), ("binding_id", "another-target")] {
        let mut value_event = event("tool.before", "allow");
        value_event["agent"][field] = json!(value);
        assert!(matches!(
            host.event(value_event, Instant::now() + TIMEOUT, &cancelled),
            Err(Error::Invalid(_))
        ));
    }
    let mut value = event("tool.before", "allow");
    value["tool"]["result"] = json!({"premature": "result"});
    let selected = host
        .event(value, Instant::now() + TIMEOUT, &cancelled)
        .unwrap();
    let invocation = selected.invoke("check").unwrap();
    assert!(matches!(invocation.result, Err(Failure::Protocol(_))));
    assert!(invocation.record.process.is_none());
    assert!(fixture.calls("invoke").is_empty());
    fixture.assert_reaped();
}
