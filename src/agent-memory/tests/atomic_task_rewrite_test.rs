//! A task save and a consolidation fact write that fail part-way must not
//! consume what was already stored.
//!
//! Both are upserts: `memory_task_save` replaces `tasks/<id>.md` for an id that
//! already exists, and a fact id is content-derived, so writing the same fact
//! again lands on the same file. `safe_fs::write` truncated the destination
//! first, so a short write kept a prefix of the replacement and dropped the rest
//! while the call still returned an error. The probe forces that short write with
//! `RLIMIT_FSIZE`, which is per-process, so each case runs in a dedicated child.

use agent_memory::config::AppConfig;
use agent_memory::consolidation::{ConsolidatedFact, FactCategory, FactWriter};
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_task::memory_task_save;

const CHILD_ENV: &str = "ANOLISA_ATOMIC_TASK_CHILD";

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.memory.paths.base_dir = tmp.path().join("data").to_string_lossy().into_owned();
    cfg.memory.session.base_dir = tmp.path().join("sessions").to_string_lossy().into_owned();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.index.enabled = false;
    cfg.memory.git.enabled = false;
    (tmp, MemoryService::new(cfg).unwrap())
}

fn save(svc: &MemoryService, title: &str, context: &str) -> agent_memory::error::Result<String> {
    memory_task_save(
        svc,
        title,
        Some("in-progress"),
        None,
        None,
        None,
        None,
        None,
        Some(context),
        Some("t1"),
    )
    .map(|_| ())
    .map(|_| String::new())
}

fn fact(content: &str) -> ConsolidatedFact {
    ConsolidatedFact {
        id: "fixed-fact-id".to_string(),
        session_id: "session".to_string(),
        category: FactCategory::Lesson,
        title: "title".to_string(),
        content: content.to_string(),
        source_tool: "memory_write".to_string(),
        related_paths: Vec::new(),
        confidence: 0.9,
        created_at: "2026-11-05T00:00:00Z".to_string(),
        token_estimate: 1,
    }
}

/// Run `operation` with the file-size soft limit lowered to 32 bytes.
fn with_short_writes<T>(operation: impl FnOnce() -> T) -> T {
    let mut old_limit = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid local pointers; this child restores both settings before
    // any assertion, and the caller only runs it in a dedicated process.
    let old_signal = unsafe {
        assert_eq!(
            nix::libc::getrlimit(nix::libc::RLIMIT_FSIZE, &mut old_limit),
            0
        );
        let signal = nix::libc::signal(nix::libc::SIGXFSZ, nix::libc::SIG_IGN);
        let limit = nix::libc::rlimit {
            rlim_cur: 32.min(old_limit.rlim_max),
            rlim_max: old_limit.rlim_max,
        };
        assert_eq!(nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &limit), 0);
        signal
    };
    let result = operation();
    unsafe {
        assert_eq!(nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &old_limit), 0);
        nix::libc::signal(nix::libc::SIGXFSZ, old_signal);
    }
    result
}

#[test]
fn failed_upsert_preserves_the_stored_task_and_fact() {
    if std::env::var_os(CHILD_ENV).is_none() {
        let mut failures = Vec::new();
        for operation in ["task", "fact", "fact-sandboxed"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "failed_upsert_preserves_the_stored_task_and_fact",
                    "--nocapture",
                ])
                .env(CHILD_ENV, operation)
                .output()
                .unwrap();
            print!("{}", String::from_utf8_lossy(&output.stdout));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            if !String::from_utf8_lossy(&output.stdout).contains("atomic-upsert-child-completed")
                || !output.status.success()
            {
                failures.push(operation);
            }
        }
        assert!(
            failures.is_empty(),
            "a failed upsert destroyed the stored {failures:?}"
        );
        return;
    }

    match std::env::var(CHILD_ENV).unwrap().as_str() {
        "task" => {
            let (_tmp, svc) = setup();
            save(&svc, "original title", "original context").unwrap();
            let path = svc.mount.root.join("tasks/t1.md");
            let before = std::fs::read(&path).unwrap();
            let result = with_short_writes(|| save(&svc, &"N".repeat(128), &"N".repeat(128)));
            println!("task save result: {result:?}");
            assert!(result.is_err(), "the kernel must reject the rewrite");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "a failed task save must keep the stored task"
            );
        }
        "fact" => {
            let tmp = tempfile::tempdir().unwrap();
            let base = tmp.path().to_path_buf();
            let writer = FactWriter::new(&base);
            writer.write(&fact("original fact")).unwrap();
            let path = base.join("facts/lesson/fixed-fact-id.md");
            let before = std::fs::read(&path).unwrap();
            let result = with_short_writes(|| writer.write(&fact(&"N".repeat(128))));
            println!("fact write result: {result:?}");
            assert!(result.is_err(), "the kernel must reject the rewrite");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "a failed fact write must keep the stored fact"
            );
            // Nothing reclaims a staging file here: reclaim only walks the
            // snapshot dir and only matches `.memory-write-*` tempfiles, and
            // git_repo's add_all would commit any leftover into the history.
            let leftovers: Vec<String> = std::fs::read_dir(base.join("facts/lesson"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name != "fixed-fact-id.md")
                .collect();
            assert!(
                leftovers.is_empty(),
                "a failed fact write left staging files behind: {leftovers:?}"
            );
        }
        "fact-sandboxed" => {
            // The route production takes: service/mod.rs always attaches the fd,
            // so the staged write_atomic branch is the one that actually runs.
            let (_tmp, svc) = setup();
            let writer = FactWriter::new(&svc.mount.root).with_root_fd(svc.mount.root_fd.clone());
            writer.write(&fact("original fact")).unwrap();
            let path = svc.mount.root.join("facts/lesson/fixed-fact-id.md");
            let before = std::fs::read(&path).unwrap();
            let result = with_short_writes(|| writer.write(&fact(&"N".repeat(128))));
            println!("sandboxed fact write result: {result:?}");
            assert!(result.is_err(), "the kernel must reject the rewrite");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "a failed sandboxed fact write must keep the stored fact"
            );
        }
        other => panic!("unknown child operation: {other}"),
    }
    println!("atomic-upsert-child-completed");
}
