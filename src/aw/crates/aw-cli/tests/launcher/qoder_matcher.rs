//! Regression tests for Qoder matcher validation and failure-action preservation.

use super::super::{Fixture, Service};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn run_single_bash_probe(on_error: &str) -> (Value, String) {
    let fixture = Fixture::new();
    let provider_log = fixture.root.join("provider-calls.log");
    let report_path = fixture.root.join("qoder-report.json");
    let provider_script = fixture.root.join("mock_provider.py");
    let agent_script = fixture.root.join("mock_qoder_agent.py");

    fs::write(
        &provider_script,
        r#"import json, pathlib, sys
req = json.loads(sys.stdin.read())
method = req.get("method", "invoke")
if method == "describe":
    resp = {"api_version": req["api_version"], "request_id": req["request_id"], "status": "ok",
            "operations": [{"name": "check", "events": ["tool.before"], "effects": ["observe", "block"]}]}
elif method == "validate_config":
    resp = {"api_version": req["api_version"], "request_id": req["request_id"], "status": "ok"}
else:
    with pathlib.Path(sys.argv[1]).open("a") as fh:
        fh.write(req["event"]["tool"]["name"] + "\n")
    resp = {"api_version": req["api_version"], "request_id": req["request_id"], "status": "ok",
            "input_digest": req["input_digest"],
            "effects": [{"type": "observe", "reason_code": "ok"}]}
sys.stdout.write(json.dumps(resp) + "\n")
"#,
    )
    .unwrap();
    fs::set_permissions(&provider_script, fs::Permissions::from_mode(0o700)).unwrap();

    fs::write(
        &agent_script,
        format!(
            "#!{}\n{}",
            fixture.python.display(),
            r#"import json, os, pathlib, subprocess, sys
if sys.argv[1:] == ["--version"]:
    print("1.1.64")
    sys.exit(0)
settings_path = pathlib.Path(sys.argv[sys.argv.index("--settings") + 1])
settings = json.loads(settings_path.read_text())
groups = settings["hooks"]["PreToolUse"]
assert len(groups) == 1
matcher = groups[0]["matcher"]
hook = groups[0]["hooks"][0]

def fire(tool_name, call_id, extra=None):
    if matcher not in ("*", tool_name):
        return 0, ""
    payload = {
        "hook_event_name": "PreToolUse",
        "session_id": "sess-1",
        "cwd": os.getcwd(),
        "tool_use_id": call_id,
        "tool_name": tool_name,
        "tool_input": {"command": "echo hi"} if tool_name == "Bash" else {"file_path": "a.txt"},
    }
    if extra:
        payload.update(extra)
    res = subprocess.run([hook["command"], *hook["args"]], input=json.dumps(payload).encode(), capture_output=True)
    return res.returncode, res.stderr.decode()

read_miss, _ = fire("Read", "call-read-miss")
malformed_read, malformed_stderr = fire("Read", "call-read-malformed", {"tool_response": {"premature": True}})
bash_match, _ = fire("Bash", "call-bash-match")
pathlib.Path(sys.argv[sys.argv.index("--report") + 1]).write_text(
    json.dumps({
        "matcher": matcher,
        "read_miss": read_miss,
        "malformed_read": malformed_read,
        "malformed_stderr": malformed_stderr,
        "bash_match": bash_match,
    })
)
"#,
        ),
    )
    .unwrap();
    fs::set_permissions(&agent_script, fs::Permissions::from_mode(0o700)).unwrap();

    let mut document = fixture.document();
    document["spec"]["providers"]["mock"] = json!({
        "protocol": "aw-provider/v1alpha1",
        "transport": {
            "type": "stdio",
            "location": "agent",
            "argv": [fixture.python, provider_script, provider_log],
        },
        "timeout_ms": 2000,
        "max_output_bytes": 4096,
        "config": {}
    });
    document["spec"]["events"] = json!({
        "tool.before": {
            "enabled": true,
            "required": true,
            "steps": [{
                "id": "bash-only",
                "match": {"tools": ["native:qoder:Bash"]},
                "provider": "mock",
                "operation": "check",
                "effects": ["observe", "block"],
                "on_error": on_error
            }]
        }
    });
    document["spec"]["agents"]["qoder"]["argv"] = json!([agent_script, "--report", report_path]);

    let service = Service::start(&fixture, &document);
    let out = fixture.run(fixture.launch(&[], None));
    assert!(
        out.status.success(),
        "ordinary aw run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = json_from(&report_path);
    let calls = fs::read_to_string(&provider_log).unwrap();
    service.released(&fixture);
    (report, calls)
}

#[test]
fn ordinary_aw_run_single_bash_block_keeps_wildcard_matcher_and_blocks_malformed_read() {
    let (report, calls) = run_single_bash_probe("block");
    assert_eq!(report["matcher"], "*");
    assert_eq!(report["read_miss"], 0);
    assert_eq!(report["malformed_read"], 2);
    assert_eq!(report["bash_match"], 0);
    assert_eq!(calls.lines().collect::<Vec<_>>(), vec!["Bash"]);
}

#[test]
fn ordinary_aw_run_single_bash_report_preserves_malformed_failure_diagnostic() {
    let (report, calls) = run_single_bash_probe("report");
    assert_eq!(report["matcher"], "*");
    assert_eq!(report["read_miss"], 0);
    assert_eq!(report["malformed_read"], 0);
    assert_eq!(report["bash_match"], 0);
    assert_eq!(calls.lines().collect::<Vec<_>>(), vec!["Bash"]);
    assert!(
        report["malformed_stderr"]
            .as_str()
            .unwrap()
            .contains("step execution failed"),
        "missing malformed report diagnostic: {report}"
    );
}

fn json_from(path: &std::path::Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}
