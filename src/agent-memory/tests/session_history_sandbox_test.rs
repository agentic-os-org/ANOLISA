//! Regression: `memory_sessions` must not read session summaries through a
//! symlink planted in the mount.
//!
//! `facts/summary/*.md` is agent-writable content, and the tool read each
//! entry with `std::fs::read_to_string`, which follows symlinks. A link at
//! `facts/summary/<name>.md` therefore makes the tool return a file from
//! outside the mount (its frontmatter is parsed and 200 chars of its body are
//! handed to the model in `description`). Every other content read is
//! anchored to the mount's `root_fd` (openat2 `RESOLVE_BENEATH|NO_SYMLINKS`),
//! which is the invariant `safe_fs` documents for exactly this adversary.

use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::service::MemoryService;
use agent_memory::tools::session_history::memory_sessions;

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
fn sessions_does_not_read_through_a_symlinked_summary() {
    let (tmp, svc) = setup();
    let secret = tmp.path().join("outside-secret.md");
    std::fs::write(
        &secret,
        "---\nsession_id: ses_leak\ncreated_at: 2026-01-01T00:00:00Z\n---\n\
         SECRET-OUTSIDE-MOUNT-DATA\n",
    )
    .unwrap();

    let summary_dir = svc.mount.root.join("facts").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    std::os::unix::fs::symlink(&secret, summary_dir.join("leak.md")).unwrap();

    let out = memory_sessions(&svc, 10).unwrap();
    assert!(
        !out.contains("SECRET-OUTSIDE-MOUNT-DATA"),
        "memory_sessions leaked the symlink target's body: {out}"
    );
    assert!(
        !out.contains("ses_leak"),
        "memory_sessions surfaced the symlink target as a session: {out}"
    );
    assert_eq!(out, "(no historical sessions found)");
}

#[test]
fn sessions_still_lists_real_summaries() {
    let (_tmp, svc) = setup();
    let summary_dir = svc.mount.root.join("facts").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    std::fs::write(
        summary_dir.join("ses_real.md"),
        "---\nsession_id: ses_real\ncreated_at: 2026-01-02T00:00:00Z\n---\n\
         Session had 42 tool calls.\n",
    )
    .unwrap();

    let out = memory_sessions(&svc, 10).unwrap();
    assert!(out.contains("ses_real"), "got: {out}");
    assert!(
        out.contains("42"),
        "expected the parsed tool-call count, got: {out}"
    );
}
