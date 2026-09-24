#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use aw_contracts::canonical;
use aw_core::journal::FileJournal;
use aw_core::ports::{Journal, JournalError};
use serde_json::{json, Value};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/journal-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn file(&self) -> PathBuf {
        self.0.join(format!("{KEY}.jsonl"))
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn acknowledged_evidence_binds_canonical_records_across_restart() {
    let directory = Directory::new();
    let mut journal = FileJournal::new(&directory.0).unwrap();
    let first = journal.claim(KEY, &json!({"plan_id": "example"})).unwrap();
    let second = journal
        .append(KEY, &json!({"status": "completed"}))
        .unwrap();
    drop(journal);
    let journal = FileJournal::new(&directory.0).unwrap();
    let records = journal.read_verified(KEY, &second).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["digest"], first["digest"]);
    assert_eq!(records[1]["previous_digest"], first["digest"]);
    for record in &records {
        let mut unsigned = record.clone();
        let digest = unsigned.as_object_mut().unwrap().remove("digest").unwrap();
        assert_eq!(digest, canonical::document_digest(&unsigned).unwrap());
    }
    assert_eq!(second["record_id"], format!("{KEY}:1"));
    assert_eq!(second["source_id"], "aw-core-file-journal-v1");
    assert!(journal.read_verified(KEY, &first).is_err());
}

#[test]
fn restart_preserves_reservation_without_transferring_write_ownership() {
    let directory = Directory::new();
    let mut journal = FileJournal::new(&directory.0).unwrap();
    journal.claim(KEY, &json!({})).unwrap();
    drop(journal);
    let mut restarted = FileJournal::new(&directory.0).unwrap();
    assert!(matches!(
        restarted.claim(KEY, &json!({"different_plan": true})),
        Err(JournalError::AlreadyClaimed)
    ));
    assert!(matches!(
        restarted.append(KEY, &json!({"status": "completed"})),
        Err(JournalError::InvalidRecord)
    ));
}

#[test]
fn concurrent_objects_have_exactly_one_claim_winner() {
    let directory = Directory::new();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = directory.0.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut journal = FileJournal::new(path).unwrap();
                barrier.wait();
                match journal.claim(KEY, &json!({})) {
                    Ok(_) => {
                        journal
                            .append(KEY, &json!({"status": "completed"}))
                            .unwrap();
                        true
                    }
                    Err(JournalError::AlreadyClaimed) => false,
                    Err(error) => panic!("unexpected claim failure: {error}"),
                }
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap()))
            .sum::<usize>(),
        1
    );
    assert_eq!(
        FileJournal::new(&directory.0)
            .unwrap()
            .read(KEY)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn malformed_keys_cannot_escape_the_journal_directory() {
    let directory = Directory::new();
    let mut journal = FileJournal::new(&directory.0).unwrap();
    for key in [
        "",
        "../outside",
        &"A".repeat(64),
        &"g".repeat(64),
        &"a".repeat(63),
    ] {
        assert!(matches!(
            journal.claim(key, &json!({})),
            Err(JournalError::InvalidRecord)
        ));
        assert!(matches!(
            journal.append(key, &json!({})),
            Err(JournalError::InvalidRecord)
        ));
        assert!(matches!(
            journal.read(key),
            Err(JournalError::InvalidRecord)
        ));
    }
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
}

#[test]
fn append_requires_ownership_and_failed_append_poisoning_is_sticky() {
    let directory = Directory::new();
    let mut journal = FileJournal::new(&directory.0).unwrap();
    assert!(journal.append(KEY, &json!({})).is_err());
    let first = journal.claim(KEY, &json!({})).unwrap();
    assert!(journal
        .append(KEY, &json!({"invalid_number": 1.5}))
        .is_err());
    assert!(journal
        .append(KEY, &json!({"status": "completed"}))
        .is_err());
    assert_eq!(journal.read_verified(KEY, &first).unwrap().len(), 1);
    assert!(matches!(
        journal.claim(KEY, &json!({})),
        Err(JournalError::AlreadyClaimed)
    ));
}

#[test]
fn interrupted_empty_reservation_is_not_repaired_or_reclaimed() {
    let directory = Directory::new();
    fs::write(directory.file(), []).unwrap();
    let mut journal = FileJournal::new(&directory.0).unwrap();
    assert!(matches!(
        journal.claim(KEY, &json!({})),
        Err(JournalError::AlreadyClaimed)
    ));
    assert!(matches!(
        journal.read(KEY),
        Err(JournalError::InvalidRecord)
    ));
    assert_eq!(fs::metadata(directory.file()).unwrap().len(), 0);
}

#[test]
fn altered_record_sequence_event_key_and_digest_are_rejected() {
    for field in [
        "record",
        "sequence",
        "event_key",
        "previous_digest",
        "digest",
        "format",
    ] {
        let directory = Directory::new();
        let mut journal = FileJournal::new(&directory.0).unwrap();
        journal.claim(KEY, &json!({})).unwrap();
        journal
            .append(KEY, &json!({"status": "completed"}))
            .unwrap();
        let mut records = journal.read(KEY).unwrap();
        records[1][field] = json!("tampered");
        let bytes = records
            .iter()
            .map(|value| format!("{value}\n"))
            .collect::<String>();
        fs::write(directory.file(), bytes).unwrap();
        assert!(
            matches!(journal.read(KEY), Err(JournalError::InvalidRecord)),
            "{field}"
        );
    }
}

#[test]
fn partial_line_is_rejected_and_complete_tail_loss_requires_external_tip() {
    let directory = Directory::new();
    let mut journal = FileJournal::new(&directory.0).unwrap();
    journal.claim(KEY, &json!({})).unwrap();
    let tip = journal
        .append(KEY, &json!({"status": "completed"}))
        .unwrap();
    let bytes = fs::read(directory.file()).unwrap();
    fs::write(directory.file(), &bytes[..bytes.len() - 1]).unwrap();
    assert!(matches!(
        journal.read(KEY),
        Err(JournalError::InvalidRecord)
    ));
    let first_line_end = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    fs::write(directory.file(), &bytes[..first_line_end]).unwrap();
    assert_eq!(journal.read(KEY).unwrap().len(), 1);
    assert!(matches!(
        journal.read_verified(KEY, &tip),
        Err(JournalError::InvalidRecord)
    ));
}

#[test]
fn deterministic_evidence_does_not_depend_on_directory_or_key_order() {
    let left = Directory::new();
    let right = Directory::new();
    let mut a = FileJournal::new(&left.0).unwrap();
    let mut b = FileJournal::new(&right.0).unwrap();
    let left_plan: Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
    let right_plan: Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
    assert_eq!(
        a.claim(KEY, &left_plan).unwrap(),
        b.claim(KEY, &right_plan).unwrap()
    );
    assert_eq!(
        a.append(KEY, &json!({"status": "completed"})).unwrap(),
        b.append(KEY, &json!({"status": "completed"})).unwrap()
    );
}

#[test]
fn duplicate_metadata_keys_and_extra_envelope_fields_are_rejected() {
    let directory = Directory::new();
    let mut journal = FileJournal::new(&directory.0).unwrap();
    journal.claim(KEY, &json!({})).unwrap();
    let original = fs::read_to_string(directory.file()).unwrap();
    let duplicate = original.replacen('{', "{\"format\":1,", 1);
    fs::write(directory.file(), duplicate).unwrap();
    assert!(journal.read(KEY).is_err());
    let mut record: Value = serde_json::from_str(&original).unwrap();
    record["extra"] = json!(true);
    fs::write(directory.file(), format!("{record}\n")).unwrap();
    assert!(journal.read(KEY).is_err());
}

#[test]
fn separate_processes_share_the_atomic_reservation() {
    use std::process::{Child, Command, Stdio};

    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            // Only terminate this test's child if an assertion aborts its wait.
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    let directory = Directory::new();
    let journal_path = directory.0.join("journal");
    let executable = std::env::current_exe().unwrap();
    let mut children: Vec<_> = (0..2)
        .map(|index| {
            OwnedChild(
                Command::new(&executable)
                    .args(["--exact", "journal_child_claim_entrypoint", "--ignored"])
                    .env("AW_JOURNAL_TEST_CHILD_DIRECTORY", &journal_path)
                    .env(
                        "AW_JOURNAL_TEST_CHILD_RESULT",
                        directory.0.join(format!("{index}")),
                    )
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            )
        })
        .collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    for child in &mut children {
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "journal child timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let winners = (0..2)
        .filter(|index| {
            fs::read_to_string(directory.0.join(format!("{index}"))).unwrap() == "claimed"
        })
        .count();
    assert_eq!(winners, 1);
    assert_eq!(
        FileJournal::new(journal_path)
            .unwrap()
            .read(KEY)
            .unwrap()
            .len(),
        2
    );
}

#[test]
#[ignore = "invoked by the process reservation test"]
fn journal_child_claim_entrypoint() {
    let Some(path) = std::env::var_os("AW_JOURNAL_TEST_CHILD_DIRECTORY") else {
        return;
    };
    let result_path = std::env::var_os("AW_JOURNAL_TEST_CHILD_RESULT").unwrap();
    let mut journal = FileJournal::new(path).unwrap();
    let outcome = match journal.claim(KEY, &json!({})) {
        Ok(_) => {
            journal
                .append(KEY, &json!({"status": "completed"}))
                .unwrap();
            "claimed"
        }
        Err(JournalError::AlreadyClaimed) => "reserved",
        Err(error) => panic!("unexpected child claim failure: {error}"),
    };
    fs::write(result_path, outcome).unwrap();
}

#[test]
fn directory_creation_rejects_a_regular_file() {
    let directory = Directory::new();
    let path = directory.0.join("regular-file");
    fs::write(&path, b"unchanged").unwrap();
    assert!(FileJournal::new(&path).is_err());
    assert_eq!(fs::read(path).unwrap(), b"unchanged");
}

#[test]
fn release_closes_successful_or_poisoned_writers_and_preserves_evidence() {
    for poisoned in [false, true] {
        let directory = Directory::new();
        let mut journal = FileJournal::new(&directory.0).unwrap();
        let mut tip = journal.claim(KEY, &json!({})).unwrap();
        if poisoned {
            assert!(journal
                .append(KEY, &json!({"invalid_number": 1.5}))
                .is_err());
        } else {
            tip = journal
                .append(KEY, &json!({"status": "completed"}))
                .unwrap();
        }
        journal.release(KEY);
        journal.release(KEY);
        journal.release("unknown");
        let path = fs::canonicalize(&directory.0).unwrap();
        assert_eq!(
            fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
                .filter(|target| target.starts_with(&path))
                .count(),
            0
        );
        assert!(matches!(
            journal.append(KEY, &json!({})),
            Err(JournalError::InvalidRecord)
        ));
        assert!(matches!(
            journal.claim(KEY, &json!({})),
            Err(JournalError::AlreadyClaimed)
        ));
        assert_eq!(
            journal.read_verified(KEY, &tip).unwrap().len(),
            if poisoned { 1 } else { 2 }
        );
    }
}
