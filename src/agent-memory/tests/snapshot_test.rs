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

// ---- artefact reclaim (#9193) ----

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
    // write_sidecar): the archive is restorable, so the sweep must repair
    // the sidecar rather than remove either file.
    std::fs::write(dir.join("snap_FIX.tar.gz"), b"archive-bytes").unwrap();
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
    assert_eq!(list[0].size, b"archive-bytes".len() as u64);
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
