//! Regression: `mem_index_refresh` must not write `MEMORY.md` through a
//! symlink planted in the mount.
//!
//! `refresh_index` is the one content *write* the index tool performs, and it
//! went through `std::fs::write` on the mount path, which follows symlinks.
//! Every other content write is anchored to the mount's `root_fd`
//! (openat2 `RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS`), and `safe_fs`'s module
//! doc names this exact adversary: "an attacker with write access to the
//! mount tree could swap a component for a symlink and escape". A symlink at
//! `<mount>/MEMORY.md` therefore turns `mem_index_refresh` into a
//! clobber-any-file-the-server-can-write primitive.

use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::error::MemoryError;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_index::refresh_index;

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.index.enabled = false;
    cfg.memory.git.enabled = false;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

#[test]
fn refresh_refuses_a_symlinked_memory_md() {
    let (tmp, svc) = setup();
    let victim = tmp.path().join("victim.txt");
    std::fs::write(&victim, "ORIGINAL CONTENT").unwrap();

    svc.write("notes/a.md", "hello body\n", false).unwrap();
    // The planted link lives inside the mount, not in the sandbox: this is
    // the "attacker with write access to the mount tree" from safe_fs's docs.
    std::os::unix::fs::symlink(&victim, svc.mount.root.join("MEMORY.md")).unwrap();

    let err = refresh_index(&svc).expect_err("a symlinked MEMORY.md must be refused");
    assert!(
        matches!(err, MemoryError::PathOutsideMount(_)),
        "expected PathOutsideMount, got: {err:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "ORIGINAL CONTENT",
        "the symlink target must not be truncated/overwritten"
    );
}

#[test]
fn refresh_still_writes_the_index_on_a_normal_mount() {
    let (_tmp, svc) = setup();
    svc.write("notes/a.md", "hello body\n", false).unwrap();

    let n = refresh_index(&svc).unwrap();
    assert!(n >= 1, "expected at least one indexed entry, got {n}");

    let index = std::fs::read_to_string(svc.mount.root.join("MEMORY.md")).unwrap();
    assert!(index.contains("# Memory Index"), "got: {index}");
    assert!(index.contains("notes/a.md"), "got: {index}");
}
