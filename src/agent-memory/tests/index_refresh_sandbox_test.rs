//! Sandbox tests for the `mem_index_refresh` write path.
//!
//! The memory mount root is writable by the agent, so `MEMORY.md` itself
//! can be replaced with a symlink that points outside the mount. Regenerating
//! the index must refuse such a destination instead of following the link:
//! `std::fs::write` would truncate and overwrite the link target, handing a
//! plain tool call an arbitrary-file-write primitive.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use agent_memory::config::{AppConfig, Profile};
use agent_memory::error::MemoryError;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_index::{
    IndexEntry, refresh_index, remove_index_entry, update_index_entry,
};

const INDEX_FILE: &str = "MEMORY.md";
const VICTIM_BODY: &str = "victim-bytes-that-must-survive";

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.profile = Profile::Advanced;
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

/// Write a victim file outside the memory mount and plant a symlink at
/// `<mount>/MEMORY.md` pointing at it. Returns the temp dir that owns the
/// victim (keep it alive) and the victim path.
fn plant_symlinked_index(svc: &MemoryService) -> (tempfile::TempDir, PathBuf) {
    let outside = tempdir().unwrap();
    let victim = outside.path().join("victim.txt");
    std::fs::write(&victim, VICTIM_BODY).unwrap();

    let index_path = svc.mount.root.join(INDEX_FILE);
    let _ = std::fs::remove_file(&index_path);
    symlink(&victim, &index_path).unwrap();

    (outside, victim)
}

fn assert_victim_intact(victim: &Path) {
    assert_eq!(
        std::fs::read_to_string(victim).unwrap(),
        VICTIM_BODY,
        "the file outside the mount must keep its bytes"
    );
}

#[test]
fn refresh_refuses_a_symlinked_memory_md() {
    let (_t, svc) = setup();
    svc.write("notes/one.md", "first memory", false).unwrap();
    svc.write("notes/two.md", "second memory", false).unwrap();

    let (_outside, victim) = plant_symlinked_index(&svc);

    let err = refresh_index(&svc).unwrap_err();
    assert!(
        matches!(err, MemoryError::PathOutsideMount(_)),
        "a symlinked MEMORY.md must be refused, got: {err:?}"
    );
    assert_victim_intact(&victim);

    // Refusing is all the refresh does — the planted link is left for the
    // operator to inspect rather than silently unlinked.
    let md = std::fs::symlink_metadata(svc.mount.root.join(INDEX_FILE)).unwrap();
    assert!(
        md.file_type().is_symlink(),
        "the symlink itself is untouched"
    );
}

#[test]
fn entry_updates_refuse_a_symlinked_memory_md() {
    let (_t, svc) = setup();
    let (_outside, victim) = plant_symlinked_index(&svc);

    // The upsert path reads the current index first; that read must not
    // follow the link either, or outside content leaks into the index.
    let entry = IndexEntry {
        title: "Planted".into(),
        path: "notes/x.md".into(),
        description: "should never be written".into(),
    };
    let err = update_index_entry(&svc, &entry).unwrap_err();
    assert!(
        matches!(err, MemoryError::PathOutsideMount(_)),
        "update_index_entry must refuse a symlinked MEMORY.md, got: {err:?}"
    );
    assert_victim_intact(&victim);

    let err = remove_index_entry(&svc, "notes/x.md").unwrap_err();
    assert!(
        matches!(err, MemoryError::PathOutsideMount(_)),
        "remove_index_entry must refuse a symlinked MEMORY.md, got: {err:?}"
    );
    assert_victim_intact(&victim);
}

#[test]
fn refresh_still_writes_a_regular_memory_md() {
    let (_t, svc) = setup();
    svc.write("notes/one.md", "first memory", false).unwrap();
    svc.write("notes/two.md", "second memory", false).unwrap();

    let count = refresh_index(&svc).unwrap();
    assert!(
        count >= 2,
        "both notes must be indexed, got {count} entries"
    );

    let index_path = svc.mount.root.join(INDEX_FILE);
    let md = std::fs::symlink_metadata(&index_path).unwrap();
    assert!(md.file_type().is_file(), "MEMORY.md must be a regular file");

    let body = std::fs::read_to_string(&index_path).unwrap();
    assert!(
        body.starts_with("# Memory Index\n"),
        "unexpected body: {body}"
    );
    assert!(body.contains("notes/one.md"), "unexpected body: {body}");
    assert!(body.contains("notes/two.md"), "unexpected body: {body}");

    // Overwriting the regular file it just wrote keeps working.
    refresh_index(&svc).unwrap();
    assert_eq!(std::fs::read_to_string(&index_path).unwrap(), body);
}
