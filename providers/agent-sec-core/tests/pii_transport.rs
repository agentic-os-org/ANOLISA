//! Shared PII CLI transport and completeness contract.
#![cfg(target_os = "linux")]

#[path = "support/fixture.rs"]
mod support;
use aw_provider_sec_core::pii::{self, Source, Verdict};
use serde_json::json;
use std::{fs, path::Path, time::Duration};

#[test]
fn shared_scanner_uses_stdin_sources_and_complete_verdicts() {
    for source in [Source::UserInput, Source::ToolInput, Source::ToolOutput] {
        for text in ["safe 界", "SENSITIVE --socket $(touch bad)"] {
            let d = support::Directory::new(json!({"pii":true}));
            let verdict = pii::call(
                &d.cli(),
                Path::new("/unused.sock"),
                text,
                source,
                &json!({"session_id":"session"}),
                Duration::from_secs(1),
            )
            .unwrap();
            assert_eq!(
                verdict,
                if text.contains("SENSITIVE") {
                    Verdict::Risk
                } else {
                    Verdict::Pass
                }
            );
            assert_eq!(fs::read_to_string(d.0.join("stdin.txt")).unwrap(), text);
            let args = d.argv();
            let args = args.as_array().unwrap();
            assert!(args.iter().all(|a| !a.as_str().unwrap().contains(text)));
            assert_eq!(args[6], "scan-pii");
            assert_eq!(args[7], "--stdin");
            assert_eq!(args[11], source.as_str());
            d.assert_stopped();
        }
    }
}

#[test]
fn malformed_incomplete_and_failed_scans_are_errors_not_risks() {
    let valid = json!({"ok":true,"verdict":"pass","summary":{
        "source":"tool_input","bytes_scanned":4,"truncated":false}});
    let mut replies = vec![
        json!({"raw":""}),
        json!({"raw":"{}"}),
        json!({"raw":"[]"}),
        json!({"stdout":valid,"exit":1}),
        json!({"stdout":valid,"stdout_repeat":1048577}),
        json!({"stdout":valid,"stderr_repeat":65537}),
    ];
    for (pointer, value) in [
        ("/ok", json!(false)),
        ("/verdict", json!("error")),
        ("/summary", json!(null)),
        ("/summary/source", json!("tool_output")),
        ("/summary/bytes_scanned", json!(3)),
        ("/summary/truncated", json!(true)),
    ] {
        let mut result = valid.clone();
        *result.pointer_mut(pointer).unwrap() = value;
        replies.push(json!({"stdout":result}));
    }
    for field in [
        json!({"execution_status":"degraded"}),
        json!({"coverage":{"status":"partial"}}),
        json!({"custom_rules":{"status":"invalid"}}),
        json!({"custom_rules":{"status":"loaded",
            "runtime_error_count":1,"budget_exhausted":false,"truncated":false}}),
    ] {
        let mut result = valid.clone();
        result["summary"]
            .as_object_mut()
            .unwrap()
            .extend(field.as_object().unwrap().clone());
        replies.push(json!({"stdout":result}));
    }
    for case in replies {
        let d = support::Directory::new(case.clone());
        assert!(
            pii::call(
                &d.cli(),
                Path::new("/unused.sock"),
                "safe",
                Source::ToolInput,
                &json!({}),
                Duration::from_secs(1)
            )
            .is_err(),
            "{case}"
        );
        d.assert_stopped();
    }
}

#[test]
fn pii_deadline_joins_cli_and_missing_backend_is_a_failure() {
    let d = support::Directory::new(json!({"sleep":10,"pii":true}));
    let error = pii::call(
        &d.cli(),
        Path::new("/unused.sock"),
        "safe",
        Source::ToolInput,
        &json!({}),
        Duration::from_millis(100),
    )
    .unwrap_err();
    assert_eq!(error.code(), "deadline_exceeded");
    d.assert_stopped();
    let error = pii::call(
        Path::new("/nonexistent-pii-cli"),
        Path::new("/unused.sock"),
        "safe",
        Source::ToolInput,
        &json!({}),
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert_eq!(error.code(), "cli_transport_error");
}
