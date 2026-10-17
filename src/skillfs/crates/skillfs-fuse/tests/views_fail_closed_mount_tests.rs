//! Fail-closed mounting for a corrupt `skillfs-views.toml`.
//!
//! `skillfs classify` refuses a present-but-unparseable views config
//! (fail-closed, file left byte-identical), but the mount path loaded
//! the same file through `ViewsConfig::load`, which returned `None`
//! for absent AND unparseable — and every consumer treated `None` as
//! "no views configured", i.e. every store skill served in the default
//! /skills view (fail-open). A views config is the mount-time allowlist
//! that hides secondary skills from /skills; a corrupt or
//! attacker-truncated file silently undid that hiding for every
//! secondary skill.
//!
//! These tests pin the mount-side semantics after the fix:
//!
//! - corrupt-while-present: the mount refuses (`InvalidViewsConfig`),
//!   and no FUSE session ever comes up;
//! - absent file: the mount still succeeds and serves every skill
//!   (the no-views behavior is unchanged);
//! - valid file: the mount still succeeds and serves only the
//!   configured default-view skill (the refusal is specifically about
//!   corrupt-while-present).
//!
//! The classify-side refusal (unchanged by the fix) is pinned by
//! `classify_views_preserve_tests.rs` in skillfs-cli.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::{MountOptions, mount_background_configured};

#[path = "common/mod.rs"]
mod common;

use common::create_skill_dir;

/// Corrupt TOML (unclosed array-of-tables header, the same shape the
/// classify preserve-tests use) whose intent clearly reserves beta and
/// gamma to a secondary view.
const CORRUPT_VIEWS: &str = "[[view]\nname = \"major\"\ndefault = true\nskills = [\"alpha\"]\n";

const VALID_VIEWS: &str = "[[view]]\nname = \"major\"\ndefault = true\nskills = [\"alpha\"]\n\n\
     [[view]]\nname = \"other\"\ndefault = false\nskills = [\"beta\", \"gamma\"]\n";

fn seed_three_skills(src: &Path, views_file: Option<&str>) {
    create_skill_dir(src, "alpha");
    create_skill_dir(src, "beta");
    create_skill_dir(src, "gamma");
    if let Some(content) = views_file {
        std::fs::write(src.join("skillfs-views.toml"), content).expect("seed views config");
    }
}

fn mounted_skill_names(
    source: &Path,
    views_file: Option<&str>,
) -> Result<Vec<String>, skillfs_fuse::FuseError> {
    seed_three_skills(source, views_file);
    let mut store = SkillStore::new();
    let load_errors = store.load_from_directory(source, &ParseConfig::default());
    assert!(load_errors.is_empty(), "fixture skills must all load");
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().expect("mount tempdir");
    let handle = mount_background_configured(
        mountpoint.path(),
        source,
        shared,
        MountOptions::default(),
        false,
        Default::default(),
    )?;
    std::thread::sleep(Duration::from_millis(300));

    let skills_dir = mountpoint.path().join("skills");
    let listed = std::fs::read_dir(&skills_dir)
        .expect("readdir /skills")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "skill-discover")
        .collect::<Vec<_>>();
    drop(handle);
    unmount_quiet(mountpoint.path());
    Ok(listed)
}

fn unmount_quiet(mountpoint: &Path) {
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("fusermount3")
            .arg("-u")
            .arg(mountpoint.as_os_str())
            .output();
    }
    let _ = mountpoint;
}

/// A corrupt views config must refuse the mount instead of silently
/// widening the default view to every store skill.
#[test]
fn corrupt_views_config_refuses_the_mount() {
    if !common::fuse_available() {
        eprintln!("SKIP corrupt_views_config_refuses_the_mount: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source tempdir");
    seed_three_skills(source.path(), Some(CORRUPT_VIEWS));
    let mut store = SkillStore::new();
    let load_errors = store.load_from_directory(source.path(), &ParseConfig::default());
    assert!(load_errors.is_empty(), "fixture skills must all load");
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().expect("mount tempdir");
    let result = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        Default::default(),
    );

    // `MountHandle` is not `Debug`, so `expect_err` cannot be used here.
    let error = match result {
        Ok(_handle) => panic!("a corrupt views config must refuse the mount"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("skillfs-views.toml"),
        "the refusal names the offending file: {error}"
    );
    assert!(
        error.to_string().contains("could not be read or parsed"),
        "the refusal names the reason: {error}"
    );

    // No FUSE session may come up for the refused mount: /skills never
    // appears, so beta/gamma are never served through the widened view.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !mountpoint.path().join("skills").exists(),
        "the refused mount must not leave a live FUSE session behind"
    );
    // ... and the corrupt file is left byte-identical for the operator.
    assert_eq!(
        std::fs::read_to_string(source.path().join("skillfs-views.toml")).unwrap(),
        CORRUPT_VIEWS
    );
}

/// Control: with no views file at all the mount keeps serving every
/// skill (the no-views behavior is unchanged).
#[test]
fn absent_views_config_still_mounts_everything() {
    if !common::fuse_available() {
        eprintln!("SKIP absent_views_config_still_mounts_everything: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source tempdir");
    let listed = mounted_skill_names(source.path(), None).expect("mount must succeed");
    let mut listed = listed;
    listed.sort();
    assert_eq!(
        listed,
        vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        "no views config => every skill serves in the default view"
    );
}

/// Control: a valid views file keeps restricting /skills to the
/// configured default view, so the refusal above is specifically about
/// the corrupt-while-present case.
#[test]
fn valid_views_config_still_restricts_the_default_view() {
    if !common::fuse_available() {
        eprintln!("SKIP valid_views_config_still_restricts_the_default_view: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source tempdir");
    let listed = mounted_skill_names(source.path(), Some(VALID_VIEWS)).expect("mount must succeed");
    assert_eq!(
        listed,
        vec!["alpha".to_string()],
        "a valid config still serves only the default-view skill"
    );
}

/// A zero-byte `skillfs-views.toml` is the attacker-truncation shape: a
/// hand-author writes a comment or removes the file, so an existing file
/// with zero bytes must refuse the mount instead of serving every store
/// skill as the default view.
#[test]
fn zero_byte_views_config_refuses_the_mount() {
    if !common::fuse_available() {
        eprintln!("SKIP zero_byte_views_config_refuses_the_mount: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source tempdir");
    seed_three_skills(source.path(), Some(""));
    let mut store = SkillStore::new();
    let load_errors = store.load_from_directory(source.path(), &ParseConfig::default());
    assert!(load_errors.is_empty(), "fixture skills must all load");
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().expect("mount tempdir");
    let result = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        Default::default(),
    );
    let error = match result {
        Ok(_handle) => panic!("a zero-byte views config must refuse the mount"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("skillfs-views.toml"),
        "the refusal names the offending file: {error}"
    );
    assert!(
        !mountpoint.path().join("skills").exists(),
        "the refused mount must not leave a live FUSE session behind"
    );
}

/// A dangling `skillfs-views.toml` symlink is a PRESENT directory entry
/// whose read fails NotFound; it must refuse rather than read as absent
/// (which would mount with no views).
#[cfg(unix)]
#[test]
fn dangling_symlink_views_config_refuses_the_mount() {
    if !common::fuse_available() {
        eprintln!("SKIP dangling_symlink_views_config_refuses_the_mount: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source tempdir");
    seed_three_skills(source.path(), None);
    std::os::unix::fs::symlink(
        source.path().join("elsewhere.toml"),
        source.path().join("skillfs-views.toml"),
    )
    .expect("plant dangling symlink");
    let mut store = SkillStore::new();
    let load_errors = store.load_from_directory(source.path(), &ParseConfig::default());
    assert!(load_errors.is_empty(), "fixture skills must all load");
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().expect("mount tempdir");
    let result = mount_background_configured(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
        Default::default(),
    );
    let error = match result {
        Ok(_handle) => panic!("a dangling symlink views config must refuse the mount"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("skillfs-views.toml"),
        "the refusal names the offending file: {error}"
    );
}

/// The still-exported legacy background wrappers must surface the
/// refusal synchronously (`Err`), not swallow it into a logged spawn
/// failure behind `Ok(MountHandle)`. No FUSE is required: the preflight
/// refuses before any mount attempt.
#[test]
#[allow(deprecated)]
fn legacy_background_entry_points_return_the_views_refusal() {
    let source = tempfile::tempdir().expect("source tempdir");
    seed_three_skills(source.path(), Some(CORRUPT_VIEWS));
    let mut store = SkillStore::new();
    let load_errors = store.load_from_directory(source.path(), &ParseConfig::default());
    assert!(load_errors.is_empty(), "fixture skills must all load");
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    let mountpoint = tempfile::tempdir().expect("mount tempdir");
    let result = skillfs_fuse::mount_background(
        mountpoint.path(),
        source.path(),
        shared,
        MountOptions::default(),
        false,
    );
    let error = match result {
        Ok(_handle) => panic!("the legacy background mount must return the views refusal"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("skillfs-views.toml"),
        "the refusal names the offending file: {error}"
    );
    assert!(
        !mountpoint.path().join("skills").exists(),
        "no FUSE session may come up for the refused legacy mount"
    );
}

/// Mount-level companion to the fs.rs snapshot unit test: the reviewer's
/// synchronized regression, driven through the real entry point.
///
/// The unit test hands a pre-parsed snapshot straight to the constructor;
/// this one reproduces the actual race end to end. The views "file" is a
/// FIFO, so every reader open blocks until a writer session is offered
/// and the readers consume sessions strictly in order (the synchronous
/// preflight in `mount_background_configured`, then the mount-time parse
/// in `mount_inner`). Sessions 1 and 2 feed both validations the
/// restrictive config; a would-be third read — the constructor re-read
/// that existed before the snapshot carry — would consume session 3 and
/// observe truncated (zero-byte) bytes. The mount must serve exactly the
/// validated snapshot, never the no-views fallback widened to beta/gamma.
///
/// On the pre-carry head this failed deterministically: the constructor
/// consumed session 3, fell back to no views, and `/skills` listed
/// alpha, beta and gamma.
#[test]
#[cfg(target_os = "linux")]
fn views_snapshot_survives_a_post_validation_swap() {
    /// One FIFO writer session: block until the next reader opens, hand
    /// it `payload`, and report whether a reader rendezvoused within
    /// `timeout`. Sessions must be offered strictly one at a time —
    /// FIFO opens pair in arrival order, so sequential offers pin which
    /// reader sees which bytes.
    fn fifo_offer(path: &Path, payload: &[u8], timeout: Duration) -> bool {
        use std::io::Write;

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let path = path.to_path_buf();
        let payload = payload.to_vec();
        std::thread::spawn(move || {
            if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(&path) {
                let _ = file.write_all(&payload);
                let _ = done_tx.send(());
            }
        });
        done_rx.recv_timeout(timeout).is_ok()
    }

    if !common::fuse_available() {
        eprintln!("SKIP views_snapshot_survives_a_post_validation_swap: FUSE not available");
        return;
    }

    let source = tempfile::tempdir().expect("source tempdir");
    seed_three_skills(source.path(), None);
    let views_path = source.path().join("skillfs-views.toml");
    let fifo = std::process::Command::new("mkfifo")
        .arg(&views_path)
        .status()
        .expect("mkfifo");
    assert!(fifo.success(), "seed the views FIFO");

    let mut store = SkillStore::new();
    let load_errors = store.load_from_directory(source.path(), &ParseConfig::default());
    assert!(load_errors.is_empty(), "fixture skills must all load");
    let shared: SharedSkillStore = Arc::new(RwLock::new(store));

    // The preflight inside `mount_background_configured` blocks reading
    // until session 1 arrives, so the mount call runs concurrently with
    // the writer sessions.
    let mountpoint = tempfile::tempdir().expect("mount tempdir");
    let mount_mountpoint = mountpoint.path().to_path_buf();
    let mount_source = source.path().to_path_buf();
    let mount_shared = shared.clone();
    let mount = std::thread::spawn(move || {
        mount_background_configured(
            &mount_mountpoint,
            &mount_source,
            mount_shared,
            MountOptions::default(),
            false,
            Default::default(),
        )
    });

    assert!(
        fifo_offer(&views_path, VALID_VIEWS.as_bytes(), Duration::from_secs(10)),
        "the synchronous preflight must read the restrictive config"
    );
    assert!(
        fifo_offer(&views_path, VALID_VIEWS.as_bytes(), Duration::from_secs(10)),
        "the mount-time validation must read the restrictive config"
    );
    // Session 3 is consumed ONLY by a post-validation re-read of the
    // file — the TOCTOU window. It hands that reader truncated
    // (zero-byte) bytes. Once the validated snapshot is carried into the
    // filesystem no third read exists and this session simply times out.
    let _re_read_after_validation = fifo_offer(&views_path, b"", Duration::from_secs(2));

    let handle = mount
        .join()
        .expect("mount thread")
        .expect("both validations observed the restrictive config; the mount must succeed");

    std::thread::sleep(Duration::from_millis(500));
    let skills_dir = mountpoint.path().join("skills");
    if skills_dir.exists() {
        let listed = std::fs::read_dir(&skills_dir)
            .expect("readdir /skills")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "skill-discover")
            .collect::<Vec<_>>();
        assert_eq!(
            listed,
            vec!["alpha".to_string()],
            "the mount must serve exactly the validated snapshot, never the \
             no-views fallback widened to beta/gamma"
        );
    }
    drop(handle);
    unmount_quiet(mountpoint.path());
    // Bookkeeping for any session-3 writer that never rendezvoused (it
    // blocks on open; the test process exit reaps it).
    let _ = std::fs::remove_file(&views_path);
}
