//! Outcome contract of the background mount entry points.
//!
//! `mount_background_configured` (and the deprecated background funnel it
//! mirrors) used to sleep for a fixed 100 ms and then return
//! `Ok(MountHandle)` unconditionally: a mount that failed before or while
//! creating the FUSE session was reported only to a tracing subscriber the
//! callers never install, and `Ok` did not mean the mountpoint was being
//! served — the background thread may still have been running the
//! environment-profile probe loop, which takes seconds on a loaded host.
//!
//! These tests pin the fixed contract:
//!
//! * a failed background mount is returned as `Err` to the caller, with the
//!   original error and no mount left behind;
//! * `Ok` is only returned once the mountpoint is live in `/proc/mounts`;
//! * the deprecated background entry points surface failures too.
//!
//! All tests skip cleanly when FUSE is unavailable.

mod common;

use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::proc_mounts;
use skillfs_fuse::{FuseError, MountConfig, MountOptions, mount_background_configured};

/// Authoritative liveness probe, mirroring `MountHandle`'s own check: the
/// mountpoint must currently appear in `/proc/mounts`.
fn mount_is_live(mountpoint: &Path) -> bool {
    match std::fs::read("/proc/mounts") {
        Ok(table) => proc_mounts::mounts_contain_target(&table, mountpoint.as_os_str().as_bytes()),
        Err(_) => false,
    }
}

fn shared_store(source: &Path) -> SharedSkillStore {
    let mut store = SkillStore::new();
    store.load_from_directory(source, &ParseConfig::default());
    Arc::new(RwLock::new(store))
}

/// A mount whose mountpoint does not exist must fail *visibly*: the error
/// is returned to the caller, not only logged to a tracing subscriber that
/// tests (and most embedders) never install.
#[cfg(target_os = "linux")]
#[test]
fn a_failed_background_mount_returns_its_error() {
    if !common::fuse_available() {
        return;
    }
    let source = tempfile::tempdir().expect("source tempdir");
    common::create_skill_dir(source.path(), "web");
    let parent = tempfile::tempdir().expect("parent tempdir");
    let missing = parent.path().join("missing-mountpoint");

    let started = Instant::now();
    let error = mount_background_configured(
        &missing,
        source.path(),
        shared_store(source.path()),
        MountOptions::default(),
        false,
        MountConfig::default(),
    )
    .err()
    .expect("a mount on a nonexistent mountpoint must return Err, not a handle");
    assert!(
        matches!(error, FuseError::InvalidMountPoint(ref message) if message.contains("does not exist")),
        "the original mount failure must be surfaced, got: {error:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "fast validation failures must surface promptly, took {:?}",
        started.elapsed()
    );
    assert!(
        !mount_is_live(&missing),
        "no mount may be left behind for a failed background mount"
    );
}

/// `Ok` may only be handed out once the FUSE session actually serves the
/// mountpoint. The pre-fix code returned after a fixed 100 ms sleep while
/// the background thread was still probing the environment profile, so on
/// a loaded host the caller received a handle to a mount that did not
/// exist yet.
#[cfg(target_os = "linux")]
#[test]
fn a_successful_background_mount_is_live_when_ok_returns() {
    if !common::fuse_available() {
        return;
    }
    let source = tempfile::tempdir().expect("source tempdir");
    common::create_skill_dir(source.path(), "web");
    let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");

    let handle = match mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared_store(source.path()),
        MountOptions::default(),
        true,
        MountConfig::default(),
    ) {
        Ok(handle) => handle,
        Err(error) => panic!("background mount must succeed, got: {error:?}"),
    };

    assert!(
        mount_is_live(mountpoint.path()),
        "Ok must only be returned once the mountpoint is served by the FUSE session"
    );

    handle.unmount().expect("unmount");
    assert!(
        !mount_is_live(mountpoint.path()),
        "unmount must tear the live mount down"
    );
}

/// The deprecated background entry points funnel into the same
/// spawn-and-wait path, so a failed mount is visible through them too.
#[cfg(target_os = "linux")]
#[test]
fn a_failed_deprecated_background_mount_returns_its_error() {
    if !common::fuse_available() {
        return;
    }
    let source = tempfile::tempdir().expect("source tempdir");
    common::create_skill_dir(source.path(), "web");
    let parent = tempfile::tempdir().expect("parent tempdir");
    let missing = parent.path().join("missing-mountpoint");

    #[allow(deprecated)]
    let result = skillfs_fuse::mount_background(
        &missing,
        source.path(),
        shared_store(source.path()),
        MountOptions::default(),
        false,
    );
    assert!(
        result.is_err(),
        "the deprecated background entry points must surface failures too"
    );
}
