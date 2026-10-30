//! Integration tests: in-place mount store-sync integrity.
//!
//! An in-place mount addresses the source through the pre-opened
//! `/proc/self/fd/{n}` dirfd (physical I/O must bypass the FUSE
//! over-mount), while the initial scan records the real source-root
//! spelling in every `source_path`. These tests pin the two invariants
//! that keep the live store equal to a fresh discovery scan of the same
//! tree:
//!
//! 1. a manifest written through the mount re-parses into the store
//!    under the *canonical* source-root spelling — the fd alias must
//!    never leak into store identity, or the duplicate-name rule would
//!    drop or steal the post-write refresh by alias ordering
//!    (`insert_discovered` compares spellings);
//! 2. removing or renaming away a *losing* same-leaf candidate (a flat
//!    slot shadowed by a categorized owner) must not drop the retained
//!    owner's entry.
//!
//! FUSE is required (`/dev/fuse` + `fusermount3`); tests skip gracefully
//! otherwise. Readiness is polled — the background mount runs the
//! environment-profile probe loop before the FUSE session comes up.

use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, SkillEntry, store::SkillStore};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

fn wait_for(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    predicate()
}

/// An in-place mount (`source == mountpoint`, the FUSE over-mount a
/// production in-place mount performs) with a handle on the shared
/// store, which is loaded from the real source path before the mount —
/// exactly what `skillfs mount --in-place` does.
struct InPlaceStoreMount {
    source: tempfile::TempDir,
    store: SharedSkillStore,
    handle: Option<MountHandle>,
}

impl InPlaceStoreMount {
    /// Seed `source`, load the store from the real path, over-mount, and
    /// poll until the over-mount is live. The probe is the mount table,
    /// not a file read: the seeded tree pre-exists on the real
    /// filesystem, so a readable probe would pass before the FUSE
    /// session ever comes up and the test's writes would bypass FUSE.
    fn new(seed: impl FnOnce(&std::path::Path)) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let mut initial = SkillStore::new();
        initial.load_from_directory(source.path(), &ParseConfig::default());
        let store: SharedSkillStore = Arc::new(RwLock::new(initial));

        let handle = mount_background_configured(
            source.path(),
            source.path(),
            store.clone(),
            MountOptions::default(),
            true,
            MountConfig::default(),
        )
        .expect("in-place mount");

        assert!(
            wait_for(Duration::from_secs(30), || {
                let mounted = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
                skillfs_fuse::proc_mounts::mounts_contain_target(
                    mounted.as_bytes(),
                    source.path().as_os_str().as_bytes(),
                )
            }),
            "in-place over-mount never went live for {}",
            source.path().display()
        );
        Self {
            source,
            store,
            handle: Some(handle),
        }
    }

    fn entry(&self, key: &str) -> Option<SkillEntry> {
        self.store.read().get(key).cloned()
    }
}

impl Drop for InPlaceStoreMount {
    fn drop(&mut self) {
        // Unmount before the source `TempDir` drops: the FUSE over-mount
        // sits on the tempdir itself, and an unmounted-then-removed
        // source is the only clean teardown.
        if let Some(handle) = self.handle.take() {
            let _ = handle.unmount();
        }
    }
}

/// kongche-jbw P1 on #7742: discovery records the real manifest path
/// while `write` enqueues the re-parse through the `/proc/self/fd/<n>`
/// alias — the same file, two spellings. The duplicate-name rule
/// compared the spellings, so a write through an in-place mount either
/// dropped the refresh (canonical spelling sorts first) or landed it
/// under the fd alias (alias sorts first); both diverge the live store
/// from a fresh scan. The refresh must land under the canonical
/// spelling.
#[test]
fn in_place_manifest_write_refreshes_the_store_under_the_canonical_spelling() {
    skip_if_no_fuse!();

    let m = InPlaceStoreMount::new(|src| {
        std::fs::create_dir_all(src.join("notes")).expect("skill dir");
        std::fs::write(
            src.join("notes/SKILL.md"),
            "---\nname: notes\ndescription: demo\n---\noriginal body\n",
        )
        .expect("SKILL.md");
    });
    assert_eq!(
        m.entry("notes").expect("discovered").source_path,
        m.source.path().join("notes/SKILL.md")
    );

    let mut md = std::fs::OpenOptions::new()
        .append(true)
        .open(m.source.path().join("notes/SKILL.md"))
        .expect("open manifest through the in-place mount");
    md.write_all(b"appended through the in-place mount\n")
        .expect("append through the mount");
    drop(md);

    assert!(
        wait_for(Duration::from_secs(5), || {
            m.entry("notes")
                .is_some_and(|e| e.body.contains("appended through the in-place mount"))
        }),
        "a manifest write through the in-place mount must refresh the store entry, body is {:?}",
        m.entry("notes").map(|e| e.body)
    );
    assert_eq!(
        m.entry("notes").expect("refreshed entry").source_path,
        m.source.path().join("notes/SKILL.md"),
        "the refresh must land under the canonical source-root spelling, not the fd alias"
    );
}

/// kongche-jbw P2 (unlink arm) on #7742: with a categorized `apple/notes`
/// owner retaining the `notes` key, unlinking the losing flat candidate's
/// manifest through the in-place `/<leaf>/SKILL.md` slot removed the key
/// by bare leaf name — the retained owner vanished although its manifest
/// was untouched. Removal must verify the candidate owns the stored
/// source before deleting the key.
#[test]
fn in_place_loser_manifest_unlink_keeps_the_categorized_owner() {
    skip_if_no_fuse!();

    let m = InPlaceStoreMount::new(|src| {
        for dir in ["apple/notes", "notes"] {
            std::fs::create_dir_all(src.join(dir)).expect("skill dir");
            std::fs::write(
                src.join(dir).join("SKILL.md"),
                format!("---\nname: notes\ndescription: {dir}\n---\nbody of {dir}\n"),
            )
            .expect("SKILL.md");
        }
    });

    let owner_md = m.source.path().join("apple/notes/SKILL.md");
    assert_eq!(
        m.entry("notes").expect("discovered owner").source_path,
        owner_md,
        "discovery keeps the lexicographically smaller categorized source"
    );

    std::fs::remove_file(m.source.path().join("notes/SKILL.md"))
        .expect("unlink the losing flat candidate's manifest");

    assert!(
        m.entry("notes").is_some(),
        "unlinking a losing same-leaf candidate must not drop the retained owner"
    );
    assert_eq!(
        m.entry("notes").expect("retained owner").source_path,
        owner_md
    );
}

/// kongche-jbw P2 (rename-away arm) on #7742: renaming the losing flat
/// candidate's manifest away hit the same unconditional leaf-key removal.
/// The retained categorized owner must survive it, exactly as a fresh
/// scan of the tree would re-discover it.
#[test]
fn in_place_loser_manifest_rename_away_keeps_the_categorized_owner() {
    skip_if_no_fuse!();

    let m = InPlaceStoreMount::new(|src| {
        for dir in ["apple/notes", "notes"] {
            std::fs::create_dir_all(src.join(dir)).expect("skill dir");
            std::fs::write(
                src.join(dir).join("SKILL.md"),
                format!("---\nname: notes\ndescription: {dir}\n---\nbody of {dir}\n"),
            )
            .expect("SKILL.md");
        }
    });

    let owner_md = m.source.path().join("apple/notes/SKILL.md");
    assert_eq!(
        m.entry("notes").expect("discovered owner").source_path,
        owner_md
    );

    let md = m.source.path().join("notes/SKILL.md");
    let gone = m.source.path().join("notes/GONE.md");
    std::fs::rename(&md, &gone).expect("rename the losing candidate's manifest away");

    assert!(
        m.entry("notes").is_some(),
        "renaming a losing same-leaf candidate away must not drop the retained owner"
    );
    assert_eq!(
        m.entry("notes").expect("retained owner").source_path,
        owner_md
    );
}
