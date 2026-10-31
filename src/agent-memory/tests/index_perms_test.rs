//! The BM25 index tree under `.anolisa` must stay owner-only: the index
//! carries the keywords, paths and statistics of every indexed memory.

use std::os::unix::fs::PermissionsExt;

use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::mount::MountStrategyKind;
use agent_memory::service::MemoryService;

fn setup_with_user(user: &str) -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = user.into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

#[test]
fn bm25_db_and_index_dir_are_owner_only() {
    let (_tmp, svc) = setup_with_user("idx-perms");
    svc.write("notes/a.md", "alpha", false).unwrap();

    let index_dir = svc.mount.meta_dir.join("index");
    assert_eq!(
        std::fs::metadata(&index_dir).unwrap().permissions().mode() & 0o777,
        0o700,
        "the index dir must not be reachable by group/other"
    );
    assert_eq!(
        std::fs::metadata(index_dir.join("bm25.db"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "the BM25 database must not be readable by group/other"
    );
}

#[test]
fn an_old_0755_0644_index_layout_is_re_tightened() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().join("user-idx-old");
    let index_dir = root.join(".anolisa").join("index");
    std::fs::create_dir_all(&index_dir).unwrap();
    std::fs::set_permissions(
        root.join(".anolisa"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::set_permissions(&index_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    // An empty file opens as a fresh sqlite database, so the pre-existing
    // 0644 inode is exactly what the store will adopt.
    std::fs::write(index_dir.join("bm25.db"), b"").unwrap();
    std::fs::set_permissions(
        index_dir.join("bm25.db"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();

    let mut cfg = AppConfig::default();
    cfg.global.user_id = "idx-old".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();

    assert_eq!(
        std::fs::metadata(&svc.mount.meta_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "the metadata dir from an older build must be re-tightened on start"
    );
    assert_eq!(
        std::fs::metadata(&index_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(index_dir.join("bm25.db"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "a pre-existing 0644 database must be repaired on open"
    );
}
