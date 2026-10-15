//! Real-callback regression coverage for the configured `SKILL.md` ceiling
//! on the rename store refresh (`update_store_after_skill_rename`).
//!
//! The unit tests next to `fs/callbacks/mutate.rs` pin the refresh helper's
//! parse ceiling in isolation. These tests drive the whole chain through a
//! real FUSE mount — `rename_impl`'s `rename_source_backs_store_entry` guard
//! included — because that guard is what makes the reachability precise:
//! the refresh only runs for a directory that already backs a store entry.
//!
//!   * a manifest that was loaded legally under a lowered ceiling and then
//!     grew past it (store entry kept) degrades across the rename instead
//!     of re-entering the store fully parsed under the default ceiling;
//!   * a manifest that is legal under a raised ceiling stays fully parsed
//!     across the rename instead of degrading to the placeholder forever;
//!   * a manifest the initial load refused (no store entry) stays out of
//!     the store across the rename: `rename_impl` never calls the refresh
//!     for a directory that backs no entry.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, ParseStatus, SharedSkillStore, store::SkillStore};
use skillfs_fuse::{
    MountConfig, MountHandle, MountOptions, SkillLayout, mount_background_configured,
};

mod common;

fn write_sized_skill(skill_dir: &Path, name: &str, total_len: usize) {
    std::fs::create_dir_all(skill_dir).expect("skill dir");
    let mut content = format!("---\nname: {name}\ndescription: sized\n---\n");
    content.push_str(&"a".repeat(total_len.saturating_sub(content.len())));
    std::fs::write(skill_dir.join("SKILL.md"), content).expect("SKILL.md");
}

fn mount_with_ceiling(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    ceiling: usize,
) -> MountHandle {
    let handle = mount_background_configured(
        mountpoint,
        source,
        store,
        MountOptions::default(),
        false,
        MountConfig {
            max_skill_size: Some(ceiling),
            ..MountConfig::default()
        },
    )
    .expect("mount with the configured ceiling");
    // Give the FUSE daemon time to start serving requests.
    std::thread::sleep(Duration::from_millis(300));
    handle
}

#[test]
fn manifest_grown_past_a_lowered_ceiling_degrades_across_a_real_rename() {
    skip_if_no_fuse!();

    // The reachable lowered-ceiling scenario: the skill loads legally under
    // the 64 KiB ceiling (the store keeps its entry), the underlying
    // manifest then grows past the ceiling, and the directory rename runs
    // the full rename_impl chain — the rename_source_backs_store_entry
    // guard finds the loaded entry, so the refresh re-parses the
    // now-over-ceiling manifest under the configured ceiling.
    let source = tempfile::tempdir().expect("source tempdir");
    write_sized_skill(&source.path().join("sized-skill"), "sized-skill", 32 * 1024);

    let config = ParseConfig {
        max_skill_size: 64 * 1024,
        ..ParseConfig::default()
    };
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &config);
    assert!(
        store
            .get("sized-skill")
            .expect("the 32 KiB skill loads under the 64 KiB ceiling")
            .parse_status
            .is_ok()
    );
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().expect("mount tempdir");
    let handle = mount_with_ceiling(mountpoint.path(), source.path(), shared.clone(), 64 * 1024);

    // Grow the underlying manifest past the ceiling after the load: the
    // store still holds the entry from the legal 32 KiB parse, so the
    // rename callback still finds a directory that backs a store entry.
    write_sized_skill(&source.path().join("sized-skill"), "sized-skill", 96 * 1024);

    std::fs::rename(
        mountpoint.path().join("skills/sized-skill"),
        mountpoint.path().join("skills/renamed-skill"),
    )
    .expect("rename the skill directory through the mount");

    let guard = shared.read();
    let entry = guard.get("renamed-skill").expect("refreshed store entry");
    assert!(
        matches!(entry.parse_status, ParseStatus::Degraded(_)),
        "the over-ceiling manifest must degrade across the rename, got {:?}",
        entry.parse_status
    );
    assert!(
        entry.body.is_empty(),
        "the over-ceiling body must not be retained"
    );
    assert!(
        guard.get("sized-skill").is_none(),
        "the old key must be gone after the rename"
    );
    drop(guard);
    handle.unmount().expect("unmount");
}

#[test]
fn legal_manifest_under_a_raised_ceiling_stays_parsed_across_a_real_rename() {
    skip_if_no_fuse!();

    // The raised-ceiling arm through the same real rename chain: a 1.5 MiB
    // manifest that is legal under the 2 MiB configured ceiling (loader,
    // reads, listings) must stay fully parsed across the rename. With the
    // default ceiling in the refresh the same rename degraded the entry
    // to the "renamed, awaiting SKILL.md update" placeholder forever —
    // this synchronous refresh is the only store update a rename gets.
    let source = tempfile::tempdir().expect("source tempdir");
    write_sized_skill(&source.path().join("big-skill"), "big-skill", 1536 * 1024);

    let config = ParseConfig {
        max_skill_size: 2 * 1024 * 1024,
        ..ParseConfig::default()
    };
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &config);
    assert!(
        store
            .get("big-skill")
            .expect("the 1.5 MiB skill loads under the 2 MiB ceiling")
            .parse_status
            .is_ok()
    );
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().expect("mount tempdir");
    let handle = mount_with_ceiling(
        mountpoint.path(),
        source.path(),
        shared.clone(),
        2 * 1024 * 1024,
    );

    std::fs::rename(
        mountpoint.path().join("skills/big-skill"),
        mountpoint.path().join("skills/renamed-skill"),
    )
    .expect("rename the skill directory through the mount");

    let guard = shared.read();
    let entry = guard.get("renamed-skill").expect("refreshed store entry");
    assert!(
        entry.parse_status.is_ok(),
        "a manifest legal under the raised ceiling must stay fully parsed, got {:?}",
        entry.parse_status
    );
    assert!(
        entry.body.len() > 1024 * 1024,
        "the legal body must be retained"
    );
    drop(guard);
    handle.unmount().expect("unmount");
}

#[test]
fn initially_refused_manifest_stays_out_of_the_store_across_a_real_rename() {
    skip_if_no_fuse!();

    // Boundary control: a manifest the initial load refused has no store
    // entry, so rename_impl's rename_source_backs_store_entry guard never
    // calls the refresh — the rename must not insert anything either.
    // This pins the guard on both the pristine and the fixed tree.
    //
    // Hermes layout: nested paths are classified by path shape (not by
    // store presence), so the refused directory stays reachable for a
    // real FUSE rename — the same reachable-rename shape the reviewer
    // used to verify the guard.
    let source = tempfile::tempdir().expect("source tempdir");
    write_sized_skill(
        &source.path().join("alpha/big-skill"),
        "big-skill",
        96 * 1024,
    );

    let config = ParseConfig {
        max_skill_size: 64 * 1024,
        ..ParseConfig::default()
    };
    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &config);
    assert!(
        store.get("big-skill").is_none(),
        "the loader must refuse the over-ceiling manifest up front"
    );
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().expect("mount tempdir");
    let handle = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared.clone(),
        MountOptions::default(),
        true,
        MountConfig {
            skill_layout: Some(SkillLayout::Hermes),
            max_skill_size: Some(64 * 1024),
            ..MountConfig::default()
        },
    )
    .expect("mount the hermes workspace with the lowered ceiling");
    std::thread::sleep(Duration::from_millis(300));

    std::fs::rename(
        mountpoint.path().join("alpha/big-skill"),
        mountpoint.path().join("alpha/renamed-skill"),
    )
    .expect("rename the refused skill directory through the mount");

    assert!(
        source.path().join("alpha/renamed-skill/SKILL.md").is_file(),
        "the physical rename must have happened"
    );
    assert!(
        !source.path().join("alpha/big-skill").exists(),
        "the old physical directory must be gone"
    );
    let guard = shared.read();
    assert!(
        guard.get("big-skill").is_none(),
        "the refused manifest must stay out of the store under its old name"
    );
    assert!(
        guard.get("renamed-skill").is_none(),
        "a rename must not insert a store entry for a directory that never backed one"
    );
    drop(guard);
    handle.unmount().expect("unmount");
}
