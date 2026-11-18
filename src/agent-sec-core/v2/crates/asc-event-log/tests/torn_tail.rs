//! Regression coverage for preserving JSONL record boundaries after an interrupted append.

use std::fs;

use asc_event_log::JsonlEventWriter;
use serde_json::json;
use tempfile::TempDir;

#[test]
fn appending_after_an_unterminated_tail_preserves_new_records() {
    for tail in [
        &b"{\"broken\":"[..],
        &b"{\"text\":\"\xe4\xb8"[..],
        &b"{\"complete\":true}"[..],
    ] {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.jsonl");
        let mut original = b"{\"prior\":1}\n".to_vec();
        original.extend_from_slice(tail);
        fs::write(&path, &original).expect("seed interrupted log");
        let writer = JsonlEventWriter::new(&path);
        writer.write_or_raise(&json!({"new": 1})).expect("append");
        writer
            .write_or_raise(&json!({"new": 2}))
            .expect("append again");
        let mut expected = original;
        expected.extend_from_slice(b"\n{\"new\":1}\n{\"new\":2}\n");
        assert_eq!(fs::read(&path).expect("read log"), expected);
    }
}

#[test]
fn complete_logs_do_not_gain_extra_separators() {
    for initial in [&b""[..], &b"{\"prior\":1}\n"[..]] {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("events.jsonl");
        fs::write(&path, initial).expect("seed log");
        JsonlEventWriter::new(&path)
            .write_or_raise(&json!({"new": 1}))
            .expect("append");
        let mut expected = initial.to_vec();
        expected.extend_from_slice(b"{\"new\":1}\n");
        assert_eq!(fs::read(path).expect("read log"), expected);
    }
}

#[test]
fn separator_counts_toward_rotation_threshold() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.jsonl");
    let original = b"{\"broken\":";
    let line = b"{\"new\":1}\n";
    fs::write(&path, original).expect("seed log");
    let writer =
        JsonlEventWriter::new(&path).with_max_bytes((original.len() + line.len() + 1) as u64);
    writer.write_or_raise(&json!({"new": 1})).expect("append");
    assert_eq!(fs::read(&path).expect("read log"), line);
    let backups: Vec<_> = fs::read_dir(dir.path())
        .expect("read directory")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("events.jsonl.20")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(backups[0].path()).expect("read backup"), original);
}

#[test]
fn separate_writers_recover_the_tail_once() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.jsonl");
    let prefix = b"{\"broken\":";
    fs::write(&path, prefix).expect("seed log");
    std::thread::scope(|scope| {
        for worker in 0..4 {
            let path = &path;
            scope.spawn(move || {
                let writer = JsonlEventWriter::new(path);
                for index in 0..20 {
                    writer
                        .write_or_raise(&json!({"worker": worker, "index": index}))
                        .expect("append");
                }
            });
        }
    });
    let data = fs::read(path).expect("read log");
    assert!(data.starts_with(b"{\"broken\":\n"));
    let records: Vec<serde_json::Value> = data
        .split(|byte| *byte == b'\n')
        .skip(1)
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("valid new record"))
        .collect();
    assert_eq!(records.len(), 80);
    for worker in 0..4 {
        for index in 0..20 {
            assert_eq!(
                records
                    .iter()
                    .filter(|record| **record == json!({"worker": worker, "index": index}))
                    .count(),
                1
            );
        }
    }
}

#[test]
fn legacy_write_only_file_remains_appendable() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.jsonl");
    fs::write(&path, b"{\"broken\":").expect("seed log");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o200)).expect("write-only mode");
    JsonlEventWriter::new(&path)
        .write_or_raise(&json!({"new": 1}))
        .expect("append");
    assert_eq!(
        fs::read(&path).expect("read restored log"),
        b"{\"broken\":\n{\"new\":1}\n"
    );
    assert_eq!(
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn failed_rotation_still_separates_the_original_tail() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.jsonl");
    let errors = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&errors);
    let writer = JsonlEventWriter::new(&path)
        .with_max_bytes(1)
        .with_error_handler(Box::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
    writer.probe().expect("prepare log and lock");
    fs::write(&path, b"{\"broken\":").expect("seed log");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).expect("readonly directory");
    let result = writer.write_or_raise(&json!({"new": 1}));
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("restore directory");
    result.expect("append after failed rotation");
    assert_eq!(errors.load(Ordering::SeqCst), 1);
    assert_eq!(
        fs::read(path).expect("read log"),
        b"{\"broken\":\n{\"new\":1}\n"
    );
}

#[test]
fn rotation_without_backups_keeps_only_the_new_record() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("events.jsonl");
    fs::write(&path, b"{\"broken\":").expect("seed log");
    JsonlEventWriter::new(&path)
        .with_max_bytes(1)
        .with_backup_count(0)
        .write_or_raise(&json!({"new": 1}))
        .expect("append");
    assert_eq!(fs::read(&path).expect("read log"), b"{\"new\":1}\n");
    assert!(
        fs::read_dir(dir.path())
            .expect("read directory")
            .filter_map(Result::ok)
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with("events.jsonl.20"))
    );
}

#[test]
fn existing_symlink_log_retains_its_append_behavior() {
    let dir = TempDir::new().expect("temp dir");
    let target = dir.path().join("target.jsonl");
    let path = dir.path().join("events.jsonl");
    fs::write(&target, b"{\"broken\":").expect("seed target");
    std::os::unix::fs::symlink(&target, &path).expect("symlink log");
    JsonlEventWriter::new(&path)
        .write_or_raise(&json!({"new": 1}))
        .expect("append through same-inode symlink");
    assert_eq!(
        fs::read(target).expect("read target"),
        b"{\"broken\":\n{\"new\":1}\n"
    );
}
