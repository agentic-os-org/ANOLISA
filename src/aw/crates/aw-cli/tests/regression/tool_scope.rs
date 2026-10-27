//! Selector fixtures share actual dispatch records and durable native-event assertions.

use crate::support::{Fixture, Service, TIMEOUT};
use serde_json::{json, Value};
use std::{fs, path::Path, thread, time::Instant};

/// Register a poison native hook or an instrumented structured policy.
pub fn step(
    fixture: &Fixture,
    document: &mut Value,
    event: &str,
    id: &str,
    tools: &[&str],
    behavior: &str,
) {
    let native = behavior == "native-error";
    document["spec"]["providers"][id] = json!({
        "protocol": if native { "native-hook/v1alpha1" } else { "aw-provider/v1alpha1" },
        "transport":{"type":"stdio","location":"agent","argv":[fixture.python,
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tool_scope/policy.py"),
            fixture.root,id,behavior]},
        "timeout_ms":2000,"max_output_bytes":4096,"config":{}
    });
    if document["spec"]["events"][event].is_null() {
        document["spec"]["events"][event] = json!({"enabled":true,"steps":[]});
    }
    let before = event == "tool.before";
    let mut step = json!({"id":id,"provider":id,"match":{"tools":tools},
        "on_error":if before { "block" } else { "report" }});
    if native {
        step["native"] = json!({});
    } else {
        step["operation"] = json!(if before { "check" } else { "record" });
        step["effects"] = if before {
            json!(["observe", "block"])
        } else if behavior == "replace" {
            json!(["replace_result"])
        } else {
            json!(["observe"])
        };
    }
    document["spec"]["events"][event]["steps"]
        .as_array_mut()
        .unwrap()
        .push(step);
}

/// Read process-side invocation records, excluding Provider preparation calls.
pub fn invocations(fixture: &Fixture) -> Vec<Value> {
    let path = fixture.root.join("scope-invocations.jsonl");
    if !path.exists() {
        return Vec::new();
    }
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Wait for automatic closure without releasing the current binding.
pub fn drained(service: &Service) {
    let deadline = Instant::now() + TIMEOUT;
    while service.status()["events"] != 0 {
        assert!(
            Instant::now() < deadline,
            "scoped native event did not close"
        );
        thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Check ordered durable claims, successful skip metadata and terminal state.
pub fn audit(fixture: &Fixture, event: &str, expected: &[(&str, bool)], terminal: &str) {
    let journals: Vec<Vec<Value>> = fs::read_dir(fixture.root.join("state/journal"))
        .unwrap()
        .map(|entry| {
            fs::read_to_string(entry.unwrap().path())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap()["record"].clone())
                .collect()
        })
        .filter(|records: &Vec<Value>| {
            records[0]["plan"]["kind"] == "event" && records[0]["plan"]["event"]["name"] == event
        })
        .collect();
    assert_eq!(journals.len(), 1, "{event}: {journals:?}");
    let records = &journals[0];
    let started: Vec<&str> = records
        .iter()
        .filter(|record| record["phase"] == "started")
        .map(|record| record["step_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        started,
        expected.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    );
    let completed: Vec<_> = records
        .iter()
        .filter(|record| record["phase"] == "completed")
        .collect();
    assert_eq!(completed.len(), expected.len());
    for (record, (id, skipped)) in completed.iter().zip(expected) {
        assert_eq!(record["call"]["step_id"], *id);
        assert_eq!(record["status"], "ok");
        assert_eq!(record["kind"] == "skipped", *skipped, "{id}: {record}");
        if *skipped {
            assert_eq!(record["call"]["process"], Value::Null);
            assert_eq!(record["failure"], Value::Null);
            assert_eq!(record["failure_action"], Value::Null);
            assert_eq!(record["policy_block"], false);
        }
    }
    assert_eq!(records.last().unwrap()["phase"], "closed");
    assert_eq!(records.last().unwrap()["status"], terminal);
}
