//! Stream-wrapper behaviour for the two `JSONL` writers.
//!
//! Covers v1 `security_events/test_writer.py` and the `JSONL` half of
//! `observability/test_writer.py`. Every case injects an explicit path, so no
//! test reads or mutates process environment.

use std::fs;

use asc_event_log::{
    DEFAULT_OBSERVABILITY_BACKUP_COUNT, DEFAULT_OBSERVABILITY_MAX_BYTES, ObservabilityWriter,
    SecurityEventWriter, is_backup_suffix,
};
use asc_observability::{ObservabilityHook, ObservabilityMetadata, ObservabilityRecord};
use asc_security_events::SecurityEvent;
use serde_json::{Map, Value, json};
use tempfile::TempDir;

fn lines(path: &std::path::Path) -> Vec<Value> {
    fs::read_to_string(path)
        .expect("log must exist")
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line must be JSON"))
        .collect()
}

fn sample_event() -> SecurityEvent {
    let mut details = Map::new();
    details.insert("verdict".to_owned(), json!("allow"));
    details.insert("note".to_owned(), json!("中文"));
    let mut event = SecurityEvent::new("code_scan", "scan", details);
    "trace-1".clone_into(&mut event.trace_id);
    event.session_id = Some("session-1".to_owned());
    event
}

fn sample_record(hook: ObservabilityHook) -> ObservabilityRecord {
    let observed_at =
        chrono::DateTime::parse_from_rfc3339("2024-01-02T03:04:05.123456+00:00").expect("valid");
    // Pick a metric the hook actually declares, so the same helper works for
    // every hook without tripping the allowlist.
    let metric = hook.metric_names()[0];
    let mut metrics = Map::new();
    metrics.insert(metric.to_owned(), json!("中文"));
    ObservabilityRecord::new(
        hook,
        observed_at,
        ObservabilityMetadata::new("session-1", "run-1"),
        metrics,
    )
    .expect("valid record")
}

#[test]
fn security_event_writer_round_trips_through_jsonl() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("security-events.jsonl");
    let writer = SecurityEventWriter::new(&path);

    let event = sample_event();
    writer.write_or_raise(&event).expect("write");

    let written = lines(&path);
    assert_eq!(written.len(), 1);
    let parsed: SecurityEvent =
        serde_json::from_value(written[0].clone()).expect("re-parsable as an event");
    assert_eq!(parsed, event);
}

#[test]
fn security_event_writer_defaults_match_v1() {
    let dir = TempDir::new().expect("temp dir");
    let writer = SecurityEventWriter::new(dir.path().join("security-events.jsonl"));
    assert_eq!(writer.inner().max_bytes(), 100 * 1024 * 1024);
    assert_eq!(writer.inner().backup_count(), 10);
    assert_eq!(writer.inner().error_prefix(), "[security_events]");
}

#[test]
fn security_event_writer_never_fails_the_caller() {
    let dir = TempDir::new().expect("temp dir");
    // A directory where the log file belongs makes every open fail.
    let path = dir.path().join("security-events.jsonl");
    fs::create_dir(&path).expect("seed directory");

    let writer = SecurityEventWriter::new(&path);
    writer.write(&sample_event());

    writer
        .write_or_raise(&sample_event())
        .expect_err("write_or_raise still surfaces the failure");
}

#[test]
fn observability_writer_round_trips_through_jsonl() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.jsonl");
    let writer = ObservabilityWriter::new(&path);

    let record = sample_record(ObservabilityHook::BeforeAgentRun);
    writer.write(&record).expect("write");

    let written = lines(&path);
    assert_eq!(written.len(), 1);
    let parsed: ObservabilityRecord =
        serde_json::from_value(written[0].clone()).expect("re-parsable as a record");
    assert_eq!(parsed, record);
}

#[test]
fn observability_writer_defaults_match_v1() {
    let dir = TempDir::new().expect("temp dir");
    let writer = ObservabilityWriter::new(dir.path().join("observability.jsonl"));
    assert_eq!(writer.inner().max_bytes(), DEFAULT_OBSERVABILITY_MAX_BYTES);
    assert_eq!(
        writer.inner().backup_count(),
        DEFAULT_OBSERVABILITY_BACKUP_COUNT
    );
    assert_eq!(writer.inner().error_prefix(), "[observability]");
    assert_eq!(DEFAULT_OBSERVABILITY_MAX_BYTES, 256 * 1024 * 1024);
    assert_eq!(DEFAULT_OBSERVABILITY_BACKUP_COUNT, 3);
}

/// v1 `ObservabilityWriter.write()` delegates to `write_or_raise()`, so unlike
/// the security-events stream it surfaces failures to the caller.
#[test]
fn observability_writer_surfaces_failures() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("observability.jsonl");
    fs::create_dir(&path).expect("seed directory");

    let writer = ObservabilityWriter::new(&path);
    writer
        .write(&sample_record(ObservabilityHook::AfterAgentRun))
        .expect_err("observability JSONL writes are not fire-and-forget");
}

#[test]
fn both_streams_rotate_with_mutually_recognizable_backup_names() {
    let dir = TempDir::new().expect("temp dir");
    let sec_path = dir.path().join("security-events.jsonl");
    let obs_path = dir.path().join("observability.jsonl");

    let sec = SecurityEventWriter::new(&sec_path).with_max_bytes(8);
    let obs = ObservabilityWriter::new(&obs_path).with_max_bytes(8);
    for _ in 0..3 {
        sec.write_or_raise(&sample_event()).expect("write");
        obs.write(&sample_record(ObservabilityHook::BeforeLlmCall))
            .expect("write");
    }

    let mut rotated = 0_usize;
    for entry in fs::read_dir(dir.path()).expect("readable") {
        let name = entry
            .expect("entry")
            .file_name()
            .into_string()
            .expect("utf8");
        for base in ["security-events.jsonl.", "observability.jsonl."] {
            if let Some(suffix) = name.strip_prefix(base)
                && is_backup_suffix(suffix)
            {
                rotated += 1;
            }
        }
    }
    assert!(rotated >= 2, "both streams should have rotated");
}
