//! Exercise the installed-shape binary contract, not a mocked CLI entry point.

use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use semantic_arbiter::protocol::{decode_request, decode_response, MAX_MESSAGE_BYTES};
use serde_json::{json, Value};

fn run(args: &[&str], input: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_semantic-arbiter"))
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(bytes) = input {
        let mut stdin = child.stdin.take().unwrap();
        if let Err(error) = stdin.write_all(bytes) {
            // An oversized stream may be closed after the bounded prefix is read.
            assert_eq!(error.kind(), ErrorKind::BrokenPipe);
        }
    }
    child.wait_with_output().unwrap()
}

fn assert_error(output: Output, code: &str, secret: &str) {
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains(secret),
        "Diagnostics must not echo input or paths"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&stderr).unwrap(),
        json!({
            "api_version": "semantic-arbiter/v1alpha1", "error": code
        })
    );
}

#[test]
fn describe_distinguishes_check_and_configured_model_evaluation() {
    let output = run(&["describe"], None);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({
            "evaluation_api_version": "semantic-arbiter/v1alpha2",
            "check_api_version": "semantic-arbiter/v1alpha1",
            "operations": ["check", "evaluate"],
            "modes": ["deterministic_check", "model_evaluation"],
            "model_backends": ["bailian", "typesafe"],
            "scenarios": ["preservation", "task_scope"],
            "candidate_controls": ["nop", "allow", "ask", "block"],
            "obligation_kinds": ["snapshot", "audit"],
            "max_message_bytes": 262144
        })
    );
}

#[test]
fn help_is_human_readable_and_available_without_arguments() {
    let expected = run(&["--help"], None);
    let text = String::from_utf8(expected.stdout.clone()).unwrap();
    for section in [
        "Usage:",
        "Commands:",
        "Scenarios:",
        "Examples",
        "Exit status:",
    ] {
        assert!(text.contains(section), "Missing help section: {section}");
    }
    assert!(text.contains("preservation"));
    assert!(text.contains("task_scope"));
    assert!(serde_json::from_str::<Value>(&text).is_err());
    for args in [&[][..], &["--help"][..], &["-h"][..], &["help"][..]] {
        let output = run(args, None);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        assert_eq!(output.stdout, expected.stdout);
    }
}

#[test]
fn command_help_does_not_require_input_configuration_or_credentials() {
    for command in ["describe", "check", "evaluate"] {
        let expected = run(&["help", command], None);
        let text = String::from_utf8(expected.stdout.clone()).unwrap();
        assert!(text.contains(&format!("semantic-arbiter {command} -")));
        assert!(text.contains("Usage:"));
        assert!(text.contains("Example"));
        for args in [["help", command], [command, "--help"], [command, "-h"]] {
            let output = run(&args, None);
            assert_eq!(output.status.code(), Some(0));
            assert!(output.stderr.is_empty());
            assert_eq!(output.stdout, expected.stdout);
        }
    }
}

#[test]
fn invalid_help_arguments_remain_sanitized_errors() {
    let secret = "synthetic-secret-help-argument";
    for args in [
        vec!["help", secret],
        vec![secret, "--help"],
        vec!["--help", secret],
        vec!["describe", "--help", secret],
    ] {
        assert_error(run(&args, None), "invalid_arguments", secret);
    }
}

#[test]
fn stdin_and_path_match_the_same_independently_stated_golden_response() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/preservation-allow.json");
    let input = include_bytes!("../examples/preservation-allow.json");
    let stdin = run(&["check", "-"], Some(input));
    let file = run(&["check", path.to_str().unwrap()], None);
    let expected: Value =
        serde_json::from_slice(include_bytes!("fixtures/preservation-allow.response.json"))
            .unwrap();
    for output in [&stdin, &file] {
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            expected
        );
        assert_eq!(output.stdout.last(), Some(&b'\n'));
        let request = decode_request(input).unwrap();
        assert!(decode_response(&output.stdout, &request).is_ok());
    }
    assert_eq!(stdin.stdout, file.stdout);
}

#[test]
fn a_valid_block_is_a_successful_judgment_with_exit_zero() {
    let bytes = include_bytes!("../examples/preserve.json");
    let output = run(&["check", "-"], Some(bytes));
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let request = decode_request(bytes).unwrap();
    let response = decode_response(&output.stdout, &request).unwrap();
    let response = serde_json::to_value(response).unwrap();
    assert_eq!(
        response["outcome"]["control"],
        json!({"candidate": "block", "subject": []})
    );
    assert_eq!(
        response["answers"],
        json!([
            {"unit": "preservation", "value": "preserve", "confidence_ppm": 1000000, "reason": "preserved", "evidence_refs": []},
            {"unit": "task_scope", "value": "preserve", "confidence_ppm": 1000000, "reason": "preserved", "evidence_refs": []}
        ])
    );
    assert_eq!(
        response["outcome"]["obligations"],
        json!([
            {"id": "audit", "critical": true, "action": {"kind": "audit", "parameters": {}}}
        ])
    );
}

#[test]
fn malformed_unknown_version_and_oversize_input_fail_without_leaking_content() {
    let secret = "synthetic-secret-do-not-echo-7d9c";
    let malformed = format!("{{\"task\":\"{secret}\", BROKEN");
    assert_error(
        run(&["check", "-"], Some(malformed.as_bytes())),
        "invalid_json",
        secret,
    );

    let mut request: Value =
        serde_json::from_slice(include_bytes!("../examples/preservation-allow.json")).unwrap();
    request["task"]["text"] = json!(secret);
    request["api_version"] = json!(format!("unsupported-{secret}"));
    assert_error(
        run(
            &["check", "-"],
            Some(&serde_json::to_vec(&request).unwrap()),
        ),
        "unsupported_version",
        secret,
    );

    request["api_version"] = json!("semantic-arbiter/v1alpha1");
    request["task"]["raw_secret"] = json!(secret);
    assert_error(
        run(
            &["check", "-"],
            Some(&serde_json::to_vec(&request).unwrap()),
        ),
        "invalid_json",
        secret,
    );

    let mut oversize = secret.as_bytes().to_vec();
    oversize.resize(MAX_MESSAGE_BYTES * 2, b' ');
    assert_error(
        run(&["check", "-"], Some(&oversize)),
        "message_too_large",
        secret,
    );
}

#[test]
fn bad_arguments_and_missing_files_do_not_echo_their_values() {
    let secret = "synthetic-secret-missing-file-9ab2";
    assert_error(run(&[secret], None), "invalid_arguments", secret);
    assert_error(run(&["replay", "-"], None), "invalid_arguments", "replay");
    let missing = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(format!("{secret}.json"));
    assert!(!missing.exists());
    assert_error(
        run(&["check", missing.to_str().unwrap()], None),
        "input_error",
        secret,
    );
}
