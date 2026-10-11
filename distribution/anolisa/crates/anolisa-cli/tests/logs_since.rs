//! Exercise RFC3339 lower bounds through the real CLI and an owned JSONL log.

use std::path::Path;

use serde_json::{Value, json};

mod common;

fn query(prefix: &Path, since: Option<&str>, limit: Option<&str>) -> Value {
    let mut arguments = vec![
        "--install-mode",
        "system",
        "--prefix",
        prefix.to_str().expect("prefix"),
        "--json",
        "logs",
    ];
    if let Some(since) = since {
        arguments.extend(["--since", since]);
    }
    if let Some(limit) = limit {
        arguments.extend(["--limit", limit]);
    }
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
        .map(|row| row["operation_id"].as_str().expect("operation id"))
        .collect()
}

#[test]
fn logs_since_preserves_fractional_matches_and_offset_equivalence() {
    let temporary = tempfile::tempdir().expect("prefix");
    let path = temporary.path().join("var/log/anolisa/central.jsonl");
    std::fs::create_dir_all(path.parent().expect("log parent")).expect("create log parent");
    let mut contents = String::new();
    for (id, timestamp) in [
        ("before", "2026-10-01T00:29:59Z"),
        ("at", "2026-10-01T00:30:00Z"),
        ("fraction", "2026-10-01T00:30:00.100Z"),
        ("after", "2026-10-01T00:30:01Z"),
        ("invalid", "not-a-time"),
    ] {
        let record = json!({
            "kind": "operation", "operation_id": id, "command": "test",
            "source": "fixture", "severity": "info", "message": "owned test record",
            "actor": "test", "started_at": timestamp, "status": "ok",
        });
        contents.push_str(&record.to_string());
        contents.push('\n');
    }
    std::fs::write(&path, &contents).expect("write log");
    assert_eq!(
        ids(&query(temporary.path(), None, None)),
        ["before", "at", "fraction", "after", "invalid"]
    );
    for since in [
        "2026-10-01T00:30:00Z",
        "2026-10-01T00:30:00.000Z",
        "2026-10-01T08:30:00+08:00",
        "2026-09-30T19:30:00-05:00",
    ] {
        assert_eq!(
            ids(&query(temporary.path(), Some(since), None)),
            ["at", "fraction", "after"],
            "since {since}"
        );
    }
    assert_eq!(
        ids(&query(
            temporary.path(),
            Some("2026-10-01T00:30:00.100Z"),
            None
        )),
        ["fraction", "after"]
    );
    assert_eq!(
        ids(&query(
            temporary.path(),
            Some("2026-10-01T00:30:00Z"),
            Some("2")
        )),
        ["fraction", "after"]
    );
    assert_eq!(std::fs::read_to_string(path).expect("reread log"), contents);
}
