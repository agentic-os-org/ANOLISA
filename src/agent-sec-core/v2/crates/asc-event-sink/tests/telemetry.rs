//! Filesystem gates and append semantics without changing host telemetry policy.
use asc_event_sink::telemetry::TelemetryWriter;
use asc_telemetry::{
    ScanTelemetryInput, TelemetryRecord, TelemetryStatus, config::TelemetryConfig,
};
use rustix::fs::{FlockOperation, flock};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::symlink;

fn record() -> TelemetryRecord {
    TelemetryRecord::for_scan(&ScanTelemetryInput {
        event_type: "code_scan",
        category: "code_scan",
        succeeded: true,
        timestamp: "2026-09-16T00:00:00+00:00",
        result: json!({"verdict":"deny"}).as_object().unwrap(),
        error_type: "",
        exit_code: Some(0),
        agent_name: None,
    })
}

#[test]
fn never_creates_target_directory_file_or_lock_files() {
    let dir = tempfile::tempdir().unwrap();
    let config = TelemetryConfig {
        path: dir.path().join("absent/telemetry.jsonl"),
        disabled_sentinel: dir.path().join("disabled"),
    };
    let writer = TelemetryWriter::new(config);
    assert_eq!(writer.write(&record()), TelemetryStatus::Skipped);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn target_symlinks_are_rejected_without_modifying_or_creating_referents() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let path = dir.path().join("telemetry.jsonl");
    fs::write(&target, b"unchanged").unwrap();
    symlink(&target, &path).unwrap();
    let writer = TelemetryWriter::new(TelemetryConfig {
        path,
        disabled_sentinel: dir.path().join("disabled"),
    });
    assert!(!writer.enabled());
    assert_eq!(writer.write(&record()), TelemetryStatus::Failed);
    assert_eq!(fs::read(&target).unwrap(), b"unchanged");

    fs::remove_file(&target).unwrap();
    assert!(!writer.enabled());
    assert_eq!(writer.write(&record()), TelemetryStatus::Failed);
    assert!(!target.exists());
}

#[test]
fn symlink_replacement_after_enabled_cannot_redirect_append() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("telemetry.jsonl");
    let target = dir.path().join("target");
    fs::write(&path, b"").unwrap();
    fs::write(&target, b"unchanged").unwrap();
    let writer = TelemetryWriter::new(TelemetryConfig {
        path: path.clone(),
        disabled_sentinel: dir.path().join("disabled"),
    });
    assert!(writer.enabled());
    fs::rename(&path, dir.path().join("old")).unwrap();
    symlink(&target, &path).unwrap();
    assert_eq!(writer.write(&record()), TelemetryStatus::Failed);
    assert_eq!(fs::read(&target).unwrap(), b"unchanged");
    assert!(fs::read(dir.path().join("old")).unwrap().is_empty());

    fs::remove_file(&path).unwrap();
    fs::write(&path, b"").unwrap();
    assert!(writer.enabled());
    assert_eq!(writer.write(&record()), TelemetryStatus::Written);
    assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1);
}

#[test]
fn gate_is_rechecked_and_a_dangling_sentinel_symlink_disables_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("telemetry.jsonl");
    fs::write(&path, b"").unwrap();
    let sentinel = dir.path().join("disabled");
    let writer = TelemetryWriter::new(TelemetryConfig {
        path: path.clone(),
        disabled_sentinel: sentinel.clone(),
    });
    assert_eq!(writer.write(&record()), TelemetryStatus::Written);
    fs::write(&sentinel, b"").unwrap();
    assert_eq!(writer.write(&record()), TelemetryStatus::Skipped);
    fs::remove_file(&sentinel).unwrap();
    symlink(dir.path().join("missing"), &sentinel).unwrap();
    assert_eq!(writer.write(&record()), TelemetryStatus::Skipped);
    fs::remove_file(&sentinel).unwrap();
    assert_eq!(writer.write(&record()), TelemetryStatus::Written);
    let text = fs::read_to_string(path).unwrap();
    assert!(text.ends_with('\n'));
    assert_eq!(text.lines().count(), 2);
    for line in text.lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn sentinel_stat_errors_disable_and_invalid_targets_fail_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file");
    fs::write(&path, b"").unwrap();
    let writer = TelemetryWriter::new(TelemetryConfig {
        path: path.clone(),
        disabled_sentinel: path.join("not-a-directory"),
    });
    assert_eq!(writer.write(&record()), TelemetryStatus::Skipped);
    let writer = TelemetryWriter::new(TelemetryConfig {
        path: dir.path().to_path_buf(),
        disabled_sentinel: dir.path().join("disabled"),
    });
    assert_eq!(writer.write(&record()), TelemetryStatus::Failed);
}

#[test]
fn locked_target_is_skipped_then_reopens_by_path_after_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("telemetry.jsonl");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    flock(&file, FlockOperation::LockExclusive).unwrap();
    let writer = TelemetryWriter::new(TelemetryConfig {
        path: path.clone(),
        disabled_sentinel: dir.path().join("disabled"),
    });
    assert_eq!(writer.write(&record()), TelemetryStatus::Skipped);
    drop(file);
    assert_eq!(writer.write(&record()), TelemetryStatus::Written);
    fs::rename(&path, dir.path().join("old")).unwrap();
    assert_eq!(writer.write(&record()), TelemetryStatus::Skipped);
    assert!(!path.exists());
    fs::write(&path, b"").unwrap();
    assert_eq!(writer.write(&record()), TelemetryStatus::Written);
    assert_eq!(fs::read_to_string(path).unwrap().lines().count(), 1);
}

#[test]
fn concurrent_calls_never_interleave_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("telemetry.jsonl");
    fs::write(&path, b"").unwrap();
    let writer = std::sync::Arc::new(TelemetryWriter::new(TelemetryConfig {
        path: path.clone(),
        disabled_sentinel: dir.path().join("disabled"),
    }));
    let workers: Vec<_> = (0..16)
        .map(|_| {
            let writer = writer.clone();
            std::thread::spawn(move || writer.write(&record()))
        })
        .collect();
    let written = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .filter(|s| *s == TelemetryStatus::Written)
        .count();
    let text = fs::read_to_string(path).unwrap();
    assert_eq!(text.lines().count(), written);
    for line in text.lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
}
