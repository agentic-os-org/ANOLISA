//! Exercise documented positional log selectors through the real CLI.

use std::path::Path;

use serde_json::{Value, json};

mod common;

fn query(prefix: &Path, selector: Option<&str>, filters: &[&str]) -> Value {
    let mut arguments = vec![
        "--install-mode",
        "system",
        "--prefix",
        prefix.to_str().expect("prefix"),
        "--json",
        "logs",
    ];
    if let Some(selector) = selector {
        arguments.push(selector);
    }
    arguments.extend_from_slice(filters);
    let output = common::run(&arguments);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).expect("JSON response");
    assert_eq!(response["ok"], true);
    assert_eq!(response["command"], "logs");
    response
}

fn ids(response: &Value) -> Vec<&str> {
    response["data"]
        .as_array()
        .expect("records")
        .iter()
        .map(|record| record["operation_id"].as_str().expect("operation id"))
        .collect()
}

fn write_log(prefix: &Path) -> (std::path::PathBuf, String) {
    let path = prefix.join("var/log/anolisa/central.jsonl");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("log directory");
    let records = [
        json!({
            "kind": "operation", "operation_id": "op-older", "command": "install agentsight",
            "source": "fixture", "objects": ["agentsight"], "severity": "info",
            "message": "installed", "actor": "test", "started_at": "2026-06-01T10:00:00Z",
        }),
        json!({
            "kind": "operation", "operation_id": "op-failed", "command": "install tokenless",
            "source": "anolisa-cli", "objects": ["tokenless"], "severity": "error",
            "message": "failed", "actor": "test", "started_at": "2026-06-01T10:00:01Z",
        }),
        json!({
            "kind": "component", "operation_id": "op-later", "command": "report agentsight",
            "source": "anolisa-cli", "component": "agentsight", "severity": "warn",
            "message": "warning", "actor": "test", "started_at": "2026-06-01T10:00:02Z",
        }),
    ];
    let contents = records
        .iter()
        .map(|record| format!("{record}\n"))
        .collect::<String>();
    std::fs::write(&path, &contents).expect("log fixture");
    (path, contents)
}

#[test]
fn positional_selectors_find_operations_sources_components_and_all_records() {
    let prefix = tempfile::tempdir().expect("prefix");
    let (path, original) = write_log(prefix.path());
    for (selector, expected) in [
        (None, vec!["op-older", "op-failed", "op-later"]),
        (Some("all"), vec!["op-older", "op-failed", "op-later"]),
        (Some("op-failed"), vec!["op-failed"]),
        (Some("anolisa-cli"), vec!["op-failed", "op-later"]),
        (Some("agentsight"), vec!["op-older", "op-later"]),
        (Some("tokenless"), vec!["op-failed"]),
        (Some("missing"), vec![]),
    ] {
        assert_eq!(ids(&query(prefix.path(), selector, &[])), expected);
    }
    assert_eq!(std::fs::read_to_string(path).expect("reread"), original);
}

#[test]
fn explicit_filters_and_limits_still_apply_to_positional_selectors() {
    let prefix = tempfile::tempdir().expect("prefix");
    let (path, original) = write_log(prefix.path());
    for (selector, filters, expected) in [
        ("all", vec!["--operation-id", "op-later"], vec!["op-later"]),
        ("all", vec!["--source", "fixture"], vec!["op-older"]),
        (
            "anolisa-cli",
            vec!["--severity", "error"],
            vec!["op-failed"],
        ),
        ("anolisa-cli", vec!["--kind", "component"], vec!["op-later"]),
        ("anolisa-cli", vec!["--limit", "1"], vec!["op-later"]),
        ("all", vec!["--limit", "0"], vec![]),
        (
            "all",
            vec!["--since", "2026-06-01T10:00:02Z"],
            vec!["op-later"],
        ),
        ("op-failed", vec!["--source", "fixture"], vec![]),
    ] {
        assert_eq!(
            ids(&query(prefix.path(), Some(selector), &filters)),
            expected
        );
    }
    assert_eq!(std::fs::read_to_string(path).expect("reread"), original);
}
