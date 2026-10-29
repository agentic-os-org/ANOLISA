//! Phase 6.3: snapshot create / list / restore round-trip.

use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::error::MemoryError;
use agent_memory::service::MemoryService;

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "snap-tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

#[test]
fn snapshot_create_writes_archive_and_sidecar() {
    let (_tmp, svc) = setup();
    svc.write("notes/a.md", "alpha", false).unwrap();
    svc.write("notes/b.md", "beta", false).unwrap();

    let info = svc.mem_snapshot(Some("baseline")).unwrap();
    assert!(info.id.starts_with("snap_"));
    assert_eq!(info.name, "baseline");
    assert_eq!(info.backend, "tar.gz");
    assert!(info.size > 0);

    let snap_dir = svc.mount.meta_dir.join("snapshots");
    assert!(snap_dir.join(format!("{}.tar.gz", info.id)).exists());
    assert!(snap_dir.join(format!("{}.json", info.id)).exists());
}

#[test]
fn snapshot_list_orders_oldest_first() {
    let (_tmp, svc) = setup();
    let a = svc.mem_snapshot(Some("first")).unwrap();
    // Force a different timestamp; ULID monotonic suffices but be safe.
    std::thread::sleep(std::time::Duration::from_millis(10));
    let b = svc.mem_snapshot(Some("second")).unwrap();

    let list = svc.mem_snapshot_list().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, a.id);
    assert_eq!(list[1].id, b.id);
}

#[test]
fn snapshot_restore_round_trips_files() {
    let (_tmp, svc) = setup();
    svc.write("doc.md", "v1 contents", false).unwrap();
    let snap = svc.mem_snapshot(None).unwrap();

    // Mutate after snapshot.
    svc.write("doc.md", "v2 contents OVERWRITTEN", true)
        .unwrap();
    svc.write("scratch/draft.md", "throwaway", false).unwrap();
    assert_eq!(svc.read("doc.md").unwrap(), "v2 contents OVERWRITTEN");

    svc.mem_snapshot_restore(&snap.id).unwrap();

    // Original file is back; post-snapshot files are gone.
    assert_eq!(svc.read("doc.md").unwrap(), "v1 contents");
    assert!(matches!(
        svc.read("scratch/draft.md"),
        Err(MemoryError::NotFound(_))
    ));
}

#[test]
fn restore_unknown_id_returns_not_found() {
    let (_tmp, svc) = setup();
    let err = svc.mem_snapshot_restore("snap_does_not_exist").unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));
}

#[test]
fn restore_rejects_invalid_gzip_trailer_without_changing_live_files() {
    for truncate_trailer in [false, true] {
        let (_tmp, svc) = setup();
        svc.write("doc.md", "snapshot contents", false).unwrap();
        let snap = svc.mem_snapshot(None).unwrap();
        let archive_path = svc
            .mount
            .meta_dir
            .join("snapshots")
            .join(format!("{}.tar.gz", snap.id));
        let mut archive = std::fs::read(&archive_path).unwrap();
        if truncate_trailer {
            archive.truncate(archive.len() - 8);
        } else {
            let crc_offset = archive.len() - 8;
            archive[crc_offset] ^= 1;
        }
        std::fs::write(&archive_path, archive).unwrap();

        svc.write("doc.md", "live contents", true).unwrap();
        svc.write("new.md", "keep this file", false).unwrap();

        let result = svc.mem_snapshot_restore(&snap.id);
        assert!(
            result.is_err(),
            "restore must reject an invalid gzip trailer (truncated={truncate_trailer})"
        );
        assert_eq!(svc.read("doc.md").unwrap(), "live contents");
        assert_eq!(svc.read("new.md").unwrap(), "keep this file");
    }
}

#[test]
fn restore_rejects_corrupted_gzip_payload_without_changing_live_files() {
    use std::io::{Read, Write};

    let (_tmp, svc) = setup();
    svc.write("doc.md", "account balance: 100", false).unwrap();
    let snap = svc.mem_snapshot(None).unwrap();
    let archive_path = svc
        .mount
        .meta_dir
        .join("snapshots")
        .join(format!("{}.tar.gz", snap.id));
    let mut tar_bytes = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(&archive_path).unwrap())
        .read_to_end(&mut tar_bytes)
        .unwrap();

    // Stored DEFLATE blocks let us damage a payload byte without breaking
    // the stream structure; the gzip checksum must catch the corruption.
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::none());
    encoder.write_all(&tar_bytes).unwrap();
    let mut archive = encoder.finish().unwrap();
    let marker = b"account balance: 100";
    let offset = archive
        .windows(marker.len())
        .position(|w| w == marker)
        .unwrap();
    archive[offset + marker.len() - 3] = b'9';
    std::fs::write(&archive_path, archive).unwrap();

    svc.write("doc.md", "live contents", true).unwrap();
    svc.write("new.md", "keep this file", false).unwrap();
    let result = svc.mem_snapshot_restore(&snap.id);
    assert!(
        result.is_err(),
        "corrupt restore returned {result:?}; current doc.md is {:?}",
        svc.read("doc.md")
    );
    assert_eq!(svc.read("doc.md").unwrap(), "live contents");
    assert_eq!(svc.read("new.md").unwrap(), "keep this file");
}

#[test]
fn restore_preserves_meta_dir_and_leaves_no_rollback_artefacts() {
    // Regression for B3: the old `delete all + move in` flow left the
    // mount empty for the duration of the move, and a crash there would
    // wipe user data. The new flow renames each top-level entry aside
    // under `.anolisa/.<id>.rollback.*` and drops them only after the
    // staging swap completes. This test verifies the happy path leaves
    // no rollback leftovers under .anolisa/.
    let (_tmp, svc) = setup();
    svc.write("doc.md", "v1", false).unwrap();
    svc.write("notes/inner.md", "deep", false).unwrap();
    // Place a marker inside .anolisa/ — restore must preserve it.
    let marker = svc.mount.meta_dir.join("audit.log");
    let marker_before = std::fs::read_to_string(&marker).unwrap_or_default();

    let snap = svc.mem_snapshot(None).unwrap();
    svc.write("doc.md", "v2", true).unwrap();
    svc.mem_snapshot_restore(&snap.id).unwrap();

    assert_eq!(svc.read("doc.md").unwrap(), "v1");
    // .anolisa/audit.log must still exist (meta dir untouched).
    let marker_after = std::fs::read_to_string(&marker).unwrap_or_default();
    assert!(
        marker_after.len() >= marker_before.len(),
        "audit.log shrank across restore"
    );

    // No `.<id>.rollback.*` leftovers under .anolisa/.
    let prefix = format!(".{}.rollback.", snap.id);
    let leftovers: Vec<_> = std::fs::read_dir(&svc.mount.meta_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .collect();
    assert!(
        leftovers.is_empty(),
        "rollback artefacts not cleaned up: {:?}",
        leftovers.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );
}

#[test]
fn snapshot_excludes_meta_directory() {
    let (_tmp, svc) = setup();
    svc.write("note.md", "real", false).unwrap();
    let info = svc.mem_snapshot(None).unwrap();

    // Read the archive raw and verify .anolisa/ is not in it.
    let archive_path = svc
        .mount
        .meta_dir
        .join("snapshots")
        .join(format!("{}.tar.gz", info.id));
    let bytes = std::fs::read(&archive_path).unwrap();
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut tar = ::tar::Archive::new(gz);
    let entries: Vec<String> = tar
        .entries()
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().into_owned()))
        .collect();

    for path in &entries {
        assert!(
            !path.starts_with(".anolisa"),
            "snapshot leaked meta path: {path}"
        );
    }
}

#[test]
fn snapshot_restores_nested_readonly_directories() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, svc) = setup();
    svc.write("reference/nested/guide.md", "saved reference", false)
        .unwrap();
    svc.mkdir("reference/empty").unwrap();
    for (dir, mode) in [
        ("reference/nested", 0o500),
        ("reference/empty", 0o555),
        ("reference", 0o555),
    ] {
        std::fs::set_permissions(
            svc.mount.root.join(dir),
            std::fs::Permissions::from_mode(mode),
        )
        .unwrap();
    }
    let snapshot = svc.mem_snapshot(Some("read-only references")).unwrap();
    std::fs::set_permissions(
        svc.mount.root.join("reference"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::set_permissions(
        svc.mount.root.join("reference/nested"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    svc.write("reference/nested/guide.md", "new contents", true)
        .unwrap();
    let first = svc.mem_snapshot_restore(&snapshot.id);
    // A second snapshot avoids the independently tracked same-id trash
    // collision while exercising an already read-only live directory.
    let result = first.and_then(|()| {
        let second = svc.mem_snapshot(None)?;
        svc.mem_snapshot_restore(&second.id)
    });
    let modes: Vec<_> = ["reference", "reference/nested", "reference/empty"]
        .into_iter()
        .map(|p| {
            std::fs::metadata(svc.mount.root.join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        })
        .collect();
    let content = svc.read("reference/nested/guide.md");
    // Restore write permission throughout this private fixture before checking
    // the result, so failed extractions and read-only trash can be removed.
    for entry in walkdir::WalkDir::new(_tmp.path())
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.file_type().is_dir() {
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    assert!(result.is_ok(), "restore failed: {result:?}");
    assert_eq!(content.unwrap(), "saved reference");
    assert_eq!(modes, vec![0o555, 0o500, 0o555]);
}

#[test]
fn restore_removes_readonly_staging_left_by_an_interrupted_attempt() {
    use std::os::unix::fs::PermissionsExt;
    for mode in [0o500, 0o444, 0o644] {
        let (_tmp, svc) = setup();
        svc.write("doc.md", "saved", false).unwrap();
        let snapshot = svc.mem_snapshot(None).unwrap();
        svc.write("doc.md", "live", true).unwrap();
        let staging = svc
            .mount
            .meta_dir
            .join("snapshots")
            .join(format!(".{}.staging", snapshot.id));
        let nested = staging.join("reference/nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("partial.md"), "interrupted").unwrap();
        for path in [&nested, &staging.join("reference")] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let result = svc.mem_snapshot_restore(&snapshot.id);
        // Make a failing fixture removable before asserting, too.
        if staging.exists() {
            for path in [&staging.join("reference"), &nested] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        result.unwrap_or_else(|error| panic!("staging mode {mode:o}: {error}"));
        assert_eq!(svc.read("doc.md").unwrap(), "saved");
        assert!(!staging.exists());
    }
}

#[test]
fn failed_restore_cleans_readonly_staging_and_can_be_retried() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, svc) = setup();
    svc.write("reference/doc.md", "saved", false).unwrap();
    let reference = svc.mount.root.join("reference");
    std::fs::set_permissions(&reference, std::fs::Permissions::from_mode(0o555)).unwrap();
    let snapshot = svc.mem_snapshot(None).unwrap();
    std::fs::set_permissions(&reference, std::fs::Permissions::from_mode(0o755)).unwrap();
    svc.write("reference/doc.md", "live", true).unwrap();
    // A nonempty owned fixture blocks the first swap without changing live data.
    let obstacle = svc
        .mount
        .meta_dir
        .join(format!(".{}.rollback.reference", snapshot.id));
    std::fs::create_dir(&obstacle).unwrap();
    std::fs::write(obstacle.join("occupied"), "keep").unwrap();
    assert!(svc.mem_snapshot_restore(&snapshot.id).is_err());
    assert_eq!(svc.read("reference/doc.md").unwrap(), "live");
    let staging = svc
        .mount
        .meta_dir
        .join("snapshots")
        .join(format!(".{}.staging", snapshot.id));
    assert!(
        !staging.exists(),
        "failed restore left read-only staging behind"
    );
    std::fs::remove_dir_all(obstacle).unwrap();
    svc.mem_snapshot_restore(&snapshot.id).unwrap();
    let mode = std::fs::metadata(&reference).unwrap().permissions().mode() & 0o777;
    std::fs::set_permissions(&reference, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(svc.read("reference/doc.md").unwrap(), "saved");
    assert_eq!(mode, 0o555);
}

#[test]
fn corrupt_gzip_cleans_readonly_staging_before_retry() {
    use std::os::unix::fs::PermissionsExt;

    let (_tmp, svc) = setup();
    svc.write("reference/doc.md", "saved", false).unwrap();
    let reference = svc.mount.root.join("reference");
    std::fs::set_permissions(&reference, std::fs::Permissions::from_mode(0o555)).unwrap();
    let snapshot = svc.mem_snapshot(None).unwrap();
    std::fs::set_permissions(&reference, std::fs::Permissions::from_mode(0o755)).unwrap();
    svc.write("reference/doc.md", "live", true).unwrap();
    svc.write("new.md", "keep until successful restore", false)
        .unwrap();

    let snapshots = svc.mount.meta_dir.join("snapshots");
    let archive_path = snapshots.join(format!("{}.tar.gz", snapshot.id));
    let staging = snapshots.join(format!(".{}.staging", snapshot.id));
    let valid_archive = std::fs::read(&archive_path).unwrap();
    let mut damaged_archive = valid_archive.clone();
    let crc_offset = damaged_archive.len() - 8;
    damaged_archive[crc_offset] ^= 1;
    std::fs::write(&archive_path, damaged_archive).unwrap();

    let result = svc.mem_snapshot_restore(&snapshot.id);
    let staging_remained = staging.exists();
    // A broken cleanup must not leave an unremovable test fixture behind.
    for entry in walkdir::WalkDir::new(svc.mount.root.as_path())
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.file_type().is_dir() {
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let error = result.unwrap_err();
    assert!(
        error.to_string().contains("snapshot gzip validation"),
        "{error}"
    );
    assert!(
        !staging_remained,
        "gzip failure left read-only staging behind"
    );
    assert_eq!(svc.read("reference/doc.md").unwrap(), "live");
    assert_eq!(svc.read("new.md").unwrap(), "keep until successful restore");

    std::fs::write(&archive_path, valid_archive).unwrap();
    let result = svc.mem_snapshot_restore(&snapshot.id);
    let mode = std::fs::metadata(&reference).unwrap().permissions().mode() & 0o777;
    std::fs::set_permissions(&reference, std::fs::Permissions::from_mode(0o755)).unwrap();
    result.unwrap();
    assert_eq!(svc.read("reference/doc.md").unwrap(), "saved");
    assert_eq!(mode, 0o555);
    assert!(matches!(svc.read("new.md"), Err(MemoryError::NotFound(_))));
    assert!(!staging.exists());
}

#[test]
fn skipped_archive_directory_rejects_restore_without_replacing_live_content() {
    let (_tmp, svc) = setup();
    svc.write("doc.md", "live", false).unwrap();
    let snapshot = svc.mem_snapshot(None).unwrap();
    let archive = svc
        .mount
        .meta_dir
        .join("snapshots")
        .join(format!("{}.tar.gz", snapshot.id));
    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(archive).unwrap(),
        flate2::Compression::default(),
    );
    let mut tar = tar::Builder::new(gz);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_mode(0o755);
    header.set_size(0);
    // set_path rejects parent traversal, so construct the malformed archive
    // header directly. tar::Entry::unpack_in returns false for this entry.
    let path = b"discard/../reference";
    header.as_mut_bytes()[..path.len()].copy_from_slice(path);
    header.set_cksum();
    tar.append(&header, std::io::empty()).unwrap();
    tar.into_inner().unwrap().finish().unwrap();
    let error = svc.mem_snapshot_restore(&snapshot.id).unwrap_err();
    assert!(error.to_string().contains("skipped"), "{error}");
    assert_eq!(svc.read("doc.md").unwrap(), "live");
}

// ---- artefact reclaim ----

use std::time::Duration;

fn age_file(path: &std::path::Path, secs_ago: i64) {
    // utimensat takes an absolute epoch timestamp; compute "now minus n"
    // so the caller's "N seconds ago" is relative to the test run, not 1970.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mtime = nix::sys::time::TimeSpec::new(now - secs_ago, 0);
    nix::sys::stat::utimensat(
        None,
        path,
        &mtime,
        &mtime,
        nix::sys::stat::UtimensatFlags::NoFollowSymlink,
    )
    .unwrap();
}

fn sidecar_info(id: &str) -> agent_memory::snapshot::SnapshotInfo {
    agent_memory::snapshot::SnapshotInfo {
        id: id.to_string(),
        name: id.to_string(),
        created_at: "2026-01-01T00:00:00+00:00".into(),
        size: 1,
        backend: "tar.gz".into(),
    }
}

#[test]
fn reclaim_removes_stale_partial_and_keeps_young() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let old = dir.join("snap_OLD.tar.gz.partial");
    let young = dir.join("snap_NEW.tar.gz.partial");
    std::fs::write(&old, b"junk").unwrap();
    std::fs::write(&young, b"junk").unwrap();
    age_file(&old, 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        agent_memory::snapshot::reclaim::DEFAULT_MIN_ARTIFACT_AGE,
    );
    assert_eq!(report.partials_removed, 1);
    assert_eq!(report.kept_young, 1);
    assert!(!old.exists());
    assert!(young.exists());
}

#[test]
fn reclaim_removes_sidecar_less_archive_but_not_complete_pair() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    // Complete pair: archive + parseable sidecar — must survive.
    let info = sidecar_info("snap_KEEP");
    std::fs::write(dir.join("snap_KEEP.tar.gz"), b"archive").unwrap();
    std::fs::write(
        dir.join("snap_KEEP.json"),
        serde_json::to_string(&info).unwrap(),
    )
    .unwrap();

    // Sidecar-less archive, old — removed.
    std::fs::write(dir.join("snap_LOST.tar.gz"), b"archive").unwrap();
    age_file(&dir.join("snap_LOST.tar.gz"), 2 * 24 * 3600);

    // Sidecar-less archive, young — kept.
    std::fs::write(dir.join("snap_INFLIGHT.tar.gz"), b"archive").unwrap();

    // Orphan sidecar (no archive), old — removed.
    std::fs::write(
        dir.join("snap_GONE.json"),
        serde_json::to_string(&sidecar_info("snap_GONE")).unwrap(),
    )
    .unwrap();
    age_file(&dir.join("snap_GONE.json"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.archives_removed, 1);
    assert_eq!(report.sidecars_removed, 1);
    assert_eq!(report.kept_young, 1); // snap_INFLIGHT
    assert!(dir.join("snap_KEEP.tar.gz").exists());
    assert!(dir.join("snap_KEEP.json").exists());
    assert!(!dir.join("snap_LOST.tar.gz").exists());
    assert!(dir.join("snap_INFLIGHT.tar.gz").exists());
    assert!(!dir.join("snap_GONE.json").exists());
}

#[test]
fn reclaim_sweep_at_mount_open_does_not_block_or_delete_live_data() {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "reclaim-open-tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    // Seed the snapshots dir before the first open: an old orphan partial,
    // an old orphan sidecar, and a complete pair. Default config has reclaim
    // enabled, so opening the service must sweep the orphans while leaving
    // the complete pair — and all user files — untouched.
    let dir = tmp
        .path()
        .join("user-reclaim-open-tester/.anolisa/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("snap_ORPH.tar.gz.partial"), b"junk").unwrap();
    age_file(&dir.join("snap_ORPH.tar.gz.partial"), 2 * 24 * 3600);
    std::fs::write(dir.join("snap_GONE.json"), b"{}").unwrap();
    age_file(&dir.join("snap_GONE.json"), 2 * 24 * 3600);
    std::fs::write(dir.join("snap_KEEP.tar.gz"), b"archive").unwrap();
    std::fs::write(
        dir.join("snap_KEEP.json"),
        serde_json::to_string(&sidecar_info("snap_KEEP")).unwrap(),
    )
    .unwrap();

    let svc = MemoryService::new(cfg).unwrap();
    svc.write("note.md", "survives", false).unwrap();
    assert_eq!(svc.read("note.md").unwrap(), "survives");
    assert!(!dir.join("snap_ORPH.tar.gz.partial").exists());
    assert!(!dir.join("snap_GONE.json").exists());
    assert!(dir.join("snap_KEEP.tar.gz").exists());
    assert!(dir.join("snap_KEEP.json").exists());
}

#[test]
fn reclaim_disabled_config_leaves_artifacts() {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "reclaim-off-tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.snapshot_reclaim.enabled = false;
    let svc = MemoryService::new(cfg).unwrap();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let old = dir.join("snap_X.tar.gz.partial");
    std::fs::write(&old, b"junk").unwrap();
    age_file(&old, 2 * 24 * 3600);

    // Re-open: sweep disabled, artefact survives.
    drop(svc);
    let mut cfg2 = AppConfig::default();
    cfg2.global.user_id = "reclaim-off-tester".into();
    cfg2.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg2.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg2.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg2.memory.snapshot_reclaim.enabled = false;
    let svc2 = MemoryService::new(cfg2).unwrap();
    assert!(
        svc2.mount
            .meta_dir
            .join("snapshots")
            .join("snap_X.tar.gz.partial")
            .exists()
    );
}

#[test]
fn reclaim_repairs_truncated_sidecar_from_intact_archive() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    // Intact archive + truncated sidecar (simulating a crash mid
    // write_sidecar): the archive is a valid gzip+tar snapshot, so the
    // sweep must repair the sidecar rather than remove either file.
    write_valid_archive(&dir, "snap_FIX", b"archive contents");
    std::fs::write(dir.join("snap_FIX.json"), b"{\"id\": \"snap_").unwrap();
    age_file(&dir.join("snap_FIX.tar.gz"), 2 * 24 * 3600);
    age_file(&dir.join("snap_FIX.json"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.sidecars_repaired, 1);
    assert_eq!(report.archives_removed, 0);
    assert!(dir.join("snap_FIX.tar.gz").exists());

    // The repaired sidecar must parse and round-trip through list/restore.
    let list = svc.mem_snapshot_list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "snap_FIX");
    assert_eq!(list[0].name, "snap_FIX");
    assert_eq!(list[0].backend, "tar.gz");
    assert!(list[0].size > 0);
}

#[test]
fn reclaim_keeps_truncated_sidecar_pair_below_age_guard() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("snap_YOUNG.tar.gz"), b"archive-bytes").unwrap();
    std::fs::write(dir.join("snap_YOUNG.json"), b"{\"id\": \"snap_").unwrap();

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.sidecars_repaired, 0);
    assert_eq!(report.kept_young, 1);
    assert!(dir.join("snap_YOUNG.tar.gz").exists());
    assert!(dir.join("snap_YOUNG.json").exists());
}

#[test]
fn reclaim_just_past_the_age_guard_is_reclaimed_and_just_below_is_kept() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    // `older_than` removes when age >= min_age. Age the two artefacts a
    // minute apart around the guard so both outcomes are stable regardless
    // of the milliseconds the test spends between aging and sweeping.
    let over = dir.join("snap_OVER.tar.gz.partial");
    let under = dir.join("snap_UNDER.tar.gz.partial");
    std::fs::write(&over, b"junk").unwrap();
    std::fs::write(&under, b"junk").unwrap();
    age_file(&over, 24 * 3600 + 60);
    age_file(&under, 24 * 3600 - 60);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.partials_removed, 1);
    assert_eq!(report.kept_young, 1);
    assert!(!over.exists());
    assert!(under.exists());
}

#[test]
fn config_validate_rejects_sub_minute_reclaim_age() {
    let mut cfg = AppConfig::default();
    cfg.memory.snapshot_reclaim.min_artifact_age_secs = 0;
    assert!(
        cfg.validate().is_err(),
        "age guard of 0 must be rejected: it is the only protection an \
         in-flight snapshot in another process has"
    );
    cfg.memory.snapshot_reclaim.min_artifact_age_secs = 59;
    assert!(cfg.validate().is_err());
    cfg.memory.snapshot_reclaim.min_artifact_age_secs = 60;
    assert!(cfg.validate().is_ok());
}

#[test]
fn reclaim_removes_stale_staging_dir_and_keeps_young_one() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    // A stale staging dir with content (as a crashed restore leaves it),
    // a young staging dir (a restore in flight in another process), and an
    // unrelated directory that is not a snapshot artefact at all.
    let stale = dir.join(".snap_DEAD.staging");
    let young = dir.join(".snap_LIVE.staging");
    let unrelated = dir.join("not-a-staging-dir");
    std::fs::create_dir_all(stale.join("notes/inner")).unwrap();
    std::fs::write(stale.join("notes/inner/deep.md"), b"extracted").unwrap();
    std::fs::create_dir_all(&young).unwrap();
    std::fs::create_dir_all(&unrelated).unwrap();
    std::fs::write(unrelated.join("keep.md"), b"stays").unwrap();
    age_file(&stale, 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.staging_dirs_removed, 1);
    assert_eq!(report.kept_young, 1); // the young staging dir
    assert!(!stale.exists());
    assert!(young.exists());
    assert!(unrelated.join("keep.md").exists());
}

#[test]
fn reclaim_staging_dir_around_the_age_guard() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let over = dir.join(".snap_OVER.staging");
    let under = dir.join(".snap_UNDER.staging");
    std::fs::create_dir_all(&over).unwrap();
    std::fs::create_dir_all(&under).unwrap();
    age_file(&over, 24 * 3600 + 60);
    age_file(&under, 24 * 3600 - 60);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.staging_dirs_removed, 1);
    assert_eq!(report.kept_young, 1);
    assert!(!over.exists());
    assert!(under.exists());
}

#[test]
fn reclaim_removes_readonly_staging_dirs() {
    use std::os::unix::fs::PermissionsExt;
    // Mirrors restore_tarball's own read-only staging shapes
    // (`restore_removes_readonly_staging_left_by_an_interrupted_attempt`):
    // a crashed extraction reproduces archived directory modes, so stale
    // staging can contain 0o500/0o444/0o555 directories whose files cannot
    // be unlinked without the staging-mode grant `remove_restore_staging`
    // performs. Each mode must come out removed, not stuck in
    // `removal_failures`.
    for mode in [0o500, 0o444, 0o555] {
        let (_tmp, svc) = setup();
        let dir = svc.mount.meta_dir.join("snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        let stale = dir.join(".snap_RO.staging");
        let nested = stale.join("reference/nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("partial.md"), b"extracted").unwrap();
        for path in [&nested, &stale.join("reference")] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        age_file(&stale, 2 * 24 * 3600);

        let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
            &svc.mount,
            Duration::from_secs(24 * 3600),
        );
        assert_eq!(
            report.staging_dirs_removed, 1,
            "staging mode {mode:o} must be removed"
        );
        assert_eq!(report.removal_failures, 0, "staging mode {mode:o}");
        assert!(!stale.exists());
    }
}

/// A real (gzip + tar, one regular entry) archive for repair tests: the
/// shape `create_tarball` writes, small enough to validate quickly.
fn write_valid_archive(dir: &std::path::Path, id: &str, content: &[u8]) {
    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(dir.join(format!("{id}.tar.gz"))).unwrap(),
        flate2::Compression::default(),
    );
    let mut tar = tar::Builder::new(gz);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_size(content.len() as u64);
    tar.append_data(&mut header, "notes.md", content).unwrap();
    tar.into_inner().unwrap().finish().unwrap();
}

/// A well-formed gzip+tar archive whose single entry has the given type
/// and path — the shapes `restore_tarball` refuses but a structural
/// gzip/tar check alone would accept.
fn write_archive_with_entry(dir: &std::path::Path, id: &str, ty: tar::EntryType, path: &str) {
    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(dir.join(format!("{id}.tar.gz"))).unwrap(),
        flate2::Compression::default(),
    );
    let mut tar = tar::Builder::new(gz);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(ty);
    header.set_mode(0o777);
    header.set_size(0);
    if ty == tar::EntryType::Link || ty == tar::EntryType::Symlink {
        header.set_link_name("/etc/passwd").unwrap();
    }
    // set_path rejects parent traversal, so construct the malformed entry
    // name directly — same technique as
    // skipped_archive_directory_rejects_restore_without_replacing_live_content.
    let bytes = path.as_bytes();
    header.as_mut_bytes()[..bytes.len()].copy_from_slice(bytes);
    header.set_cksum();
    tar.append(&header, std::io::empty()).unwrap();
    tar.into_inner().unwrap().finish().unwrap();
}

/// A symlinked sidecar must never be read or written through. The planted
/// link points outside the mount; an unanchored repair path would be an
/// arbitrary-file-overwrite primitive (the sweep writes attacker-chosen
/// JSON to the link target). The anchored read refuses the symlink, so
/// the archive is discarded and the link and the outside victim are
/// untouched.
#[test]
fn reclaim_does_not_follow_a_symlinked_sidecar() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim_authorized_keys");
    std::fs::write(&victim, b"ssh-ed25519 AAAAC3Nz the-real-key user@host\n").unwrap();

    write_valid_archive(&dir, "snap_EVIL", b"not really an archive");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&victim, dir.join("snap_EVIL.json")).unwrap();
    age_file(&dir.join("snap_EVIL.tar.gz"), 2 * 24 * 3600);
    age_file(&dir.join("snap_EVIL.json"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );

    // The victim must be byte-identical: no repair write ever reached it.
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"ssh-ed25519 AAAAC3Nz the-real-key user@host\n".to_vec()
    );
    // The archive is gone (its only sidecar was a link, which reads as
    // absent through the anchored probe); the link itself must survive —
    // the sweep never removes names it cannot classify as its own regular
    // artefacts.
    assert!(
        !dir.join("snap_EVIL.tar.gz").exists(),
        "an archive whose sidecar is a symlink must be treated as sidecar-less"
    );
    assert!(dir.join("snap_EVIL.json").is_symlink());
    let _ = report;
}

/// A `*.json` *directory* next to a like-named archive must not reach the
/// repair path (a plain read would fail with EISDIR and a plain write
/// would fail too); the archive is sidecar-less from the anchored view.
#[test]
fn reclaim_treats_a_directory_named_like_a_sidecar_as_absent() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(dir.join("snap_D.json")).unwrap();
    write_valid_archive(&dir, "snap_D", b"payload");
    age_file(&dir.join("snap_D.tar.gz"), 2 * 24 * 3600);
    age_file(&dir.join("snap_D.json"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.sidecars_repaired, 0);
    assert_eq!(report.removal_failures, 0);
    assert!(!dir.join("snap_D.tar.gz").exists());
    assert!(dir.join("snap_D.json").is_dir());
}

/// A corrupt archive with a truncated sidecar must not be "repaired" into
/// an advertised, unrestorable snapshot — that entry would be listed
/// forever, fail every restore, and be reclaimed by no rule. The
/// validator rejects it, so the archive is removed and the entry never
/// appears in the list; the truncated sidecar is then orphaned and
/// removed too.
#[test]
fn reclaim_deletes_a_corrupt_archive_instead_of_fabricating_a_sidecar() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(dir.join("snap_JUNK.tar.gz"), vec![0u8; 4096]).unwrap();
    std::fs::write(dir.join("snap_JUNK.json"), b"{\"id\": \"snap_JU").unwrap();
    age_file(&dir.join("snap_JUNK.tar.gz"), 2 * 24 * 3600);
    age_file(&dir.join("snap_JUNK.json"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.sidecars_repaired, 0);
    assert!(!dir.join("snap_JUNK.tar.gz").exists());
    // The sweep's orphan-sidecar pass runs after the archive pass, so the
    // truncated sidecar is already orphaned within this same sweep and is
    // removed with it.
    assert!(!dir.join("snap_JUNK.json").exists());

    // A second sweep is a no-op — no immortal, advertised, unrestorable
    // entry, and nothing left to trip later.
    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.total_removed() + report.sidecars_repaired, 0);
    assert!(svc.mem_snapshot_list().unwrap().is_empty());
}

/// A valid archive with a truncated sidecar is still repaired end-to-end:
/// validation passes, the fabricated sidecar parses, restore succeeds.
#[test]
fn reclaim_repairs_only_when_the_archive_is_truly_restorable() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    write_valid_archive(&dir, "snap_OK", b"precious backup");
    std::fs::write(dir.join("snap_OK.json"), b"{\"id\": \"snap_").unwrap();
    age_file(&dir.join("snap_OK.tar.gz"), 2 * 24 * 3600);
    age_file(&dir.join("snap_OK.json"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.sidecars_repaired, 1);
    assert_eq!(report.removal_failures, 0);

    // The repaired sidecar must survive restore, not just list.
    let list = svc.mem_snapshot_list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "snap_OK");
    svc.write("live.md", "live", false).unwrap();
    svc.mem_snapshot_restore("snap_OK").unwrap();
    assert_eq!(svc.read("notes.md").unwrap(), "precious backup");
}

/// A well-formed gzip+tar archive whose entries `restore_tarball` refuses
/// (symlink/hardlink types, `..` traversal paths) is structurally valid
/// but unrestorable. Repairing its truncated sidecar would advertise a
/// snapshot that lists forever and fails every restore, so the validator
/// must reject it: the archive is deleted, the sidecar is orphaned and
/// removed in the same sweep, and nothing is advertised.
#[test]
fn reclaim_deletes_a_structurally_valid_but_unrestorable_archive() {
    for (label, ty, path) in [
        ("symlink", tar::EntryType::Symlink, "notes.md"),
        ("hardlink", tar::EntryType::Link, "notes.md"),
        ("traversal", tar::EntryType::Regular, "../escape"),
    ] {
        let (_tmp, svc) = setup();
        let dir = svc.mount.meta_dir.join("snapshots");
        std::fs::create_dir_all(&dir).unwrap();

        write_archive_with_entry(&dir, "snap_LNK", ty, path);
        std::fs::write(dir.join("snap_LNK.json"), b"{\"id\": \"snap_L").unwrap();
        age_file(&dir.join("snap_LNK.tar.gz"), 2 * 24 * 3600);
        age_file(&dir.join("snap_LNK.json"), 2 * 24 * 3600);

        let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
            &svc.mount,
            Duration::from_secs(24 * 3600),
        );
        assert_eq!(
            report.sidecars_repaired, 0,
            "{label}: an unrestorable archive must not be repaired"
        );
        assert!(
            !dir.join("snap_LNK.tar.gz").exists(),
            "{label}: the archive must be deleted, not advertised"
        );
        assert!(
            !dir.join("snap_LNK.json").exists(),
            "{label}: the orphaned sidecar must go in the same sweep"
        );
        assert!(
            svc.mem_snapshot_list().unwrap().is_empty(),
            "{label}: nothing may be advertised"
        );
    }
}

/// An archive whose sidecar is a symlink is tampering, not a crash
/// leftover: the anchored probe reports the link as a path violation, so
/// the discarded pair is reported as `tampered_pairs` — an operator has
/// to be able to tell "someone planted links in my snapshots dir" from
/// an ordinary sidecar-less reclaim. It must not be folded into
/// `removal_failures`: the archive removal itself succeeded, so that
/// counter must keep meaning "an attempt failed".
#[test]
fn reclaim_counts_a_symlinked_sidecar_as_tampered() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    write_valid_archive(&dir, "snap_TAMP", b"precious backup");
    let outside = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path().join("victim"), dir.join("snap_TAMP.json")).unwrap();
    age_file(&dir.join("snap_TAMP.tar.gz"), 2 * 24 * 3600);
    age_file(&dir.join("snap_TAMP.json"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.archives_removed, 1, "the pair is discarded");
    assert_eq!(
        report.tampered_pairs, 1,
        "tampering must be distinguishable from an ordinary reclaim"
    );
    assert_eq!(
        report.removal_failures, 0,
        "the archive removal succeeded; removal_failures must stay 0"
    );
    assert!(dir.join("snap_TAMP.json").is_symlink());
}

/// The repair path stages its replacement as a hidden
/// `.memory-write-*.tmp` file next to the archives; a hard kill between
/// staging and publication leaves one behind, and no user-facing listing
/// ever shows it. The sweeper must reclaim it once it ages past the
/// guard — and only those, not an unrelated `.tmp` someone else placed
/// there.
#[test]
fn reclaim_removes_orphaned_atomic_write_tempfiles_only() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(dir.join(".memory-write-abc123.tmp"), b"half-written").unwrap();
    std::fs::write(dir.join("unrelated.tmp"), b"not ours").unwrap();
    age_file(&dir.join(".memory-write-abc123.tmp"), 2 * 24 * 3600);
    age_file(&dir.join("unrelated.tmp"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.partials_removed, 1, "only our own staging names");
    assert!(dir.join("unrelated.tmp").exists(), "foreign names stay");
    assert!(!dir.join(".memory-write-abc123.tmp").exists());
}

/// A gzip+tar archive with arbitrary entries (type + path per entry), for
/// shapes where a single entry is not enough — duplicates and prefix
/// collisions are pair-wise properties.
fn write_archive_with_entries(dir: &std::path::Path, id: &str, entries: &[(tar::EntryType, &str)]) {
    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(dir.join(format!("{id}.tar.gz"))).unwrap(),
        flate2::Compression::default(),
    );
    let mut tar = tar::Builder::new(gz);
    for (ty, path) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(*ty);
        // Directories need the execute bit or the extracted tree is
        // untraversable (unpack_dir applies the header mode), which fails
        // any read-back through it on a non-root run.
        header.set_mode(if *ty == tar::EntryType::Directory {
            0o755
        } else {
            0o644
        });
        header.set_size(0);
        // set_path rejects parent traversal and unusual components, so
        // construct entry names directly — same technique as
        // write_archive_with_entry.
        let bytes = path.as_bytes();
        header.as_mut_bytes()[..bytes.len()].copy_from_slice(bytes);
        header.set_cksum();
        tar.append(&header, std::io::empty()).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
}

/// `unpack_in` only *skips* entries whose name contains a `..` component;
/// `./notes.md` and `/notes.md` have those components dropped and are
/// re-rooted into staging, so restore accepts them. The validator must
/// accept them too — a sweep that deleted such an archive would destroy a
/// restorable backup (the destructive case the gate's strictness was
/// called out for). Repaired sidecar, archive survives.
#[test]
fn reclaim_repairs_archives_whose_entries_restore_accepts_dot_and_root_forms() {
    for (label, path) in [("curdir", "./notes.md"), ("rootdir", "/notes.md")] {
        let (_tmp, svc) = setup();
        let dir = svc.mount.meta_dir.join("snapshots");
        std::fs::create_dir_all(&dir).unwrap();

        write_archive_with_entry(&dir, "snap_DOT", tar::EntryType::Regular, path);
        std::fs::write(dir.join("snap_DOT.json"), b"{\"id\": \"snap_D").unwrap();
        age_file(&dir.join("snap_DOT.tar.gz"), 2 * 24 * 3600);
        age_file(&dir.join("snap_DOT.json"), 2 * 24 * 3600);

        let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
            &svc.mount,
            Duration::from_secs(24 * 3600),
        );
        assert_eq!(
            report.sidecars_repaired, 1,
            "{label}: restore accepts this archive, so the sidecar is repaired"
        );
        assert_eq!(
            report.archives_removed, 0,
            "{label}: a restorable archive must not be deleted"
        );
        assert!(dir.join("snap_DOT.tar.gz").exists(), "{label}: survives");
        assert_eq!(
            svc.mem_snapshot_list().unwrap().len(),
            1,
            "{label}: the repaired snapshot is advertised"
        );
    }
}

/// A zero-entry archive (what `create_tarball` writes for an empty mount)
/// whose sidecar was truncated is repaired, not deleted: the validator
/// deliberately accepts it, so deleting it would destroy a legitimate
/// backup of an empty workspace.
#[test]
fn reclaim_repairs_a_zero_entry_archive() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(dir.join("snap_EMPTY.tar.gz")).unwrap(),
        flate2::Compression::default(),
    );
    tar::Builder::new(gz)
        .into_inner()
        .unwrap()
        .finish()
        .unwrap();
    std::fs::write(dir.join("snap_EMPTY.json"), b"{\"id\": \"snap_E").unwrap();
    age_file(&dir.join("snap_EMPTY.tar.gz"), 2 * 24 * 3600);
    age_file(&dir.join("snap_EMPTY.json"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(
        report.sidecars_repaired, 1,
        "an empty archive is a valid backup of an empty mount"
    );
    assert_eq!(report.archives_removed, 0);
    assert!(dir.join("snap_EMPTY.tar.gz").exists());
    assert_eq!(svc.mem_snapshot_list().unwrap().len(), 1);
}

/// A duplicate entry name: `restore_tarball` extracts with overwrite
/// disabled, so the second extraction fails and the restore errors. The
/// shared gate must refuse such an archive so its sidecar is never
/// repaired — otherwise the snapshot would list forever and fail every
/// restore.
#[test]
fn reclaim_deletes_an_archive_with_duplicate_entry_names() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    write_archive_with_entries(
        &dir,
        "snap_DUP",
        &[
            (tar::EntryType::Regular, "notes.md"),
            (tar::EntryType::Regular, "notes.md"),
        ],
    );
    std::fs::write(dir.join("snap_DUP.json"), b"{\"id\": \"snap_D").unwrap();
    age_file(&dir.join("snap_DUP.tar.gz"), 2 * 24 * 3600);
    age_file(&dir.join("snap_DUP.json"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(report.sidecars_repaired, 0, "restore refuses duplicates");
    assert!(
        !dir.join("snap_DUP.tar.gz").exists(),
        "deleted, not repaired"
    );
    assert!(
        svc.mem_snapshot_list().unwrap().is_empty(),
        "nothing may be advertised"
    );
}

/// A regular file entry followed by an entry under it as a directory
/// (`a` then `a/b`): extraction fails with ENOTDIR once `a` exists as a
/// file, so restore refuses the archive and the gate must too.
#[test]
fn reclaim_deletes_an_archive_with_a_file_dir_prefix_collision() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    write_archive_with_entries(
        &dir,
        "snap_COL",
        &[
            (tar::EntryType::Regular, "a"),
            (tar::EntryType::Regular, "a/b"),
        ],
    );
    std::fs::write(dir.join("snap_COL.json"), b"{\"id\": \"snap_C").unwrap();
    age_file(&dir.join("snap_COL.tar.gz"), 2 * 24 * 3600);
    age_file(&dir.join("snap_COL.json"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(
        report.sidecars_repaired, 0,
        "restore fails on the file/dir collision"
    );
    assert!(
        !dir.join("snap_COL.tar.gz").exists(),
        "deleted, not repaired"
    );
    assert!(
        svc.mem_snapshot_list().unwrap().is_empty(),
        "nothing may be advertised"
    );
}

/// The same collision in the other order — a directory entry followed by
/// a path under it extracted as a regular file (`a/` then `a/b` as a
/// file) collides just as hard, and the reverse-containment check must
/// refuse it: nothing may advertise an archive whose restore dies
/// mid-extraction.
#[test]
fn reclaim_deletes_an_archive_with_a_dir_file_path_collision() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    write_archive_with_entries(
        &dir,
        "snap_REV",
        &[
            (tar::EntryType::Directory, "a"),
            (tar::EntryType::Regular, "a/b"),
            (tar::EntryType::Regular, "a"),
        ],
    );
    std::fs::write(dir.join("snap_REV.json"), b"{\"id\": \"snap_R").unwrap();
    age_file(&dir.join("snap_REV.tar.gz"), 2 * 24 * 3600);
    age_file(&dir.join("snap_REV.json"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(
        report.sidecars_repaired, 0,
        "the file `a` collides with dir `a`"
    );
    assert!(
        !dir.join("snap_REV.tar.gz").exists(),
        "deleted, not repaired"
    );
}

/// The populated-only refusal shape, validator side: `a/b` as a regular
/// file followed by `a` as another regular file never collides through the
/// files/dirs sets — only the populated set (check 4b) refuses it. Without
/// that set the sweep would repair and advertise a snapshot restore
/// refuses, a user-visible snapshot that can never restore.
#[test]
fn reclaim_deletes_an_archive_with_a_populated_subtree_collision() {
    let (_tmp, svc) = setup();
    let dir = svc.mount.meta_dir.join("snapshots");
    std::fs::create_dir_all(&dir).unwrap();

    write_archive_with_entries(
        &dir,
        "snap_POP",
        &[
            (tar::EntryType::Regular, "a/b"),
            (tar::EntryType::Regular, "a"),
        ],
    );
    std::fs::write(dir.join("snap_POP.json"), b"{\"id\": \"snap_P").unwrap();
    age_file(&dir.join("snap_POP.tar.gz"), 2 * 24 * 3600);
    age_file(&dir.join("snap_POP.json"), 2 * 24 * 3600);

    let report = agent_memory::snapshot::reclaim::reclaim_artifacts(
        &svc.mount,
        Duration::from_secs(24 * 3600),
    );
    assert_eq!(
        report.sidecars_repaired, 0,
        "restore refuses `a` over the extracted subtree at a/b"
    );
    assert!(
        !dir.join("snap_POP.tar.gz").exists(),
        "deleted, not repaired"
    );
    assert!(
        svc.mem_snapshot_list().unwrap().is_empty(),
        "nothing may be advertised"
    );
}

/// The restore side of the shared gate, refused shapes: each archive must
/// fail `mem_snapshot_restore` — the gate lives in restore too, and these
/// tests pin that half of the "validator and restore cannot disagree"
/// property (deleting `gate.admit` from restore's loop used to leave the
/// whole suite green).
#[test]
fn restore_refuses_the_gate_shapes_the_sweep_deletes() {
    for (label, entries) in [
        (
            "duplicate regular entries",
            vec![
                (tar::EntryType::Regular, "notes.md"),
                (tar::EntryType::Regular, "notes.md"),
            ],
        ),
        // Refusal carried solely by the populated set (check 4b): both
        // mutations of the set show up here.
        (
            "file then a path under it",
            vec![
                (tar::EntryType::Regular, "a"),
                (tar::EntryType::Regular, "a/b"),
            ],
        ),
        (
            "populated subtree then a regular at its name",
            vec![
                (tar::EntryType::Directory, "a/b"),
                (tar::EntryType::Regular, "a"),
            ],
        ),
        (
            "dir then a regular at its name",
            vec![
                (tar::EntryType::Directory, "a"),
                (tar::EntryType::Regular, "a"),
            ],
        ),
    ] {
        let (_tmp, svc) = setup();
        let dir = svc.mount.meta_dir.join("snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        write_archive_with_entries(&dir, "snap_GA", &entries);
        std::fs::write(dir.join("snap_GA.json"), b"{\"id\": \"snap_G").unwrap();
        let err = svc.mem_snapshot_restore("snap_GA").unwrap_err();
        assert!(
            err.to_string().contains("duplicate entry")
                || err.to_string().contains("collides")
                || err.to_string().contains("ancestor"),
            "{label}: unexpected refusal: {err}"
        );
    }
}

/// The gate must accept exactly what extraction accepts: GNU tar's
/// child-before-parent directory order, duplicate directory entries, a
/// directory landing over an already-created subtree, a `./` entry that
/// is not first, and empty-normalised names. Each shape restores `Ok`
/// and the content reads back — the "do not delete what restore accepts"
/// property pinned from the restore side.
#[test]
fn restore_accepts_directory_orders_and_dotted_forms_extraction_tolerates() {
    let depth_first = vec![
        (tar::EntryType::Regular, "project/src/main.rs"),
        (tar::EntryType::Directory, "project/src"),
        (tar::EntryType::Regular, "project/config/app.toml"),
        (tar::EntryType::Directory, "project/config"),
        (tar::EntryType::Directory, "project"),
    ];
    let admitted: [(&str, Vec<(tar::EntryType, &str)>); 5] = [
        // Depth-first ordering: child directories before their parents —
        // the shape `find -depth | tar --no-recursion -T -` produces and
        // the deferred-directory pass deliberately supports.
        ("child dirs before parent", depth_first),
        (
            "dir over existing subtree",
            vec![
                (tar::EntryType::Regular, "a/b"),
                (tar::EntryType::Directory, "a"),
            ],
        ),
        // Duplicate directory entries and a dir entry over an existing
        // subtree: `unpack_dir` tolerates both.
        (
            "duplicate directory entries",
            vec![
                (tar::EntryType::Directory, "d"),
                (tar::EntryType::Directory, "d"),
                (tar::EntryType::Regular, "d/f.md"),
            ],
        ),
        // A `./` entry after a regular file normalises to nothing and
        // extracts nothing; it must not collide with anything.
        (
            "root entry not first",
            vec![
                (tar::EntryType::Regular, "first.md"),
                (tar::EntryType::Directory, "./"),
            ],
        ),
        (
            "dir entry under a prior dir",
            vec![
                (tar::EntryType::Directory, "a"),
                (tar::EntryType::Directory, "a/b"),
                (tar::EntryType::Regular, "a/b/c.md"),
            ],
        ),
    ];
    // Each shape's own regular-file paths, read back after restore:
    // `write_archive_with_entries` writes empty files, so a successful
    // `svc.read` of the shape's leaf files both proves the tree is
    // traversable (the M1 mode class) and pins content round-trips per
    // shape.
    let read_back: [(&str, &[&str]); 5] = [
        (
            "child dirs before parent",
            &["project/src/main.rs", "project/config/app.toml"],
        ),
        ("dir over existing subtree", &["a/b"]),
        ("duplicate directory entries", &["d/f.md"]),
        ("root entry not first", &["first.md"]),
        ("dir entry under a prior dir", &["a/b/c.md"]),
    ];
    for ((label, entries), (read_label, paths)) in admitted.iter().zip(read_back.iter()) {
        assert_eq!(label, read_label, "read-back table drifted from the shapes");
        let (_tmp, svc) = setup();
        let dir = svc.mount.meta_dir.join("snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        write_archive_with_entries(&dir, "snap_ACC", entries);
        std::fs::write(dir.join("snap_ACC.json"), b"{\"id\": \"snap_A").unwrap();
        svc.write("live.md", "live", false).unwrap();
        svc.mem_snapshot_restore("snap_ACC")
            .unwrap_or_else(|e| panic!("{label}: restore must succeed: {e}"));
        for path in paths.iter() {
            assert_eq!(
                svc.read(path)
                    .unwrap_or_else(|e| panic!("{label}: read {path}: {e}")),
                "",
                "{label}: {path} must read back through the restored tree"
            );
        }
    }
}
