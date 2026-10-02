//! Integration tests for SkillFS FUSE write passthrough behavior.
//!
//! Tests verify:
//!   - Read operations work correctly (readdir, read SKILL.md)
//!   - Write operations passthrough to the physical filesystem
//!   - New skill dirs appear in readdir immediately after mkdir (no async window)
//!   - Renamed skill dir is visible under new name immediately (no empty window)
//!   - Post-rename write with stale frontmatter does not resurrect old name
//!   - All three behaviors above also hold in in-place mount mode
//!   - Operations always rejected (mknod, symlink, link) return EROFS
//!
//! These tests require:
//!   - `/dev/fuse` to be accessible (Linux FUSE support)
//!   - The `fusermount3` binary to be available for cleanup
//!
//! If the environment cannot mount FUSE the tests are skipped gracefully.

use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, store::SkillStore};
use skillfs_fuse::{MountConfig, MountOptions, mount_background_configured};

mod common;

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Returns `true` when FUSE is likely usable in this environment.
fn fuse_available() -> bool {
    common::fuse_available()
}

/// Build a SharedSkillStore from the test fixture directory.
fn fixture_store() -> skillfs_core::SharedSkillStore {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

    let mut store = SkillStore::new();
    let config = ParseConfig::default();
    let errors = store.load_from_directory(&fixture_dir, &config);
    if !errors.is_empty() {
        eprintln!("fixture load warnings: {:?}", errors);
    }
    Arc::new(RwLock::new(store))
}

/// Assert the error is EROFS (os error 30 = Read-only file system).
fn assert_erofs(result: std::io::Result<()>, op: &str) {
    match result {
        Err(e) if e.raw_os_error() == Some(libc::EROFS) => { /* expected */ }
        Err(e) if e.kind() == ErrorKind::ReadOnlyFilesystem => { /* expected (stable alias) */ }
        Err(e) => panic!("{op}: expected EROFS (os error 30), got: {e}"),
        Ok(()) => panic!("{op}: expected EROFS but operation succeeded"),
    }
}

/// Assert the operation succeeded.
fn assert_ok(result: std::io::Result<()>, op: &str) {
    if let Err(e) = result {
        panic!("{op}: expected success, got: {e}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: write passthrough succeeds; always-rejected ops return EROFS
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_write_ops_return_erofs() {
    if !fuse_available() {
        eprintln!("SKIP test_write_ops_return_erofs: FUSE not available");
        return;
    }

    let mountpoint = tempfile::tempdir().expect("tempdir");
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

    let store = fixture_store();
    let opts = MountOptions::default();

    let handle = mount_background_configured(
        mountpoint.path(),
        &fixture_dir,
        store,
        opts,
        false,
        MountConfig::default(),
    )
    .expect("mount_background_configured");

    // Give the FUSE daemon time to start serving.
    std::thread::sleep(Duration::from_millis(300));

    let skills_dir = mountpoint.path().join("skills");
    let skill_dir = skills_dir.join("test-skill");
    let skill_md = skill_dir.join("SKILL.md");

    // ── write: open SKILL.md for writing (passthrough) ────────────────────
    assert_ok(
        std::fs::OpenOptions::new()
            .write(true)
            .open(&skill_md)
            .map(|_| ()),
        "write/open-for-write",
    );

    // ── create: create a new file inside skill dir (passthrough) ────────────
    assert_ok(
        std::fs::write(skill_dir.join("new_file.txt"), b"hello"),
        "create+write",
    );
    let _ = std::fs::remove_file(skill_dir.join("new_file.txt"));

    // ── mkdir: create a new skill directory (passthrough) ───────────────────
    assert_ok(std::fs::create_dir(skills_dir.join("new-skill")), "mkdir");
    let _ = std::fs::remove_dir(skills_dir.join("new-skill"));

    // ── symlink: always rejected (no physical mapping at root level) ─────────
    assert_erofs(
        std::os::unix::fs::symlink("/tmp/target", skills_dir.join("link")),
        "symlink",
    );

    // ── Cleanup ──────────────────────────────────────────────────────────────
    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────
// Smoke test: read operations still work after mount
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_read_ops_succeed() {
    if !fuse_available() {
        eprintln!("SKIP test_read_ops_succeed: FUSE not available");
        return;
    }

    let mountpoint = tempfile::tempdir().expect("tempdir");
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

    let store = fixture_store();
    let opts = MountOptions::default();

    let handle = mount_background_configured(
        mountpoint.path(),
        &fixture_dir,
        store,
        opts,
        false,
        MountConfig::default(),
    )
    .expect("mount_background_configured");

    std::thread::sleep(Duration::from_millis(300));

    // /skills directory must be listable
    let skills_dir = mountpoint.path().join("skills");
    let entries: Vec<_> = std::fs::read_dir(&skills_dir)
        .expect("read_dir /skills")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    // At least test-skill and skill-discover should appear.
    assert!(
        entries.iter().any(|n| n == "test-skill"),
        "test-skill not found in /skills, got: {:?}",
        entries
    );
    assert!(
        entries.iter().any(|n| n == "skill-discover"),
        "skill-discover not found in /skills, got: {:?}",
        entries
    );

    // SKILL.md must be readable and its content must contain compiled output
    // (not raw frontmatter) — this guards the read path / compilation pipeline.
    let content = std::fs::read_to_string(mountpoint.path().join("skills/test-skill/SKILL.md"))
        .expect("read SKILL.md");
    assert!(!content.is_empty(), "SKILL.md compiled output is empty");
    assert!(
        content.contains("test-skill"),
        "SKILL.md compiled output does not mention 'test-skill': {content}"
    );

    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────
// Immediate consistency: mkdir new skill dir → visible in readdir at once
// ─────────────────────────────────────────────────────────────────────────────

/// After `mkdir /skills/new-skill` the new directory must appear in
/// `readdir /skills` without any sleep or retry.
#[test]
fn test_mkdir_skill_immediately_visible() {
    if !fuse_available() {
        eprintln!("SKIP test_mkdir_skill_immediately_visible: FUSE not available");
        return;
    }

    // Use a temp dir as the source so the fixture is clean each run.
    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    // Pre-populate one skill so the store is non-empty.
    let existing = source_dir.path().join("existing-skill");
    std::fs::create_dir(&existing).unwrap();
    std::fs::write(
        existing.join("SKILL.md"),
        b"---\nname: existing-skill\ndescription: pre-existing\n---\n",
    )
    .unwrap();

    let store = {
        use skillfs_core::ParseConfig;
        use skillfs_core::store::SkillStore;
        let mut s = SkillStore::new();
        s.load_from_directory(source_dir.path(), &ParseConfig::default());
        Arc::new(RwLock::new(s))
    };

    let handle = mount_background_configured(
        mountpoint.path(),
        source_dir.path(),
        store,
        MountOptions::default(),
        false,
        MountConfig::default(),
    )
    .expect("mount_background_configured");
    std::thread::sleep(Duration::from_millis(300));

    let skills_dir = mountpoint.path().join("skills");

    // Create a new skill directory through the mount.
    std::fs::create_dir(skills_dir.join("brand-new-skill")).expect("mkdir brand-new-skill");

    // The new skill must be immediately visible in readdir — no sleep.
    let entries: Vec<String> = std::fs::read_dir(&skills_dir)
        .expect("read_dir /skills")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    assert!(
        entries.iter().any(|n| n == "brand-new-skill"),
        "brand-new-skill not immediately visible in /skills, got: {:?}",
        entries
    );

    // Cleanup: rmdir through mount, then unmount.
    let _ = std::fs::remove_dir(skills_dir.join("brand-new-skill"));
    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

/// With views config present: if the new skill name is already listed in the
/// default view, `mkdir` must make it visible in `/skills` immediately.
#[test]
fn test_mkdir_skill_visible_with_views_config() {
    if !fuse_available() {
        eprintln!("SKIP test_mkdir_skill_visible_with_views_config: FUSE not available");
        return;
    }

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    // Seed one existing skill so mount has initial content.
    let existing = source_dir.path().join("existing-skill");
    std::fs::create_dir(&existing).unwrap();
    std::fs::write(
        existing.join("SKILL.md"),
        b"---\nname: existing-skill\ndescription: pre-existing\n---\n",
    )
    .unwrap();

    // Pre-create views config where the to-be-created skill is in default view.
    std::fs::write(
        source_dir.path().join("skillfs-views.toml"),
        r#"[[view]]
name = "major"
default = true
description = "Core skills"
skills = ["existing-skill", "brand-new-skill"]

[[view]]
name = "other"
default = false
description = "Other skills"
skills = []
"#,
    )
    .unwrap();

    let store = {
        use skillfs_core::ParseConfig;
        use skillfs_core::store::SkillStore;
        let mut s = SkillStore::new();
        s.load_from_directory(source_dir.path(), &ParseConfig::default());
        Arc::new(RwLock::new(s))
    };

    let handle = mount_background_configured(
        mountpoint.path(),
        source_dir.path(),
        store,
        MountOptions::default(),
        false,
        MountConfig::default(),
    )
    .expect("mount_background_configured");
    std::thread::sleep(Duration::from_millis(300));

    let skills_dir = mountpoint.path().join("skills");

    // Before mkdir, brand-new-skill should not be visible because it is not in store yet.
    let before: Vec<String> = std::fs::read_dir(&skills_dir)
        .expect("read_dir /skills before mkdir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        !before.iter().any(|n| n == "brand-new-skill"),
        "brand-new-skill unexpectedly visible before mkdir, got: {:?}",
        before
    );

    // Create through mount; placeholder upsert should make it visible immediately.
    std::fs::create_dir(skills_dir.join("brand-new-skill")).expect("mkdir brand-new-skill");

    // Lookup dimension: stat on the new path must succeed immediately,
    // proving lookup resolves the new skill directory right away.
    let new_skill_path = skills_dir.join("brand-new-skill");
    let md =
        std::fs::metadata(&new_skill_path).expect("metadata /skills/brand-new-skill after mkdir");
    assert!(md.is_dir(), "brand-new-skill exists but is not a directory");

    let after: Vec<String> = std::fs::read_dir(&skills_dir)
        .expect("read_dir /skills after mkdir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    assert!(
        after.iter().any(|n| n == "brand-new-skill"),
        "brand-new-skill not immediately visible with views config, got: {:?}",
        after
    );

    let _ = std::fs::remove_dir(skills_dir.join("brand-new-skill"));
    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────
// Immediate consistency: rename skill dir → no empty window
// ─────────────────────────────────────────────────────────────────────────────

/// After `rename /skills/old-name /skills/new-name`:
///   - new-name must appear in readdir immediately
///   - old-name must no longer appear in readdir
///   - there must be no point where both are absent simultaneously
///     (verified by checking the moment immediately after rename returns)
#[test]
fn test_rename_skill_no_empty_window() {
    if !fuse_available() {
        eprintln!("SKIP test_rename_skill_no_empty_window: FUSE not available");
        return;
    }

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    // Create the skill to be renamed.
    let skill_dir = source_dir.path().join("old-name");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        b"---\nname: old-name\ndescription: to be renamed\n---\n",
    )
    .unwrap();

    let store = {
        use skillfs_core::ParseConfig;
        use skillfs_core::store::SkillStore;
        let mut s = SkillStore::new();
        s.load_from_directory(source_dir.path(), &ParseConfig::default());
        Arc::new(RwLock::new(s))
    };

    let handle = mount_background_configured(
        mountpoint.path(),
        source_dir.path(),
        store,
        MountOptions::default(),
        false,
        MountConfig::default(),
    )
    .expect("mount_background_configured");
    std::thread::sleep(Duration::from_millis(300));

    let skills_dir = mountpoint.path().join("skills");

    // Verify old-name is visible before rename.
    let before: Vec<String> = std::fs::read_dir(&skills_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        before.iter().any(|n| n == "old-name"),
        "old-name not visible before rename: {:?}",
        before
    );

    // Rename through the mount.
    std::fs::rename(skills_dir.join("old-name"), skills_dir.join("new-name")).expect("rename");

    // Immediately after rename: new-name must be visible, old-name must be gone.
    let after: Vec<String> = std::fs::read_dir(&skills_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    assert!(
        after.iter().any(|n| n == "new-name"),
        "new-name not immediately visible after rename, got: {:?}",
        after
    );
    assert!(
        !after.iter().any(|n| n == "old-name"),
        "old-name still visible after rename, got: {:?}",
        after
    );

    // Cleanup
    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────
// In-place mode: source == mountpoint, /proc/self/fd bypass
// ─────────────────────────────────────────────────────────────────────────────
//
// In-place mode mounts the FUSE layer on top of the source directory itself.
// Physical writes are routed through /proc/self/fd/{n} to bypass the mount.
// The root of the mount IS the skills directory (no /skills/ prefix).

/// Helper: build a store from `dir` and mount in-place (`source == mountpoint`).
/// Returns the mount handle; the caller owns the TempDir lifetime.
fn mount_inplace(dir: &std::path::Path) -> skillfs_fuse::MountHandle {
    let mut s = SkillStore::new();
    s.load_from_directory(dir, &ParseConfig::default());
    let store = Arc::new(RwLock::new(s));
    mount_background_configured(
        dir,
        dir,
        store,
        MountOptions::default(),
        true,
        MountConfig::default(),
    )
    .expect("mount_background_configured")
}

// ─────────────────────────────────────────────────────────────────────────────

/// In-place mode: mkdir a new skill dir at the root → immediately visible.
#[test]
fn test_inplace_mkdir_immediately_visible() {
    if !fuse_available() {
        eprintln!("SKIP test_inplace_mkdir_immediately_visible: FUSE not available");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");

    // Pre-populate one skill so the store is non-empty on mount.
    std::fs::create_dir(dir.path().join("seed-skill")).unwrap();
    std::fs::write(
        dir.path().join("seed-skill/SKILL.md"),
        b"---\nname: seed-skill\ndescription: seed\n---\n",
    )
    .unwrap();

    let handle = mount_inplace(dir.path());
    std::thread::sleep(Duration::from_millis(300));

    // Create a new skill directory directly under the mountpoint.
    std::fs::create_dir(dir.path().join("inplace-new")).expect("mkdir inplace-new");

    // Must be immediately visible — no sleep.
    let entries: Vec<String> = std::fs::read_dir(dir.path())
        .expect("readdir mountpoint")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    assert!(
        entries.iter().any(|n| n == "inplace-new"),
        "inplace-new not immediately visible in in-place mount root, got: {:?}",
        entries
    );

    let _ = std::fs::remove_dir(dir.path().join("inplace-new"));
    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &dir.path().to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────

/// In-place mode: rename skill dir → new name visible immediately, no empty window.
#[test]
fn test_inplace_rename_no_empty_window() {
    if !fuse_available() {
        eprintln!("SKIP test_inplace_rename_no_empty_window: FUSE not available");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");

    std::fs::create_dir(dir.path().join("ip-old")).unwrap();
    std::fs::write(
        dir.path().join("ip-old/SKILL.md"),
        b"---\nname: ip-old\ndescription: to rename\n---\n",
    )
    .unwrap();

    let handle = mount_inplace(dir.path());
    std::thread::sleep(Duration::from_millis(300));

    // Verify ip-old visible before rename.
    let before: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        before.iter().any(|n| n == "ip-old"),
        "ip-old not visible before rename: {:?}",
        before
    );

    // Rename through the mount.
    std::fs::rename(dir.path().join("ip-old"), dir.path().join("ip-new")).expect("rename");

    // Immediately after: ip-new visible, ip-old gone.
    let after: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    assert!(
        after.iter().any(|n| n == "ip-new"),
        "ip-new not immediately visible after rename (in-place), got: {:?}",
        after
    );
    assert!(
        !after.iter().any(|n| n == "ip-old"),
        "ip-old still visible after rename (in-place), got: {:?}",
        after
    );

    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &dir.path().to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────

/// In-place mode: rename, then write to SKILL.md with stale frontmatter name →
/// old name must not resurrect after the async Reparse fires.
#[test]
fn test_inplace_post_rename_write_does_not_resurrect_old_name() {
    if !fuse_available() {
        eprintln!(
            "SKIP test_inplace_post_rename_write_does_not_resurrect_old_name: FUSE not available"
        );
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");

    std::fs::create_dir(dir.path().join("ip2-old")).unwrap();
    std::fs::write(
        dir.path().join("ip2-old/SKILL.md"),
        b"---\nname: ip2-old\ndescription: original\n---\n",
    )
    .unwrap();

    let handle = mount_inplace(dir.path());
    std::thread::sleep(Duration::from_millis(300));

    // Step 1: rename.
    std::fs::rename(dir.path().join("ip2-old"), dir.path().join("ip2-new")).expect("rename");

    // Sanity check.
    let after_rename: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        after_rename.iter().any(|n| n == "ip2-new"),
        "ip2-new not visible after rename: {:?}",
        after_rename
    );

    // Step 2: append to SKILL.md without updating `name:` frontmatter.
    let skill_md = dir.path().join("ip2-new/SKILL.md");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&skill_md)
        .expect("open SKILL.md");
    use std::io::Write as _;
    writeln!(f, "\nextra content after rename").expect("write");
    drop(f);

    // Wait for debounce + async Reparse to fire.
    std::thread::sleep(Duration::from_millis(300));

    // ip2-old must not reappear; ip2-new must still be present.
    let after_write: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    assert!(
        !after_write.iter().any(|n| n == "ip2-old"),
        "ip2-old resurrected in in-place mode, got: {:?}",
        after_write
    );
    assert!(
        after_write.iter().any(|n| n == "ip2-new"),
        "ip2-new disappeared in in-place mode, got: {:?}",
        after_write
    );

    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &dir.path().to_string_lossy()])
        .output();
}
// ─────────────────────────────────────────────────────────────────────────────
// In-place mode: new-skill authoring (issue #1264)
// ─────────────────────────────────────────────────────────────────────────────

/// Set atime/mtime on `path` via `utimensat`, emulating what `cp -a` does
/// when preserving timestamps on the top-level skill directory.
fn set_times_now(path: &std::path::Path) -> std::io::Result<()> {
    let c_path = std::ffi::CString::new(path.to_string_lossy().into_owned())
        .expect("CString path for utimensat");
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_NOW,
        },
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_NOW,
        },
    ];
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// In-place mode: a freshly `mkdir`'d skill directory with no physical
/// SKILL.md must list as empty — no phantom, broken SKILL.md entry.
#[test]
fn test_inplace_new_skill_dir_has_no_phantom_skill_md() {
    if !fuse_available() {
        eprintln!("SKIP test_inplace_new_skill_dir_has_no_phantom_skill_md: FUSE not available");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");

    std::fs::create_dir(dir.path().join("seed-skill")).unwrap();
    std::fs::write(
        dir.path().join("seed-skill/SKILL.md"),
        b"---\nname: seed-skill\ndescription: seed\n---\n",
    )
    .unwrap();

    let handle = mount_inplace(dir.path());
    std::thread::sleep(Duration::from_millis(300));

    let new_skill = dir.path().join("test-ws");
    std::fs::create_dir(&new_skill).expect("mkdir test-ws");

    // Listing the new empty skill dir must not contain SKILL.md.
    let entries: Vec<String> = std::fs::read_dir(&new_skill)
        .expect("read_dir test-ws")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        !entries.iter().any(|n| n == "SKILL.md"),
        "phantom SKILL.md listed for empty new skill dir, got: {:?}",
        entries
    );
    assert!(
        entries.is_empty(),
        "new empty skill dir should list as empty, got: {:?}",
        entries
    );

    // Direct probe of the phantom path must be ENOENT, not a broken entry.
    let probe = std::fs::metadata(new_skill.join("SKILL.md"));
    assert!(
        probe.is_err(),
        "SKILL.md must not stat in an empty new skill dir"
    );

    // A complete existing skill must still list its SKILL.md.
    let seed_entries: Vec<String> = std::fs::read_dir(dir.path().join("seed-skill"))
        .expect("read_dir seed-skill")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        seed_entries.iter().any(|n| n == "SKILL.md"),
        "existing complete skill lost its SKILL.md listing, got: {:?}",
        seed_entries
    );

    let _ = std::fs::remove_dir(&new_skill);
    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &dir.path().to_string_lossy()])
        .output();
}

/// In-place mode: mkdir a new skill dir then write its SKILL.md — the file
/// must become physically present, stat/read must succeed, and the store
/// reparse must surface the skill with compiled content.
#[test]
fn test_inplace_new_skill_write_skill_md() {
    if !fuse_available() {
        eprintln!("SKIP test_inplace_new_skill_write_skill_md: FUSE not available");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");

    std::fs::create_dir(dir.path().join("seed-skill")).unwrap();
    std::fs::write(
        dir.path().join("seed-skill/SKILL.md"),
        b"---\nname: seed-skill\ndescription: seed\n---\n",
    )
    .unwrap();

    let handle = mount_inplace(dir.path());
    std::thread::sleep(Duration::from_millis(300));

    let new_skill = dir.path().join("authored");
    std::fs::create_dir(&new_skill).expect("mkdir authored");

    // Write the manifest through the mount (create + write path).
    let skill_md = new_skill.join("SKILL.md");
    std::fs::write(
        &skill_md,
        b"---\nname: authored\ndescription: authored in place\n---\n",
    )
    .expect("write SKILL.md");

    // stat must now succeed.
    let md = std::fs::metadata(&skill_md).expect("metadata SKILL.md after write");
    assert!(md.is_file(), "SKILL.md exists but is not a regular file");

    // read must return compiled content mentioning the skill name.
    let content = std::fs::read_to_string(&skill_md).expect("read SKILL.md after write");
    assert!(
        content.contains("authored"),
        "compiled SKILL.md does not mention skill name: {content}"
    );

    // SKILL.md must now appear in the skill dir listing.
    let entries: Vec<String> = std::fs::read_dir(&new_skill)
        .expect("read_dir authored")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        entries.iter().any(|n| n == "SKILL.md"),
        "SKILL.md not listed after write, got: {:?}",
        entries
    );

    // Reparse should keep the skill visible at the root.
    std::thread::sleep(Duration::from_millis(300));
    let root: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read_dir root")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        root.iter().any(|n| n == "authored"),
        "authored skill not visible at root after reparse, got: {:?}",
        root
    );

    let _ = std::fs::remove_file(&skill_md);
    let _ = std::fs::remove_dir(&new_skill);
    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &dir.path().to_string_lossy()])
        .output();
}

/// In-place mode: `cp -a`-style metadata preservation (chmod + utimensat)
/// on an ordinary top-level skill directory must succeed rather than
/// failing with EROFS.
#[test]
fn test_inplace_skill_dir_metadata_preservation() {
    if !fuse_available() {
        eprintln!("SKIP test_inplace_skill_dir_metadata_preservation: FUSE not available");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");

    std::fs::create_dir(dir.path().join("seed-skill")).unwrap();
    std::fs::write(
        dir.path().join("seed-skill/SKILL.md"),
        b"---\nname: seed-skill\ndescription: seed\n---\n",
    )
    .unwrap();

    let handle = mount_inplace(dir.path());
    std::thread::sleep(Duration::from_millis(300));

    let new_skill = dir.path().join("copied-skill");
    std::fs::create_dir(&new_skill).expect("mkdir copied-skill");

    // chmod (setattr mode) on the top-level skill dir must succeed.
    use std::os::unix::fs::PermissionsExt as _;
    assert_ok(
        std::fs::set_permissions(&new_skill, std::fs::Permissions::from_mode(0o755)),
        "chmod skill dir",
    );

    // utimensat (setattr atime/mtime) on the top-level skill dir must succeed.
    assert_ok(set_times_now(&new_skill), "utimensat skill dir");

    let _ = std::fs::remove_dir(&new_skill);
    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &dir.path().to_string_lossy()])
        .output();
}

// ─────────────────────────────────────────────────────────────────────────────
// Post-rename write: stale frontmatter name must not resurrect old entry
// ─────────────────────────────────────────────────────────────────────────────

/// Regression test: rename old-name → new-name, then write to new-name/SKILL.md
/// with the frontmatter `name:` field still set to `old-name`.
/// The async Reparse must not re-insert `old-name` into the store,
/// so `old-name` must not reappear in readdir after the write settles.
#[test]
fn test_post_rename_write_does_not_resurrect_old_name() {
    if !fuse_available() {
        eprintln!("SKIP test_post_rename_write_does_not_resurrect_old_name: FUSE not available");
        return;
    }

    let source_dir = tempfile::tempdir().expect("source tempdir");
    let mountpoint = tempfile::tempdir().expect("mount tempdir");

    // Seed with the skill to be renamed.
    let skill_dir = source_dir.path().join("old-name");
    std::fs::create_dir(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        b"---\nname: old-name\ndescription: original\n---\n",
    )
    .unwrap();

    let store = {
        use skillfs_core::ParseConfig;
        use skillfs_core::store::SkillStore;
        let mut s = SkillStore::new();
        s.load_from_directory(source_dir.path(), &ParseConfig::default());
        Arc::new(RwLock::new(s))
    };

    let handle = mount_background_configured(
        mountpoint.path(),
        source_dir.path(),
        store,
        MountOptions::default(),
        false,
        MountConfig::default(),
    )
    .expect("mount_background_configured");
    std::thread::sleep(Duration::from_millis(300));

    let skills_dir = mountpoint.path().join("skills");

    // Step 1: rename the skill directory.
    std::fs::rename(skills_dir.join("old-name"), skills_dir.join("new-name")).expect("rename");

    // Sanity: new-name visible, old-name gone.
    let after_rename: Vec<String> = std::fs::read_dir(&skills_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        after_rename.iter().any(|n| n == "new-name"),
        "new-name not visible right after rename: {:?}",
        after_rename
    );

    // Step 2: append to SKILL.md *without* updating the `name:` frontmatter field.
    // The file still contains `name: old-name`.
    let skill_md = skills_dir.join("new-name").join("SKILL.md");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&skill_md)
        .expect("open SKILL.md for append");
    use std::io::Write as _;
    writeln!(f, "\nsome extra content").expect("write");
    drop(f);

    // Wait longer than the 50 ms debounce + processing time for the async
    // Reparse to have fired.
    std::thread::sleep(Duration::from_millis(300));

    // After Reparse settles: old-name must NOT reappear, new-name must still be there.
    let after_write: Vec<String> = std::fs::read_dir(&skills_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    assert!(
        !after_write.iter().any(|n| n == "old-name"),
        "old-name resurrected after post-rename write, got: {:?}",
        after_write
    );
    assert!(
        after_write.iter().any(|n| n == "new-name"),
        "new-name disappeared after post-rename write, got: {:?}",
        after_write
    );

    // Cleanup
    drop(handle);
    std::thread::sleep(Duration::from_millis(200));
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", &mountpoint.path().to_string_lossy()])
        .output();
}
