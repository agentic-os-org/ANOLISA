//! Bidirectional `JSONL` compatibility with v1, at the line level.
//!
//! The v1 lines below were produced by a live v1 run through
//! `SecurityEventWriter` / `ObservabilityWriter`, i.e. only through public v1
//! API. This test proves the two directions required by the contract layer:
//!
//! 1. v1-written lines parse into v2 types with every field preserved.
//! 2. v2-written lines are re-readable, and are emitted to
//!    `target/cp1-v2-jsonl/` so the v1 side can parse them in turn.
//!
//! Note that the two encoders are *not* byte-identical: v1 `json.dumps` uses
//! `", "` / `": "` separators and preserves object insertion order, while
//! `serde_json` is compact and orders object keys. Both remain valid JSON and
//! parse to equal values, which is the documented comparison basis.

use std::fs;
use std::path::PathBuf;

use asc_event_log::{ObservabilityWriter, SecurityEventWriter};
use asc_observability::{ObservabilityHook, ObservabilityMetadata, ObservabilityRecord};
use asc_security_events::{EventResult, SecurityEvent};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

const V1_SECURITY_LINES: &[&str] = &[
    r#"{"event_id": "9315e503-15dd-467f-94b8-463aed12bcef", "event_type": "code_scan", "category": "scan", "result": "succeeded", "timestamp": "2024-01-02T03:04:05.123456+00:00", "trace_id": "trace-1", "pid": 77931, "uid": 502, "session_id": "session-1", "run_id": null, "call_id": null, "tool_call_id": null, "details": {"verdict": "allow", "note": "中文", "nested": {"b": 1, "a": 2}}}"#,
    r#"{"event_id": "f6c34131-9db1-4f96-a89c-dcfeefe4693f", "event_type": "sandbox", "category": "exec", "result": "failed", "timestamp": "2024-01-02T03:04:05+00:00", "trace_id": "", "pid": 77931, "uid": 502, "session_id": null, "run_id": "run-1", "call_id": "call-1", "tool_call_id": "tool-1", "details": {}}"#,
];

const V1_OBSERVABILITY_LINES: &[&str] = &[
    r#"{"hook": "before_agent_run", "observedAt": "2024-01-02T03:04:05.123456Z", "metadata": {"sessionId": "s1", "runId": "r1"}, "metrics": {"prompt": "p", "user_input": "中文", "images_count": 2}}"#,
    r#"{"hook": "after_tool_call", "observedAt": "2024-01-02T03:04:05+08:00", "metadata": {"sessionId": "s1", "runId": "r1", "toolCallId": "t1", "callId": "c1"}, "metrics": {"result": null, "status": "ok", "exit_code": 0}}"#,
];

#[test]
fn v1_security_event_lines_parse_with_every_field_preserved() {
    let first: SecurityEvent = serde_json::from_str(V1_SECURITY_LINES[0]).expect("parsable");
    assert_eq!(first.event_id, "9315e503-15dd-467f-94b8-463aed12bcef");
    assert_eq!(first.event_type, "code_scan");
    assert_eq!(first.category, "scan");
    assert_eq!(first.result, EventResult::Succeeded);
    assert_eq!(first.timestamp, "2024-01-02T03:04:05.123456+00:00");
    assert_eq!(first.trace_id, "trace-1");
    assert_eq!(first.pid, 77931);
    assert_eq!(first.uid, 502);
    assert_eq!(first.session_id.as_deref(), Some("session-1"));
    assert_eq!(first.run_id, None);
    assert_eq!(first.call_id, None);
    assert_eq!(first.tool_call_id, None);
    assert_eq!(first.details.get("note"), Some(&json!("中文")));
    assert_eq!(
        first.details.get("nested"),
        Some(&json!({"a": 2, "b": 1})),
        "nested payloads survive; only key ordering differs"
    );

    let second: SecurityEvent = serde_json::from_str(V1_SECURITY_LINES[1]).expect("parsable");
    assert_eq!(second.result, EventResult::Failed);
    assert_eq!(second.timestamp, "2024-01-02T03:04:05+00:00");
    assert_eq!(second.trace_id, "");
    assert_eq!(second.run_id.as_deref(), Some("run-1"));
    assert_eq!(second.call_id.as_deref(), Some("call-1"));
    assert_eq!(second.tool_call_id.as_deref(), Some("tool-1"));
    assert!(second.details.is_empty());
}

#[test]
fn v1_observability_lines_parse_with_every_field_preserved() {
    let first: ObservabilityRecord =
        serde_json::from_str(V1_OBSERVABILITY_LINES[0]).expect("parsable");
    assert_eq!(first.hook(), ObservabilityHook::BeforeAgentRun);
    assert_eq!(first.observed_at_iso(), "2024-01-02T03:04:05.123456Z");
    assert_eq!(first.metadata().session_id, "s1");
    assert_eq!(first.metadata().run_id, "r1");
    assert_eq!(first.metrics().get("user_input"), Some(&json!("中文")));
    assert_eq!(first.metrics().get("images_count"), Some(&json!(2)));

    let second: ObservabilityRecord =
        serde_json::from_str(V1_OBSERVABILITY_LINES[1]).expect("parsable");
    assert_eq!(second.hook(), ObservabilityHook::AfterToolCall);
    assert_eq!(
        second.observed_at_iso(),
        "2024-01-02T03:04:05+08:00",
        "a non-UTC offset must be preserved rather than normalized to UTC"
    );
    assert_eq!(second.metadata().tool_call_id.as_deref(), Some("t1"));
    assert_eq!(second.metadata().call_id.as_deref(), Some("c1"));
    assert_eq!(second.metrics().get("result"), Some(&Value::Null));
}

/// Re-encoding a v1 line and parsing it again must be lossless.
#[test]
fn v1_lines_survive_a_v2_round_trip() {
    for line in V1_SECURITY_LINES {
        let event: SecurityEvent = serde_json::from_str(line).expect("parsable");
        let reencoded = serde_json::to_string(&event).expect("serializable");
        let again: SecurityEvent = serde_json::from_str(&reencoded).expect("re-parsable");
        assert_eq!(again, event);
    }
    for line in V1_OBSERVABILITY_LINES {
        let record: ObservabilityRecord = serde_json::from_str(line).expect("parsable");
        let reencoded = serde_json::to_string(&record).expect("serializable");
        let again: ObservabilityRecord = serde_json::from_str(&reencoded).expect("re-parsable");
        assert_eq!(again, record);
    }
}

/// Emits v2-written lines for the v1 side to parse.
///
/// The output directory is inside `target/`, so it is a build artifact rather
/// than committed fixture data.
#[test]
fn v2_written_lines_are_exported_for_the_v1_side() {
    let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cp1-v2-jsonl");
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).expect("create export dir");

    let staging = TempDir::new().expect("temp dir");
    let sec_path = staging.path().join("security-events.jsonl");
    let obs_path = staging.path().join("observability.jsonl");

    let sec = SecurityEventWriter::new(&sec_path);
    for line in V1_SECURITY_LINES {
        let event: SecurityEvent = serde_json::from_str(line).expect("parsable");
        sec.write_or_raise(&event).expect("write");
    }

    let obs = ObservabilityWriter::new(&obs_path);
    for line in V1_OBSERVABILITY_LINES {
        let record: ObservabilityRecord = serde_json::from_str(line).expect("parsable");
        obs.write(&record).expect("write");
    }

    // A record built from scratch exercises the default-filling constructors
    // rather than only the deserialization path.
    let mut details = Map::new();
    details.insert("verdict".to_owned(), json!("deny"));
    let fresh = SecurityEvent::new("prompt_scan", "prompt", details);
    sec.write_or_raise(&fresh).expect("write");

    let mut metrics = Map::new();
    metrics.insert("latency_ms".to_owned(), json!(12));
    let fresh_record = ObservabilityRecord::new(
        ObservabilityHook::AfterLlmCall,
        chrono::DateTime::parse_from_rfc3339("2025-06-07T08:09:10.000123-07:00").expect("valid"),
        ObservabilityMetadata::new("s2", "r2").with_call_id("c2"),
        metrics,
    )
    .expect("valid record");
    obs.write(&fresh_record).expect("write");

    for (source, name) in [
        (&sec_path, "security-events.jsonl"),
        (&obs_path, "observability.jsonl"),
    ] {
        let body = fs::read_to_string(source).expect("written log");
        assert_eq!(body.lines().count(), 3, "expected three lines in {name}");
        fs::write(out_dir.join(name), &body).expect("export");
    }
}
