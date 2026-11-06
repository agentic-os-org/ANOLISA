#![cfg(target_os = "linux")]

use aw_core::{
    journal::{
        reader::{Error, JournalReader, MAX_CHAIN_BYTES, MAX_ENTRIES},
        FileJournal,
    },
    ports::Journal,
};
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    path: PathBuf,
    journal: FileJournal,
}
impl Fixture {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/journal-reader")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let journal = FileJournal::new(&path).unwrap();
        Self { path, journal }
    }
    fn key(n: u8) -> String {
        format!("{n:064x}")
    }
    fn reader(&self) -> JournalReader {
        JournalReader::open(&self.path, fs::metadata(&self.path).unwrap().uid()).unwrap()
    }
    fn claim(&mut self, n: u8) -> String {
        let key = Self::key(n);
        self.journal.claim(&key, &json!({"kind":"test"})).unwrap();
        key
    }
    fn file(&self, key: &str) -> PathBuf {
        self.path.join(format!("{key}.jsonl"))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}

#[test]
fn ordered_inventory_progresses_under_additions_and_deleted_continuations() {
    let mut f = Fixture::new();
    let third = f.claim(30);
    let first = f.claim(10);
    let second = f.claim(20);
    let reader = f.reader();
    let page = reader.enumerate(None, None, None, 1, &|| false).unwrap();
    assert_eq!(page.keys, [first]);
    f.claim(1);
    let next = reader
        .enumerate(
            page.next.as_deref(),
            Some(&page.digest),
            page.through.as_deref(),
            1,
            &|| false,
        )
        .unwrap();
    assert_eq!(next.keys.as_slice(), std::slice::from_ref(&second));
    assert!(next.changed);
    f.claim(2);
    f.claim(40);
    fs::remove_file(f.file(&second)).unwrap();
    let last = reader
        .enumerate(
            next.next.as_deref(),
            Some(&next.digest),
            next.through.as_deref(),
            1,
            &|| false,
        )
        .unwrap();
    assert_eq!(last.keys, [third]);
    assert!(last.changed);
    assert!(last.next.is_none());
    let again = reader.enumerate(None, None, None, 64, &|| false).unwrap();
    assert!(again.keys.contains(&Fixture::key(1)) && again.keys.contains(&Fixture::key(40)));
}

#[test]
fn open_chain_revisit_reads_late_append_and_restart_keeps_positions() {
    let mut f = Fixture::new();
    let key = f.claim(1);
    let reader = f.reader();
    let first = reader.read(&key, None, None, 1, &|| false).unwrap();
    assert!(first.caught_up);
    assert!(!first.terminal);
    f.journal
        .append(&key, &json!({"phase":"completed"}))
        .unwrap();
    f.journal.append(&key, &json!({"phase":"closed"})).unwrap();
    let next = f
        .reader()
        .read(&key, first.next.as_ref(), Some(&first.tip), 1, &|| false)
        .unwrap();
    assert_eq!(next.records.len(), 1);
    assert!(!next.caught_up);
    assert!(next.terminal);
    let last = reader
        .read(&key, next.next.as_ref(), Some(&next.tip), 1, &|| false)
        .unwrap();
    assert!(last.caught_up);
    assert_eq!(last.tip, next.tip);
    let empty = reader
        .read(&key, last.next.as_ref(), Some(&last.tip), 1, &|| false)
        .unwrap();
    assert!(empty.records.is_empty());
    assert!(empty.caught_up);
}

#[test]
fn retained_tip_detects_removed_suffix_even_before_it_was_consumed() {
    let mut f = Fixture::new();
    let key = f.claim(1);
    let prefix = fs::read(f.file(&key)).unwrap();
    f.journal
        .append(&key, &json!({"phase":"completed"}))
        .unwrap();
    let first = f.reader().read(&key, None, None, 1, &|| false).unwrap();
    fs::write(f.file(&key), prefix).unwrap();
    assert!(matches!(
        f.reader()
            .read(&key, first.next.as_ref(), Some(&first.tip), 1, &|| false),
        Err(Error::Changed)
    ));
}

#[test]
fn missing_corrupt_and_incomplete_history_are_distinct() {
    let mut f = Fixture::new();
    let key = f.claim(1);
    let reader = f.reader();
    assert!(matches!(
        reader.read(&Fixture::key(2), None, None, 1, &|| false),
        Err(Error::Missing)
    ));
    let bytes = fs::read(f.file(&key)).unwrap();
    fs::write(f.file(&key), &bytes[..bytes.len() - 1]).unwrap();
    assert!(matches!(
        reader.read(&key, None, None, 1, &|| false),
        Err(Error::Incomplete)
    ));
    fs::write(f.file(&key), b"{}\n").unwrap();
    assert!(matches!(
        reader.read(&key, None, None, 1, &|| false),
        Err(Error::Corrupt)
    ));
    fs::write(f.file(&key), b"").unwrap();
    assert!(matches!(
        reader.read(&key, None, None, 1, &|| false),
        Err(Error::Incomplete)
    ));
}

#[test]
fn cancellation_and_size_limits_stop_reads() {
    let mut f = Fixture::new();
    let key = f.claim(1);
    let reader = f.reader();
    assert!(matches!(
        reader.read(&key, None, None, 1, &|| true),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        reader.enumerate(None, None, None, 1, &|| true),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        reader.read(&key, None, None, 65, &|| false),
        Err(Error::Request)
    ));
    OpenOptions::new()
        .write(true)
        .open(f.file(&key))
        .unwrap()
        .set_len(MAX_CHAIN_BYTES + 1)
        .unwrap();
    assert!(matches!(
        reader.read(&key, None, None, 1, &|| false),
        Err(Error::Limit)
    ));
}

#[test]
fn no_follow_private_modes_and_cross_uid_checks() {
    let mut f = Fixture::new();
    let key = f.claim(1);
    let reader = f.reader();
    let owner = fs::metadata(&f.path).unwrap().uid();
    assert!(matches!(
        JournalReader::open(&f.path, owner.wrapping_add(1)),
        Err(Error::Access)
    ));
    fs::set_permissions(f.file(&key), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        reader.read(&key, None, None, 1, &|| false),
        Err(Error::Access)
    ));
    fs::set_permissions(f.file(&key), fs::Permissions::from_mode(0o600)).unwrap();
    let alias = f.file(&Fixture::key(2));
    symlink(f.file(&key), &alias).unwrap();
    assert!(reader
        .read(&Fixture::key(2), None, None, 1, &|| false)
        .is_err());
    fs::remove_file(&alias).unwrap();
    fs::hard_link(f.file(&key), &alias).unwrap();
    assert!(matches!(
        reader.read(&Fixture::key(2), None, None, 1, &|| false),
        Err(Error::Access)
    ));
}

#[test]
fn directory_replacement_does_not_redirect_open_reader() {
    let mut f = Fixture::new();
    let key = f.claim(1);
    let reader = f.reader();
    let original = f.path.with_extension("retained");
    fs::rename(&f.path, &original).unwrap();
    let replacement = FileJournal::new(&f.path).unwrap();
    drop(replacement);
    assert_eq!(
        reader
            .read(&key, None, None, 1, &|| false)
            .unwrap()
            .tip
            .sequence,
        0
    );
    fs::remove_dir_all(original).unwrap();
}

#[test]
fn inventory_is_bounded_even_for_non_journal_entries() {
    let f = Fixture::new();
    for n in 0..=MAX_ENTRIES {
        fs::write(f.path.join(format!("unrelated-{n}")), b"").unwrap();
    }
    assert!(matches!(
        f.reader().enumerate(None, None, None, 1, &|| false),
        Err(Error::Limit)
    ));
}

#[test]
fn failed_tail_and_post_terminal_append_never_report_success() {
    let mut f = Fixture::new();
    let key = f.claim(1);
    f.journal.append(&key, &json!({"phase":"closed"})).unwrap();
    f.journal
        .append(&key, &json!({"phase":"completed"}))
        .unwrap();
    assert!(matches!(
        f.reader().read(&key, None, None, 1, &|| false),
        Err(Error::Corrupt)
    ));
    let other = f.claim(2);
    OpenOptions::new()
        .append(true)
        .open(f.file(&other))
        .unwrap()
        .write_all(b"{\"broken\"")
        .unwrap();
    assert!(matches!(
        f.reader().read(&other, None, None, 1, &|| false),
        Err(Error::Incomplete)
    ));
}

#[test]
fn appends_during_read_wait_for_next_visit_and_truncation_is_visible() {
    let mut f = Fixture::new();
    let key = f.claim(1);
    let original = fs::read(f.file(&key)).unwrap();
    f.journal
        .append(&key, &json!({"phase":"completed"}))
        .unwrap();
    let complete = fs::read(f.file(&key)).unwrap();
    fs::write(f.file(&key), &original).unwrap();
    let calls = AtomicUsize::new(0);
    let page = f
        .reader()
        .read(&key, None, None, 64, &|| {
            if calls.fetch_add(1, Ordering::Relaxed) == 1 {
                fs::write(f.file(&key), &complete).unwrap();
            }
            false
        })
        .unwrap();
    assert_eq!(page.tip.sequence, 0);
    assert_eq!(
        f.reader()
            .read(&key, page.next.as_ref(), Some(&page.tip), 64, &|| false)
            .unwrap()
            .tip
            .sequence,
        1
    );
    let calls = AtomicUsize::new(0);
    assert!(matches!(
        f.reader().read(&key, None, None, 64, &|| {
            if calls.fetch_add(1, Ordering::Relaxed) == 1 {
                fs::write(f.file(&key), &original).unwrap();
            }
            false
        }),
        Err(Error::Changed)
    ));
}

#[test]
fn record_count_and_mid_read_cancellation_are_bounded() {
    let mut f = Fixture::new();
    let key = f.claim(1);
    let mut previous = f.journal.read(&key).unwrap()[0]["digest"].clone();
    let mut file = OpenOptions::new().append(true).open(f.file(&key)).unwrap();
    for sequence in 1..=aw_core::journal::reader::MAX_CHAIN_RECORDS {
        let mut envelope = json!({"format":1,"event_key":key,"sequence":sequence,"previous_digest":previous,"record":{"phase":"started"}});
        previous = json!(aw_contracts::canonical::document_digest(&envelope).unwrap());
        envelope["digest"] = previous.clone();
        file.write_all(&aw_contracts::canonical::bytes(&envelope).unwrap())
            .unwrap();
        file.write_all(b"\n").unwrap();
    }
    drop(file);
    assert!(matches!(
        f.reader().read(&key, None, None, 64, &|| false),
        Err(Error::Limit)
    ));
    let calls = AtomicUsize::new(0);
    assert!(matches!(
        f.reader().read(&key, None, None, 64, &|| calls
            .fetch_add(1, Ordering::Relaxed)
            > 3),
        Err(Error::Cancelled)
    ));
}
