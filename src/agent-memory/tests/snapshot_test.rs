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
