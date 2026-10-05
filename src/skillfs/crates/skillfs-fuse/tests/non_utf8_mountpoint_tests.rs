//! Teardown of mounts whose mountpoint contains bytes that are not valid
//! UTF-8.
//!
//! The CLI accepts such mountpoints since `args_os` capture
//! (`fix(skillfs): accept non-UTF-8 CLI arguments`), and the FUSE session
//! itself addresses the path as raw bytes. Mount *detection* is byte-exact
//! (`proc_mounts::mounts_contain_target`), so every unmount invocation must
//! address the same bytes: a `to_string_lossy()` view replaces the invalid
//! byte with U+FFFD, `fusermount3 -u` then names a nonexistent path, and
//! the bounded force-unmount loop retries the wrong path until the mount
//! leaks.
//!
//! The argv-level contract is pinned deterministically by the unit test
//! `unmount_commands_pass_the_mountpoint_as_raw_bytes`; this file exercises
//! the end-to-end teardown route on environments where FUSE can mount.

#![allow(unused_imports)]

mod common;

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use skillfs_core::store::SkillStore;
use skillfs_core::{ParseConfig, SharedSkillStore};
use skillfs_fuse::{MountOptions, mount_background_configured};

/// Byte-exact mount probe: same matcher the production teardown uses.
fn is_mounted(path: &std::path::Path) -> bool {
    let Ok(mounts) = std::fs::read("/proc/mounts") else {
        return false;
    };
    skillfs_fuse::proc_mounts::mounts_contain_target(&mounts, path.as_os_str().as_bytes())
}

/// Correct-byte best-effort cleanup so a failing (red) run does not leak
/// the mount into the developer environment: invoke fusermount3 with the
/// raw OS string the production code should have used.
fn cleanup_mount(path: &std::path::Path) {
    for _ in 0..10 {
        if !is_mounted(path) {
            return;
        }
        let _ = std::process::Command::new("fusermount3")
            .arg("-u")
            .arg(path.as_os_str())
            .output();
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Wait up to `timeout` for the mount to become visible. `mount_background_configured`
/// returns `Ok` even when its background session thread failed to mount, so
/// visibility — not the handle — is the truth. Environments whose kernel
/// cannot mount FUSE at all (e.g. some containers) make this never fire;
/// the caller then skips, because only a live mount can prove teardown.
fn wait_until_mounted(path: &std::path::Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if is_mounted(path) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn teardown_unmounts_a_non_utf8_mountpoint() {
    if !common::fuse_available() {
        eprintln!("SKIP {}: FUSE not available", std::file!());
        return;
    }

    let base = tempfile::tempdir().expect("base tempdir");
    // Mountpoint with a byte that is not valid UTF-8: the CLI accepts it
    // (args_os) and the FUSE session mounts it as raw bytes.
    let mountpoint: PathBuf = base.path().join(OsStr::from_bytes(b"mount-\xff"));
    std::fs::create_dir(&mountpoint).expect("create non-UTF-8 mountpoint");

    let source = tempfile::tempdir().expect("source tempdir");
    let skill_dir = source.path().join("alpha");
    std::fs::create_dir(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: alpha\ndescription: demo skill\n---\n\nBody.\n",
    )
    .expect("write SKILL.md");

    let mut store = SkillStore::new();
    store.load_from_directory(source.path(), &ParseConfig::default());
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let handle = mount_background_configured(
        &mountpoint,
        source.path(),
        shared,
        MountOptions::default(),
        false,
        Default::default(),
    )
    .expect("mount at non-UTF-8 mountpoint");

    // The background session mounts asynchronously; a mount that never
    // appears means this environment cannot mount FUSE (the unit test pins
    // the argv contract), so skip rather than fail for an unrelated reason.
    if !wait_until_mounted(&mountpoint, Duration::from_secs(5)) {
        eprintln!(
            "SKIP {}: FUSE session did not mount in this environment",
            std::file!()
        );
        drop(handle);
        return;
    }

    // Teardown route used by every embedder: Drop runs unmount_inner and
    // then the bounded force-unmount fallback.
    drop(handle);

    let unmounted = !is_mounted(&mountpoint);
    if !unmounted {
        // Red-run hygiene: remove the mount this test just leaked so the
        // failure does not poison the environment for later runs.
        cleanup_mount(&mountpoint);
    }
    assert!(
        unmounted,
        "dropping the handle must unmount the non-UTF-8 mountpoint; \
         fusermount3 received a lossy (U+FFFD-mangled) path instead of the \
         raw mountpoint bytes"
    );
}
