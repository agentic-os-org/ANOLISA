//! Provider tool status must survive the real Python-to-analysis handoff.

use agentsight_opt::atif::AtifTrajectory;
use agentsight_opt::perf::compute_stats;
use std::path::Path;
use std::process::Command;

fn analyzed_error(flag: Option<bool>, content: &str) -> bool {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/batch-analyze.py");
    let fixture = r#"
import importlib.util, json, pathlib, sys, tempfile
spec = importlib.util.spec_from_file_location("batch", sys.argv[1])
batch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(batch)
reply = {"type": "tool_result", "tool_use_id": "a", "content": sys.argv[3]}
flag = json.loads(sys.argv[2])
if flag is not None:
    reply["is_error"] = flag
events = [
    {"type": "assistant", "timestamp": "2026-01-01T00:00:00Z", "message": {
        "role": "assistant", "content": [{"type": "tool_use", "id": "a", "name": "Bash", "input": {}}]}},
    {"type": "user", "message": {"role": "user", "content": [reply]}},
    {"type": "assistant", "timestamp": "2026-01-01T00:00:30Z", "message": {
        "role": "assistant", "content": "Done."}},
]
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory) / "session.jsonl"
    path.write_text("".join(json.dumps(event) + "\n" for event in events), encoding="utf-8")
    print(json.dumps(batch.jsonl_to_atif(path)))
"#;
    let output = Command::new("python3")
        .args(["-c", fixture])
        .arg(script)
        .arg(serde_json::to_string(&flag).unwrap())
        .arg(content)
        .output()
        .expect("python3 must be available for the batch converter");
    assert!(
        output.status.success(),
        "batch fixture failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let json = String::from_utf8(output.stdout).unwrap();
    let trajectory = AtifTrajectory::from_json(&json).unwrap();
    let stats = compute_stats(&trajectory).unwrap();
    assert_eq!(stats.tool_calls.len(), 1);
    stats.tool_calls[0].err
}

#[test]
fn batch_failure_flag_reaches_the_analysis_result() {
    assert!(analyzed_error(Some(true), "Command exited with status 1"));
}

#[test]
fn batch_explicit_success_overrides_error_words() {
    assert!(!analyzed_error(
        Some(false),
        "Error: recovered; operation completed"
    ));
}

#[test]
fn batch_unknown_status_keeps_the_text_fallback() {
    assert!(analyzed_error(None, "Error: request timed out"));
}
