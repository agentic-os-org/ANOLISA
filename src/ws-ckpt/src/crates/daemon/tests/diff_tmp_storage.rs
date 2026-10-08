//! Integration tests for the daemon-owned diff temp storage layout.
//!
//! These run in their own test process on purpose: they shim `btrfs` onto
//! the PATH, and a process-global PATH swap must not change what the lib
//! tests' real-backend calls observe (a lib test asserting that the backend
//! fails outside btrfs would otherwise see the shim succeed).

use ws_ckpt_common::backend::{SnapshotDeleteOutcome, StorageBackend};
use ws_ckpt_common::{CleanupRetention, DaemonConfig};

use ws_ckpt_daemon::backends::btrfs_base::{BtrfsBaseBackend, BtrfsBaseScenario};
use ws_ckpt_daemon::backends::btrfs_common::{cleanup_snapshots_batch, diff_against_live};

/// Mirrors `btrfs_common::DIFF_TMP_DIR_NAME` (crate-private in the lib).
const DIFF_TMP_DIR_NAME: &str = ".diff-tmp";

/// Serializes the tests in this file: each installs a PATH-shimmed `btrfs`,
/// and two concurrent shims could observe each other's variant (the
/// `send_fails` arm in particular).
static SHIM_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn shim_lock() -> tokio::sync::MutexGuard<'static, ()> {
    SHIM_LOCK.lock().await
}

/// A self-contained `btrfs` fake so the tests need no btrfs filesystem:
/// `subvolume delete <path>` removes the directory (the filesystem effect
/// the real binary has), everything else exits 0 with no output — including
/// `filesystem usage` (unparseable => fail-closed High) and
/// `inspect-internal rootid` (no id => best-effort delete), so deletions
/// ride the guarded path exactly as they would on a degraded backend. With
/// `send_fails`, `btrfs send` exits 1 to force a diff failure. PATH is
/// restored on Drop.
struct FakeBtrfs {
    _dir: tempfile::TempDir,
    saved_path: std::ffi::OsString,
}

impl FakeBtrfs {
    fn install(send_fails: bool) -> FakeBtrfs {
        let dir = tempfile::tempdir().expect("tempdir");
        let shim = dir.path().join("btrfs");
        let send_arm = if send_fails {
            "if [ \"$1\" = 'send' ]; then\n  exit 1\nfi\n"
        } else {
            ""
        };
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\n{}if [ \"$1\" = 'subvolume' ] && [ \"$2\" = 'delete' ]; then\n  rm -rf -- \"$3\"\nfi\nexit 0\n",
                send_arm
            ),
        )
        .expect("write shim");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("chmod shim");
        let saved_path = std::env::var_os("PATH").unwrap_or_default();
        std::env::set_var(
            "PATH",
            format!(
                "{}:{}",
                dir.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
        FakeBtrfs {
            _dir: dir,
            saved_path,
        }
    }

    /// Variant whose `subvolume delete` fails for internal diff-temp paths
    /// (they all live under `.diff-tmp`): the sweep cannot clean them, so
    /// recover must report the paths that remain instead of only logging.
    fn install_diff_delete_fails() -> FakeBtrfs {
        let dir = tempfile::tempdir().expect("tempdir");
        let shim = dir.path().join("btrfs");
        std::fs::write(
            &shim,
            "#!/bin/sh\nif [ \"$1\" = 'subvolume' ] && [ \"$2\" = 'delete' ]; then\n  case \"$3\" in\n    *.diff-tmp*) exit 1 ;;\n    *) rm -rf -- \"$3\" ;;\n  esac\nfi\nexit 0\n",
        )
        .expect("write shim");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("chmod shim");
        let saved_path = std::env::var_os("PATH").unwrap_or_default();
        std::env::set_var(
            "PATH",
            format!(
                "{}:{}",
                dir.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
        FakeBtrfs {
            _dir: dir,
            saved_path,
        }
    }
}

impl Drop for FakeBtrfs {
    fn drop(&mut self) {
        std::env::set_var("PATH", &self.saved_path);
    }
}

fn dummy_config() -> DaemonConfig {
    DaemonConfig {
        mount_path: std::path::PathBuf::from("/tmp/unused"),
        socket_path: std::path::PathBuf::from("/tmp/unused.sock"),
        log_level: "info".to_string(),
        auto_cleanup: false,
        auto_cleanup_keep: CleanupRetention::Count(20),
        auto_cleanup_interval_secs: 86_400,
        health_check_interval_secs: 300,
        backend_type: "btrfs-base".to_string(),
        img_size: 1,
        img_max_percent: 1.0,
        min_free_bytes: 0,
        min_free_percent: 0.0,
    }
}

/// A temp snapshot now lives under `<data-root>/.diff-tmp/<ws-id>/` — never
/// inside `snapshots/` — and the diff removes it and the now-empty internal
/// directory on success, while a leftover from an interrupted prior diff is
/// swept before the new temp is created.
#[tokio::test]
async fn diff_against_live_uses_internal_dir_and_sweeps_crash_leftovers() {
    let _guard = shim_lock().await;
    let _shim = FakeBtrfs::install(false);
    let root = tempfile::tempdir().expect("root");
    let snap_base = root.path().join("snapshots").join("ws-1");
    std::fs::create_dir_all(&snap_base).expect("snap base");
    let snap_from = snap_base.join("snap-1");
    std::fs::create_dir_all(&snap_from).expect("base snapshot dir");
    let tmp_dir = root.path().join(DIFF_TMP_DIR_NAME).join("ws-1");
    let stale = tmp_dir.join("a1b2c3");
    std::fs::create_dir_all(&stale).expect("stale internal temp dir");
    let live = tempfile::tempdir().expect("live dir");

    diff_against_live(&snap_from, live.path(), &tmp_dir, root.path())
        .await
        .expect("diff must succeed against the fake backend");

    assert!(
        !stale.exists(),
        "crash leftover must be swept by the next diff"
    );
    assert!(!tmp_dir.exists(), "the internal dir is removed once empty");
    assert!(snap_from.exists(), "real snapshots must not be touched");
    let snapshot_entries: Vec<std::ffi::OsString> = std::fs::read_dir(&snap_base)
        .expect("snap base readable")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(
        snapshot_entries,
        vec![std::ffi::OsString::from("snap-1")],
        "no temp snapshot may be created inside snapshots/"
    );
}

/// The cleanup runs when the diff itself fails, so a failed diff leaves no
/// temp snapshot behind for the next sweep.
#[tokio::test]
async fn diff_against_live_failure_still_cleans_the_temp_dir() {
    let _guard = shim_lock().await;
    let _shim = FakeBtrfs::install(true);
    let root = tempfile::tempdir().expect("root");
    let snap_from = root.path().join("snapshots").join("ws-1").join("snap-1");
    std::fs::create_dir_all(&snap_from).expect("base snapshot dir");
    let live = tempfile::tempdir().expect("live dir");
    let tmp_dir = root.path().join(DIFF_TMP_DIR_NAME).join("ws-1");

    let result = diff_against_live(&snap_from, live.path(), &tmp_dir, root.path()).await;
    assert!(
        result.is_err(),
        "the shim fails btrfs send, so the diff must fail"
    );
    assert!(
        !tmp_dir.exists(),
        "the internal dir is cleaned on failure too"
    );
}

/// Bootstrap sweeps crash leftovers out of the daemon-owned `.diff-tmp/`
/// root before the daemon serves any request, creates the root, and only
/// warns about legacy-named `.diff-tmp-*` snapshots in `snapshots/` (older
/// versions allowed users to create them as formal snapshots).
#[tokio::test]
async fn bootstrap_sweeps_internal_diff_tmp_and_keeps_legacy_names() {
    let _guard = shim_lock().await;
    let _shim = FakeBtrfs::install(false);
    let tmp = tempfile::tempdir().unwrap();
    let backend = BtrfsBaseBackend::new(tmp.path().to_path_buf(), BtrfsBaseScenario::InPlace);
    let data_root = tmp.path().join("ws-ckpt-data");
    let stale = data_root
        .join(DIFF_TMP_DIR_NAME)
        .join("ws-abc123")
        .join("a1b2c3");
    std::fs::create_dir_all(&stale).unwrap();
    let legacy = data_root
        .join("snapshots")
        .join("ws-abc123")
        .join(".diff-tmp-old");
    std::fs::create_dir_all(&legacy).unwrap();

    backend.bootstrap(&dummy_config()).await.unwrap();

    assert!(!stale.exists(), "crash leftover must be swept at bootstrap");
    assert!(
        data_root.join(DIFF_TMP_DIR_NAME).is_dir(),
        "the internal diff temp root exists"
    );
    assert!(
        legacy.exists(),
        "legacy-named snapshots are kept for manual handling"
    );
}

/// The sweep never follows symlinks and refuses entries that are not
/// directories: the internal directory is daemon-owned, so an unexpected
/// entry type means manual inspection, not silent deletion. The per-ws
/// directory then stays because it is not empty after the refused entries.
#[tokio::test]
async fn bootstrap_sweep_refuses_abnormal_entries() {
    let _guard = shim_lock().await;
    let _shim = FakeBtrfs::install(false);
    let tmp = tempfile::tempdir().unwrap();
    let backend = BtrfsBaseBackend::new(tmp.path().to_path_buf(), BtrfsBaseScenario::InPlace);
    let data_root = tmp.path().join("ws-ckpt-data");
    let ws_dir = data_root.join(DIFF_TMP_DIR_NAME).join("ws-abc123");
    let subvol = ws_dir.join("leftover");
    std::fs::create_dir_all(&subvol).unwrap();
    let outside = tempfile::tempdir().expect("outside dir");
    let link = ws_dir.join("link");
    std::os::unix::fs::symlink(outside.path(), &link).expect("symlink");
    let stray = ws_dir.join("stray.txt");
    std::fs::write(&stray, b"x").expect("stray file");

    backend.bootstrap(&dummy_config()).await.unwrap();

    assert!(!subvol.exists(), "directory leftovers are swept");
    assert!(link.exists(), "symlinks are refused, not followed");
    assert!(stray.exists(), "non-directory entries are refused");
    assert!(
        ws_dir.exists(),
        "the per-ws dir stays while refused entries remain"
    );
}

/// Unsafe ids fail their own batch entry while valid ids in the same batch
/// still delete: the batch contract keeps per-item outcomes, and no path may
/// be constructed from an id that is not a single component.
#[tokio::test]
async fn cleanup_snapshots_batch_refuses_unsafe_ids() {
    let _guard = shim_lock().await;
    let _shim = FakeBtrfs::install(false);
    let root = tempfile::tempdir().expect("root");
    let snap_dir = root.path().join("ws-1");
    let keep = snap_dir.join("snap-keep");
    std::fs::create_dir_all(&keep).expect("snapshot dir");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside dir");

    let ids = vec![
        "snap-keep".to_string(),
        "../outside".to_string(),
        "./.diff-tmp-backup".to_string(),
    ];
    let outcomes = cleanup_snapshots_batch(&snap_dir, root.path(), &ids).await;

    assert_eq!(outcomes.len(), 3, "one outcome per requested id, in order");
    assert_eq!(outcomes[0].0, "snap-keep");
    assert_eq!(outcomes[0].1, SnapshotDeleteOutcome::Removed);
    for (id, outcome) in &outcomes[1..] {
        match outcome {
            SnapshotDeleteOutcome::Failed(error) => {
                assert!(error.contains("single path component"), "{id}: {error}")
            }
            other => panic!("{id} must be refused, got {other:?}"),
        }
    }
    assert!(!keep.exists(), "the valid id still deletes");
    assert!(
        outside.exists(),
        "the traversal id must not touch anything outside the snapshot dir"
    );
}

/// recover surfaces the internal temp snapshots it could not delete: the
/// sweep's failure reaches the caller as leftover paths on the success
/// result, not only as daemon logs (#6198 review point 5). The per-ws
/// directory itself is also reported when it cannot be emptied, which on
/// real btrfs happens whenever a leftover subvolume survives the sweep
/// (a subvolume cannot be removed by the plain recursive delete that
/// follows); the shim's plain directories always can, so that branch is
/// production defense rather than something this test can observe.
#[tokio::test]
async fn recover_reports_temp_snapshots_the_sweep_failed_to_delete() {
    let _guard = shim_lock().await;
    let _shim = FakeBtrfs::install_diff_delete_fails();
    let tmp = tempfile::tempdir().unwrap();
    let backend = BtrfsBaseBackend::new(tmp.path().to_path_buf(), BtrfsBaseScenario::InPlace);
    let data_root = tmp.path().join("ws-ckpt-data");
    let subvol = data_root.join("ws-1");
    std::fs::create_dir_all(&subvol).unwrap();
    std::fs::write(subvol.join("payload"), b"payload").unwrap();
    std::fs::create_dir_all(data_root.join("snapshots").join("ws-1")).unwrap();
    let original = tmp.path().join("original");
    std::os::unix::fs::symlink(&subvol, &original).unwrap();
    let ws_tmp_dir = data_root.join(DIFF_TMP_DIR_NAME).join("ws-1");
    let stale = ws_tmp_dir.join("a1b2c3");
    std::fs::create_dir_all(&stale).unwrap();

    let leftovers = backend
        .recover_workspace("ws-1", original.to_str().unwrap())
        .await
        .expect("recover must succeed");

    assert!(
        leftovers.contains(&stale.display().to_string()),
        "the undeletable temp snapshot must be reported, got {leftovers:?}"
    );
    assert!(original.is_dir(), "workspace contents were restored");
    assert!(
        original.join("payload").is_file(),
        "restored contents came from the subvolume"
    );
}

/// recover must not report paths it actually deleted: when the internal
/// temp directory contains a plain file, the sweep refuses to touch it and
/// reports it, so the teardown afterwards must keep it too — every leftover
/// the response lists has to still be on disk, and the per-ws directory
/// itself is reported when it survives because it is not empty (the same
/// remove_dir-only contract the bootstrap sweep and diff path follow).
#[tokio::test]
async fn recover_keeps_the_leftovers_it_reports() {
    let _guard = shim_lock().await;
    let _shim = FakeBtrfs::install(false);
    let tmp = tempfile::tempdir().unwrap();
    let backend = BtrfsBaseBackend::new(tmp.path().to_path_buf(), BtrfsBaseScenario::InPlace);
    let data_root = tmp.path().join("ws-ckpt-data");
    let subvol = data_root.join("ws-1");
    std::fs::create_dir_all(&subvol).unwrap();
    std::fs::write(subvol.join("payload"), b"payload").unwrap();
    std::fs::create_dir_all(data_root.join("snapshots").join("ws-1")).unwrap();
    let original = tmp.path().join("original");
    std::os::unix::fs::symlink(&subvol, &original).unwrap();
    let ws_tmp_dir = data_root.join(DIFF_TMP_DIR_NAME).join("ws-1");
    std::fs::create_dir_all(&ws_tmp_dir).unwrap();
    let stale = ws_tmp_dir.join("a1b2c3");
    std::fs::create_dir_all(&stale).unwrap();
    let stray = ws_tmp_dir.join("stray.bin");
    std::fs::write(&stray, b"stray").unwrap();

    let leftovers = backend
        .recover_workspace("ws-1", original.to_str().unwrap())
        .await
        .expect("recover must succeed");

    assert!(
        leftovers.contains(&stray.display().to_string()),
        "the refused file must be reported, got {leftovers:?}"
    );
    assert!(
        leftovers.contains(&ws_tmp_dir.display().to_string()),
        "the per-ws temp dir that survives must be reported, got {leftovers:?}"
    );
    for path in &leftovers {
        assert!(
            std::path::Path::new(path).exists(),
            "reported leftover {path} must still exist on disk"
        );
    }
    assert!(
        stray.is_file(),
        "the sweep's refusal must survive the directory teardown"
    );
    assert_eq!(
        std::fs::read(&stray).unwrap(),
        b"stray".as_slice(),
        "the refused file must be untouched"
    );
    assert!(
        original.join("payload").is_file(),
        "workspace contents were restored"
    );
}
