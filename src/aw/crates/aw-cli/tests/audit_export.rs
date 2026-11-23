//! The standalone export command remains usable with no control service.

use aw_core::{journal::FileJournal, ports::Journal};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn export_command_reads_and_reports_failures_without_starting_service() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    let mut journal = FileJournal::new(state.join("journal")).unwrap();
    journal
        .claim(&"a".repeat(64), &json!({"legacy":true}))
        .unwrap();
    let source = state.join("journal/.aw-audit-source.json");
    fs::write(&source,serde_json::to_vec(&json!({"apiVersion":"audit.aw/v1alpha1","kind":"AuditSource","sourceId":"b".repeat(64)})).unwrap()).unwrap();
    fs::set_permissions(source, fs::Permissions::from_mode(0o600)).unwrap();
    let request = directory.path().join("request.json");
    let run = || {
        Command::new("timeout")
            .arg("10")
            .arg(env!("CARGO_BIN_EXE_aw"))
            .arg("audit-export")
            .arg("--state-dir")
            .arg(&state)
            .arg("--request")
            .arg(&request)
            .output()
            .unwrap()
    };
    fs::write(&request,serde_json::to_vec(&json!({"api_version":"audit-export.aw/v1alpha1","operation":{"type":"enumerate","cursor":null,"limit":64}})).unwrap()).unwrap();
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["result"]["keys"], json!(["a".repeat(64)]));
    assert!(!state.join("aw.sock").exists());
    fs::write(
        &request,
        br#"{"api_version":"audit-export.aw/v1alpha1","operation":{"type":"stop"}}"#,
    )
    .unwrap();
    let output = run();
    assert_eq!(output.status.code(), Some(1));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"], "invalid_request");
    assert!(!state.join("aw.sock").exists());
}

#[test]
fn export_discovery_and_help_share_the_public_command() {
    for args in [["audit-export", "--help"], ["commands", "--json"]] {
        let output = Command::new("timeout")
            .arg("10")
            .arg(env!("CARGO_BIN_EXE_aw"))
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("audit-export"));
        assert!(text.contains("state-dir"));
        assert!(text.contains("request"));
    }
}
