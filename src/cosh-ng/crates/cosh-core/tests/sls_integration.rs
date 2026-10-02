use std::io::{Read, Write};
use std::process::{Command, Stdio};

use serde_json::Value;

mod common;

/// End-to-end: cosh-core writes a SLS JSONL record after handling a user message.
#[test]
fn user_message_produces_sls_record() {
    if common::system_telemetry_is_disabled() {
        eprintln!("skipping enabled-telemetry test because the host opted out system-wide");
        return;
    }

    let home = tempfile::tempdir().expect("temp home");
    let sls_dir = home.path().join("sls");
    std::fs::create_dir_all(&sls_dir).unwrap();
    let sls_file = sls_dir.join("cosh.jsonl");
    // Pre-create the file (platform provisioning)
    std::fs::write(&sls_file, "").unwrap();

    // Unified-channel test: the pre-created cosh.jsonl file prevents any
    // standalone self-upload, so keep the per-user sentinel absent. A real
    // system-level opt-out is checked above and never bypassed.
    let absent_user_sentinel = home.path().join("telemetry_disabled_absent");

    let mut child = Command::new(common::binary_path())
        .env("HOME", home.path())
        .env("COSH_TELEMETRY_DISABLED_PATH", &absent_user_sentinel)
        .env("COSH_SLS_LOG_PATH", sls_file.to_str().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("Failed to spawn {}: {e}", common::binary_path().display()));

    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, r#"{{"type":"control_request","request_id":"init-1","request":{{"subtype":"initialize"}}}}"#).unwrap();
        writeln!(stdin, r#"{{"type":"user","message":{{"role":"user","content":"say hello"}},"parent_tool_use_id":null}}"#).unwrap();
        writeln!(stdin, r#"{{"type":"control_request","request_id":"shut-1","request":{{"subtype":"shutdown"}}}}"#).unwrap();
        stdin.flush().unwrap();
    }

    let output = child.wait_with_output().unwrap();
    let initialized_session = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|message| message["type"] == "system" && message["subtype"] == "init")
        .and_then(|message| message["session_id"].as_str().map(str::to_string))
        .expect("initialized session id");

    let mut content = String::new();
    std::fs::File::open(&sls_file)
        .unwrap()
        .read_to_string(&mut content)
        .unwrap();

    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        !lines.is_empty(),
        "expected at least 1 SLS JSONL line, file was empty"
    );

    let record: Value = serde_json::from_str(lines[0]).expect("SLS record should be valid JSON");

    // Verify key fields
    assert_eq!(record["component.name"], "cosh");
    assert_eq!(record["component.agent_name"], "cosh-ng");
    assert_eq!(record["session.id"], initialized_session);
    assert!(record["component.version"].is_string());

    // Verify all numeric fields exist
    assert!(record["session.tokens.input"].is_number());
    assert!(record["session.tokens.output"].is_number());
    assert!(record["session.tokens.total"].is_number());
    assert!(record["session.api.total_requests"].is_number());
    assert!(record["session.tool_call_counts.total"].is_number());
    assert!(record["session.audit_decision_counts.approve"].is_number());

    // Verify environment fields
    assert!(record["os.type"].is_string());
    assert!(record["os.arch"].is_string());
}

/// SLS file is not written when the file does not exist (no O_CREAT).
#[test]
fn sls_not_created_when_missing() {
    let home = tempfile::tempdir().expect("temp home");
    let sls_file = home.path().join("nonexistent-sls.jsonl");

    let mut child = common::cosh_core_command(home.path())
        .env("COSH_SLS_LOG_PATH", sls_file.to_str().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, r#"{{"type":"control_request","request_id":"init-1","request":{{"subtype":"initialize"}}}}"#).unwrap();
        writeln!(stdin, r#"{{"type":"user","message":{{"role":"user","content":"hi"}},"parent_tool_use_id":null}}"#).unwrap();
        writeln!(stdin, r#"{{"type":"control_request","request_id":"shut-1","request":{{"subtype":"shutdown"}}}}"#).unwrap();
        stdin.flush().unwrap();
    }

    let _output = child.wait_with_output().unwrap();

    assert!(
        !sls_file.exists(),
        "SLS file should NOT be created when it doesn't exist (no O_CREAT)"
    );
}
