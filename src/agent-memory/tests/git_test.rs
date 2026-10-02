//! Phase 6.2: git versioning end-to-end (auto-commit + log + revert).

use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::error::MemoryError;
use agent_memory::service::MemoryService;

fn setup(git_enabled: bool, auto_commit: bool) -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "git-tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.git.enabled = git_enabled;
    cfg.memory.git.auto_commit = auto_commit;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

#[test]
fn git_disabled_means_no_repo() {
    let (_tmp, svc) = setup(false, false);
    assert!(svc.git.is_none());
    assert!(!svc.mount.root.join(".git").exists());

    let err = svc.mem_log(10, None).unwrap_err();
    assert!(matches!(err, MemoryError::NotImplemented(_)));
}

#[test]
fn git_enabled_initializes_repo_with_gitignore() {
    let (_tmp, svc) = setup(true, false);
    assert!(svc.git.is_some());
    let git_dir = svc.mount.root.join(".git");
    assert!(git_dir.is_dir(), "expected .git/ at {}", git_dir.display());

    let gi = std::fs::read_to_string(svc.mount.root.join(".gitignore")).unwrap();
    assert!(gi.contains(".anolisa/"));

    // Initial commit recorded.
    let log = svc.mem_log(10, None).unwrap();
    assert!(!log.is_empty(), "expected at least an initial commit");
}

#[test]
fn auto_commit_records_writes() {
    let (_tmp, svc) = setup(true, true);
    let baseline = svc.mem_log(50, None).unwrap().len();

    svc.write("notes/hello.md", "first", false).unwrap();
    svc.write("notes/world.md", "second", false).unwrap();

    let after = svc.mem_log(50, None).unwrap();
    assert!(
        after.len() >= baseline + 2,
        "expected ≥{} commits, got {} ({:?})",
        baseline + 2,
        after.len(),
        after.iter().map(|e| &e.summary).collect::<Vec<_>>()
    );
    let summaries: Vec<&str> = after.iter().map(|e| e.summary.as_str()).collect();
    assert!(
        summaries
            .iter()
            .any(|s| s.contains("mem_write notes/hello.md")),
        "missing mem_write hello: {summaries:?}"
    );
}

#[test]
fn read_does_not_create_commits() {
    let (_tmp, svc) = setup(true, true);
    svc.write("a.md", "alpha", false).unwrap();
    let before = svc.mem_log(50, None).unwrap().len();
    let _ = svc.read("a.md").unwrap();
    let _ = svc.read("a.md").unwrap();
    let after = svc.mem_log(50, None).unwrap().len();
    assert_eq!(before, after, "reads should not bump HEAD");
}

#[test]
fn mem_revert_restores_committed_content() {
    let (_tmp, svc) = setup(true, true);
    svc.write("doc.md", "v1", false).unwrap();
    // Now uncommitted edit (write through OS, but skip git auto-commit
    // by going through raw fs).
    let p = svc.mount.root.join("doc.md");
    std::fs::write(&p, "v2 uncommitted").unwrap();
    assert_eq!(svc.read("doc.md").unwrap(), "v2 uncommitted");

    svc.mem_revert("doc.md").unwrap();
    assert_eq!(svc.read("doc.md").unwrap(), "v1");
}

#[test]
fn mem_log_filters_by_path() {
    let (_tmp, svc) = setup(true, true);
    svc.write("alpha.md", "a1", false).unwrap();
    svc.write("beta.md", "b1", false).unwrap();
    svc.write("alpha.md", "a2", true).unwrap();

    let alpha_log = svc.mem_log(50, Some("alpha.md")).unwrap();
    let beta_log = svc.mem_log(50, Some("beta.md")).unwrap();
    assert!(
        alpha_log.len() > beta_log.len(),
        "alpha should have more commits: alpha={} beta={}",
        alpha_log.len(),
        beta_log.len()
    );
}

#[test]
fn revert_unknown_path_errors() {
    let (_tmp, svc) = setup(true, true);
    let err = svc.mem_revert("does-not-exist.md").unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));
}

#[test]
fn concurrent_writes_serialize_into_history() {
    // Regression: two write tools called from concurrent tokio tasks both
    // try to drive `commit_all`, racing on git index.lock. Pre-fix the
    // loser was dropped at debug-level and the user lost a commit.
    //
    // Note: commit_all stages with `add_all(["*"])`, so the first
    // writer's commit may sweep up files a concurrent writer has
    // already flushed to disk; subsequent commits then see no tree
    // change and are skipped by the empty-commit guard. So we don't
    // assert one-commit-per-write; instead we verify (a) every file
    // produced by every thread is reflected in *some* committed tree
    // (no silent loss), and (b) at least one new commit landed beyond
    // baseline (the mutex didn't deadlock).
    use std::sync::Arc;
    use std::thread;

    let (_tmp, svc) = setup(true, true);
    let svc = Arc::new(svc);
    let baseline = svc.mem_log(200, None).unwrap().len();

    let n = 8;
    let mut handles = Vec::with_capacity(n);
    for i in 0..n {
        let svc = Arc::clone(&svc);
        handles.push(thread::spawn(move || {
            let path = format!("notes/c{i}.md");
            svc.write(&path, &format!("body {i}"), false).unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    for i in 0..n {
        let p = format!("notes/c{i}.md");
        let touched = svc.mem_log(200, Some(&p)).unwrap_or_default();
        assert!(
            !touched.is_empty(),
            "expected at least one commit touching {p} after concurrent writes",
        );
    }

    let after = svc.mem_log(200, None).unwrap().len();
    assert!(
        after > baseline,
        "expected at least one new commit beyond baseline {baseline}, got {after}",
    );
}
