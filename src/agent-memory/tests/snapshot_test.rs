//! Phase 6.3: snapshot create / list / restore round-trip.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::Command;

use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::error::MemoryError;
use agent_memory::ns::{MountPoint, Namespace};
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
fn snapshot_gzip_finish_failure_is_not_published() {
    const CHILD_BASE: &str = "AGENT_MEMORY_SNAPSHOT_LIMIT_TEST_BASE";
    if let Some(base) = std::env::var_os(CHILD_BASE) {
        let mount = MountPoint::ensure(
            Namespace::user("limit-test").unwrap(),
            std::path::Path::new(&base),
        )
        .unwrap();

        let result = agent_memory::snapshot::create(&mount, Some("limited"));
        assert!(result.is_err(), "incomplete gzip was published: {result:?}");
        assert!(result.unwrap_err().to_string().contains("gzip finish"));
        assert!(agent_memory::snapshot::list(&mount).unwrap().is_empty());
        for entry in std::fs::read_dir(mount.meta_dir.join("snapshots")).unwrap() {
            let path = entry.unwrap().path();
            assert!(
                !matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("gz" | "json")
                ),
                "failed snapshot published {}",
                path.display()
            );
        }
        return;
    }

    let tmp = tempdir().unwrap();
    let mount = MountPoint::ensure(Namespace::user("limit-test").unwrap(), tmp.path()).unwrap();
    let mut state = 0x1234_5678_u32;
    let contents: Vec<u8> = (0..8192)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    std::fs::write(mount.root.join("original.bin"), &contents).unwrap();

    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "snapshot_gzip_finish_failure_is_not_published",
            "--nocapture",
        ])
        .env(CHILD_BASE, tmp.path());
    // A separate process keeps the file limit and signal disposition away
    // from concurrently running tests. SIG_IGN turns EFBIG into an IO error.
    unsafe {
        child.pre_exec(|| {
            nix::libc::signal(nix::libc::SIGXFSZ, nix::libc::SIG_IGN);
            let limit = nix::libc::rlimit {
                rlim_cur: 1024,
                rlim_max: 1024,
            };
            if nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = child.output().unwrap();
    assert!(
        output.status.success(),
        "snapshot failure regression child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(mount.root.join("original.bin")).unwrap(),
        contents
    );
}

#[test]
fn snapshot_finishes_gzip_and_restores_binary_contents() {
    let (_tmp, svc) = setup();
    let contents: Vec<u8> = (0..8192).map(|i| (i % 256) as u8).collect();
    let original = svc.mount.root.join("original.bin");
    std::fs::write(&original, &contents).unwrap();
    let info = svc.mem_snapshot(Some("complete gzip")).unwrap();
    let archive = svc
        .mount
        .meta_dir
        .join("snapshots")
        .join(format!("{}.tar.gz", info.id));
    let bytes = std::fs::read(archive).unwrap();
    assert_eq!(info.size, bytes.len() as u64);
    // Reading to EOF also validates the gzip trailer; iterating tar entries
    // alone may stop at the tar end marker before consuming the trailer.
    flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_end(&mut Vec::new())
        .unwrap();
    std::fs::write(&original, b"changed").unwrap();
    svc.mem_snapshot_restore(&info.id).unwrap();
    assert_eq!(std::fs::read(original).unwrap(), contents);
}
