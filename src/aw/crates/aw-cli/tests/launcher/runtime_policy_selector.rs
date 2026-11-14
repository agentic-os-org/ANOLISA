//! Selector-miss validation and observation regressions for reloadable Qoder hooks.

use super::support::{Fixture, Service};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Output};

fn add_scoped_policy_step(fixture: &Fixture, document: &mut Value, id: &str, tool: &str) {
    let policy_script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tool_scope/policy.py");
    document["spec"]["providers"][id] = json!({
        "protocol": "aw-provider/v1alpha1",
        "transport": {
            "type": "stdio",
            "location": "agent",
            "argv": [fixture.python, policy_script, fixture.root, id, "allow"],
        },
        "timeout_ms": 2000,
        "max_output_bytes": 4096,
        "config": {},
    });
    if document["spec"]["events"]["tool.before"].is_null() {
        document["spec"]["events"]["tool.before"] = json!({"enabled": true, "steps": []});
    }
    document["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": id,
            "provider": id,
            "operation": "check",
            "effects": ["observe", "block"],
            "match": {"tools": [tool]},
            "on_error": "block",
        }));
}

fn run_qoder_hook_probe(
    fixture: &Fixture,
    document: &mut Value,
    events: Vec<Value>,
) -> (Service, Vec<Value>, Output) {
    let probe = fixture.root.join("qoder_probe.py");
    fs::write(
        &probe,
        format!(
            r#"#!{}
import json, os, subprocess, sys
from pathlib import Path
if sys.argv[1:] == ["--version"]:
    print("1.1.73")
    raise SystemExit(0)
settings = json.loads(Path(sys.argv[sys.argv.index("--settings") + 1]).read_text())
root = Path(os.environ["FAKE_ROOT"])
def fire(name, payload):
    results = []
    for group in settings["hooks"].get(name, []):
        for hook in group["hooks"]:
            proc = subprocess.run([hook["command"], *hook["args"]], input=json.dumps(payload).encode(), capture_output=True, timeout=6, check=False)
            results.append({{"event": name, "code": proc.returncode, "stdout": proc.stdout.decode(), "stderr": proc.stderr.decode()}})
    return results
out = []
out.extend(fire("SessionStart", {{"hook_event_name": "SessionStart", "cwd": str(root), "session_id": "s1"}}))
out.extend(fire("UserPromptSubmit", {{"hook_event_name": "UserPromptSubmit", "cwd": str(root), "session_id": "s1", "request_set_id": "req-1", "prompt": "hi"}}))
for item in json.loads(os.environ["PROBE_EVENTS"]):
    out.extend(fire(item["hook_event_name"], item))
(root / "probe-results.json").write_text(json.dumps(out))
"#,
            fixture.python.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o700)).unwrap();
    document["spec"]["agents"]["qoder"]["argv"] = json!([probe]);
    let service = Service::start(fixture, document);
    let mut cmd = fixture.command();
    cmd.args(["run", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder", "--reloadable"])
        .env("PROBE_EVENTS", serde_json::to_string(&events).unwrap());
    let output = fixture.run(cmd);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let results: Vec<Value> =
        serde_json::from_slice(&fs::read(fixture.root.join("probe-results.json")).unwrap())
            .unwrap();
    (service, results, output)
}

#[test]
fn selector_miss_with_non_empty_steps_preserves_schema_validation_and_fail_closed() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    add_scoped_policy_step(&fixture, &mut document, "scan-bash", "native:qoder:Bash");
    add_scoped_policy_step(&fixture, &mut document, "scan-write", "native:qoder:Write");
    let events = vec![json!({
        "hook_event_name": "PreToolUse",
        "cwd": fixture.root,
        "session_id": "s1",
        "tool_use_id": "call-premature",
        "tool_name": "Read",
        "tool_input": {"file": "a.txt"},
        "tool_response": "premature-result",
    })];
    let (service, results, _) = run_qoder_hook_probe(&fixture, &mut document, events);
    let pre = results.iter().find(|r| r["event"] == "PreToolUse").unwrap();
    assert_eq!(pre["code"], 2, "expected fail-closed block, got: {pre}");
    assert!(!fixture.root.join("scope-invocations.jsonl").exists());
    service.released(&fixture);
}

#[test]
fn selector_miss_with_non_empty_steps_and_observation_preserves_evidence() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    add_scoped_policy_step(&fixture, &mut document, "scan-bash", "native:qoder:Bash");
    add_scoped_policy_step(&fixture, &mut document, "scan-write", "native:qoder:Write");
    fixture.observation(&mut document, "qoder");
    let events = vec![
        json!({
            "hook_event_name": "PreToolUse",
            "cwd": fixture.root,
            "session_id": "s1",
            "tool_use_id": "call-read",
            "tool_name": "Read",
            "tool_input": {"file": "a.txt"},
        }),
        json!({
            "hook_event_name": "PostToolUse",
            "cwd": fixture.root,
            "session_id": "s1",
            "tool_use_id": "call-read",
            "tool_name": "Read",
            "tool_input": {"file": "a.txt"},
            "tool_response": "ok",
        }),
    ];
    let (service, results, output) = run_qoder_hook_probe(&fixture, &mut document, events);
    for item in &results {
        assert_eq!(item["code"], 0, "unexpected hook failure: {item}");
        assert!(
            !item["stderr"]
                .as_str()
                .unwrap_or("")
                .contains("AW observation failed"),
            "observation callback failed: {item}"
        );
    }
    assert!(!fixture.root.join("scope-invocations.jsonl").exists());
    let audit = fixture.run_audit(&service, &output);
    let callback_count = audit["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["record"]["phase"] == "native_callback")
        .count();
    assert!(
        callback_count >= 2,
        "expected preserved native_callback observation records, got {audit}"
    );
    assert_eq!(audit["terminal"], true, "audit not terminal: {audit}");
    service.released(&fixture);
}
