//! Sandbox tests for the `MEMORY.md` index writer.
//!
//! `mem_index_refresh` regenerates `<mount>/MEMORY.md`, and the mount root is
//! writable by the agent, so the index path is attacker-influenced. These
//! tests pin the guarantee that every open of the index is anchored on the
//! mount's `root_fd` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`: a symlink
//! planted at `MEMORY.md` is refused instead of followed to a target outside
//! the mount — neither truncated by a write nor read back into the index.
//!
//! A refusal is only half the guarantee: like every other mount tool, the
//! index writer must also leave the refused attempt on the audit chain
//! (`<mount>/.anolisa/audit.log`, session log, journald fanout) with
//! `ok:false` and the reason, and must not report success to its caller.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::tempdir;

use agent_memory::config::{AppConfig, Profile};
use agent_memory::error::MemoryError;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_index::{self, IndexEntry};

/// Content of the file outside the mount that a planted link points at.
const VICTIM_BODY: &str = "PRIVATE KEY";

/// Build a service over a throwaway base dir plus a second tempdir that
/// stands in for "outside the mount".
fn setup() -> (tempfile::TempDir, tempfile::TempDir, MemoryService) {
    let base = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.profile = Profile::Advanced;
    cfg.memory.paths.base_dir = base.path().to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();
    (base, outside, svc)
}

/// Replace `<mount>/MEMORY.md` with a symlink to a victim file outside the
/// mount, and return the victim's path.
fn plant_symlinked_index(svc: &MemoryService, outside: &Path) -> PathBuf {
    let victim = outside.join("id_rsa.md");
    fs::write(&victim, VICTIM_BODY).unwrap();
    let link = svc.mount.root.join("MEMORY.md");
    let _ = fs::remove_file(&link);
    symlink(&victim, &link).unwrap();
    victim
}

fn entry(path: &str) -> IndexEntry {
    IndexEntry {
        title: "Title".into(),
        path: path.into(),
        description: "description".into(),
    }
}

/// The `mem_index_refresh` entries written to the mount's JSONL audit log.
fn index_audit(svc: &MemoryService) -> Vec<serde_json::Value> {
    let log = fs::read_to_string(svc.mount.audit_log_path()).unwrap_or_default();
    log.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("audit line is not JSON"))
        .filter(|v| v["tool"] == json!("mem_index_refresh"))
        .collect()
}

/// Assert the index audit log gained exactly `added` entries since `before`,
/// and that each of them is a refusal of `MEMORY.md` carrying a reason.
fn assert_refusals_audited(svc: &MemoryService, before: usize, added: usize) {
    let entries = index_audit(svc);
    assert_eq!(
        entries.len(),
        before + added,
        "expected {added} audited refusals, got {entries:?}"
    );
    for entry in &entries[before..] {
        assert_eq!(
            entry["ok"],
            json!(false),
            "a refusal must not audit ok:true"
        );
        assert_eq!(entry["path"], json!("MEMORY.md"));
        let reason = entry["error"].as_str().unwrap_or("");
        assert!(
            reason.contains("outside the namespace mount point"),
            "the audit entry must carry the refusal reason, got {reason:?}"
        );
    }
}

#[test]
fn refresh_refuses_a_symlinked_memory_md() {
    let (_base, outside, svc) = setup();
    svc.write("notes/foo.md", "foo body", false).unwrap();
    svc.write("notes/bar.md", "bar body", false).unwrap();
    let victim = plant_symlinked_index(&svc, outside.path());

    let err = memory_index::refresh_index(&svc).expect_err("a symlinked MEMORY.md must be refused");
    assert!(
        matches!(err, MemoryError::PathOutsideMount(_)),
        "refresh should reject the planted link, got {err:?}"
    );
    assert_eq!(
        fs::read_to_string(&victim).unwrap(),
        VICTIM_BODY,
        "the victim outside the mount must not be truncated"
    );
}

#[test]
fn refused_refresh_reaches_the_audit_log() {
    let (_base, outside, svc) = setup();
    svc.write("notes/foo.md", "foo body", false).unwrap();
    let victim = plant_symlinked_index(&svc, outside.path());
    let before = index_audit(&svc).len();

    memory_index::refresh_index(&svc).expect_err("a symlinked MEMORY.md must be refused");

    assert_refusals_audited(&svc, before, 1);
    assert_eq!(fs::read_to_string(&victim).unwrap(), VICTIM_BODY);
}

#[test]
fn update_entry_refuses_a_symlinked_memory_md() {
    let (_base, outside, svc) = setup();
    let victim = plant_symlinked_index(&svc, outside.path());

    let err = memory_index::update_index_entry(&svc, &entry("notes/foo.md"))
        .expect_err("a symlinked MEMORY.md must be refused");
    assert!(
        matches!(err, MemoryError::PathOutsideMount(_)),
        "the upsert should reject the planted link, got {err:?}"
    );
    assert_eq!(fs::read_to_string(&victim).unwrap(), VICTIM_BODY);
}

#[test]
fn refused_read_back_reaches_the_audit_log() {
    let (_base, outside, svc) = setup();
    let victim = plant_symlinked_index(&svc, outside.path());
    let before = index_audit(&svc).len();

    // The upsert reads the existing index back first, so this covers the read
    // half of the guard: the planted link must not leak into the index and
    // the refusal must be audible.
    memory_index::update_index_entry(&svc, &entry("notes/foo.md"))
        .expect_err("a symlinked MEMORY.md must be refused");

    assert_refusals_audited(&svc, before, 1);
    assert_eq!(fs::read_to_string(&victim).unwrap(), VICTIM_BODY);
}

#[test]
fn refused_remove_is_not_a_silent_success() {
    let (_base, outside, svc) = setup();
    let victim = plant_symlinked_index(&svc, outside.path());
    let before = index_audit(&svc).len();

    // Nothing was removed — the guard refused to open the index — so the
    // caller must see an error rather than an unconditional `Ok(())`.
    let err = memory_index::remove_index_entry(&svc, "notes/foo.md")
        .expect_err("a refused removal must not report success");
    assert!(
        matches!(err, MemoryError::PathOutsideMount(_)),
        "the removal should reject the planted link, got {err:?}"
    );

    assert_refusals_audited(&svc, before, 1);
    assert_eq!(fs::read_to_string(&victim).unwrap(), VICTIM_BODY);
}

#[test]
fn remove_entry_without_an_index_is_a_silent_no_op() {
    let (_base, _outside, svc) = setup();

    // No `MEMORY.md` at all is a legitimate no-op, not a refusal: it stays
    // `Ok(())`, creates no index, and adds no audit noise.
    memory_index::remove_index_entry(&svc, "notes/foo.md").unwrap();

    assert!(!svc.mount.root.join("MEMORY.md").exists());
    assert!(
        index_audit(&svc).is_empty(),
        "a missing index must not be audited as a refusal"
    );
}

#[test]
fn refresh_writes_a_regular_memory_md() {
    let (_base, _outside, svc) = setup();
    svc.write("notes/foo.md", "---\ntitle: Foo\n---\nfoo body", false)
        .unwrap();

    let count = memory_index::refresh_index(&svc).unwrap();
    assert!(count >= 1, "the note should be indexed, got {count}");

    let index = fs::read_to_string(svc.mount.root.join("MEMORY.md")).unwrap();
    assert!(index.starts_with("# Memory Index"), "got: {index}");
    assert!(index.contains("notes/foo.md"), "got: {index}");
    assert!(
        fs::symlink_metadata(svc.mount.root.join("MEMORY.md"))
            .unwrap()
            .file_type()
            .is_file(),
        "MEMORY.md must be a regular file"
    );

    // The success path still audits, and still reports success.
    let entries = index_audit(&svc);
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0]["ok"], json!(true));
    assert_eq!(entries[0]["path"], json!("MEMORY.md"));
    assert!(entries[0]["error"].is_null(), "got {entries:?}");
    assert!(
        entries[0]["bytes"].as_u64().unwrap_or(0) > 0,
        "got {entries:?}"
    );
}

#[test]
fn update_and_remove_entry_round_trip_a_regular_memory_md() {
    let (_base, _outside, svc) = setup();

    memory_index::update_index_entry(&svc, &entry("notes/foo.md")).unwrap();
    let index = fs::read_to_string(svc.mount.root.join("MEMORY.md")).unwrap();
    assert!(index.contains("(notes/foo.md)"), "got: {index}");

    memory_index::remove_index_entry(&svc, "notes/foo.md").unwrap();
    let index = fs::read_to_string(svc.mount.root.join("MEMORY.md")).unwrap();
    assert!(!index.contains("(notes/foo.md)"), "got: {index}");
}
