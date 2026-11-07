//! Sandbox tests for the `MEMORY.md` index writer.
//!
//! `mem_index_refresh` regenerates `<mount>/MEMORY.md`, and the mount root is
//! writable by the agent, so the index path is attacker-influenced. These
//! tests pin the guarantee that every open of the index is anchored on the
//! mount's `root_fd` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`: a symlink
//! planted at `MEMORY.md` is refused instead of followed to a target outside
//! the mount — neither truncated by a write nor read back into the index.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

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
fn remove_entry_leaves_a_symlinked_memory_md_alone() {
    let (_base, outside, svc) = setup();
    let victim = plant_symlinked_index(&svc, outside.path());

    // No index to update: the removal no-ops instead of rewriting the link's
    // target with a freshly generated index.
    memory_index::remove_index_entry(&svc, "notes/foo.md").unwrap();
    assert_eq!(fs::read_to_string(&victim).unwrap(), VICTIM_BODY);
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
