//! A task save and a consolidation fact write that fail part-way must not
//! consume what was already stored.
//!
//! `memory_task_save` is an upsert: it replaces `tasks/<id>.md` for an id that
//! already exists. A fact write normally creates a new path, because fact ids
//! are ULIDs; it only replaces an existing file when the id is already there,
//! which this test forces. `safe_fs::write` truncated the task file before it
//! rewrote, so a short write kept a prefix of the replacement and dropped the
//! rest while the call still returned an error. The probe forces that short
//! write with `RLIMIT_FSIZE`, which is per-process, so each case runs in a
//! dedicated child.

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

fn save(svc: &MemoryService, title: &str, context: &str) -> agent_memory::error::Result<()> {
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
        for operation in [
            "task",
            "fact",
            "fact-sandboxed",
            "fact-new",
            "fact-new-unsandboxed",
            "fact-new-concurrent",
        ] {
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
            "a failed write was not contained for {failures:?}"
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
            // Without the sandbox anchor a replacement is refused outright:
            // staging one here would publish the staging file's mode and ACLs
            // and bypass the destination's write-permission gate, so the
            // writer declines instead of weakening either guarantee.
            let refusal = result.expect_err("a replacement without the anchor must be refused");
            assert!(
                refusal.to_string().contains("needs the sandbox root fd"),
                "the refusal must name what is missing, got: {refusal}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "a refused fact write must keep the stored fact"
            );
            // Nothing reclaims a staging file here: reclaim only walks the
            // snapshot dir and only matches `.memory-write-*` tempfiles, and
            // git_repo's add_all would commit any leftover into the history.
            assert_eq!(
                leftover_entries(&base.join("facts/lesson")),
                vec!["fixed-fact-id.md".to_string()],
                "a failed fact write left staging files behind"
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
            // The same litter check as the unsandboxed arm, over both directories
            // this path can touch: the sandboxed branch stages through
            // `safe_fs::write_atomic`, and a regression there would litter the
            // meta dir rather than `facts/lesson`. Without this the arm asserts
            // nothing about leftovers and a planted `.memory-write-*.tmp` in
            // `.anolisa` would pass.
            let fact_leftovers = leftover_entries(&svc.mount.root.join("facts/lesson"));
            assert_eq!(
                fact_leftovers,
                vec!["fixed-fact-id.md".to_string()],
                "a failed sandboxed fact write left staging files behind"
            );
            let allowed = ["audit.log", "manifest.toml", "session-logs"];
            let meta_leftovers: Vec<String> = leftover_entries(&svc.mount.meta_dir)
                .into_iter()
                .filter(|name| !allowed.contains(&name.as_str()))
                .collect();
            assert!(
                meta_leftovers.is_empty(),
                "a failed sandboxed fact write left staging files in the meta dir: \
                 {meta_leftovers:?}"
            );
        }
        "fact-new-concurrent" => {
            // Two writes of the same id racing on one writer. The staging name
            // used to be unique per (id, pid) rather than per attempt, so the
            // loser of `create_new` cleaned up the winner's in-flight file and
            // both writes failed -- 40 of 40 rounds before the guard.
            for round in 0..20 {
                let tmp = tempfile::tempdir().unwrap();
                let writer = std::sync::Arc::new(FactWriter::new(tmp.path()));
                let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
                let handles: Vec<_> = (0..2)
                    .map(|_| {
                        let (writer, barrier) = (writer.clone(), barrier.clone());
                        std::thread::spawn(move || {
                            barrier.wait();
                            writer.write(&fact(&"N".repeat(128)))
                        })
                    })
                    .collect();
                let results: Vec<_> = handles
                    .into_iter()
                    .map(|handle| handle.join().expect("the write thread panicked"))
                    .collect();
                assert!(
                    results.iter().any(Result::is_ok),
                    "round {round}: both writes of one id failed: {results:?}"
                );
                // `any(is_ok)` cannot see the per-attempt suffix: with a
                // pid-only name and a conditional guard the loser fails
                // `EEXIST` instead of reaching the documented refusal, which
                // no assertion above notices. Which error may appear is the
                // part that is deterministic -- a thread that checks
                // `exists()` after its sibling's rename legitimately gets the
                // refusal.
                for result in &results {
                    if let Err(error) = result {
                        assert!(
                            error.to_string().contains("needs the sandbox root fd"),
                            "round {round}: a write lost the staging-name race \
                             instead of taking the documented refusal: {error}"
                        );
                    }
                }
                // The published destination is expected here; only a
                // surviving staging name means the race leaked one.
                let staging: Vec<String> = leftover_entries(&tmp.path().join("facts/lesson"))
                    .into_iter()
                    .filter(|name| name.contains(".memory-write-"))
                    .collect();
                assert!(
                    staging.is_empty(),
                    "round {round}: the race left staging files behind: {staging:?}"
                );
            }
        }
        "fact-new-unsandboxed" => {
            // The create arm without the sandbox anchor: ~18 lines including
            // `create_new`, `sync_all`, `rename` and the cleanup, which the
            // `fact` child only reaches as setup on its happy path. Deleting
            // the cleanup leaves the whole suite green without this child.
            let tmp = tempfile::tempdir().unwrap();
            let base = tmp.path().to_path_buf();
            let writer = FactWriter::new(&base);
            let result = with_short_writes(|| writer.write(&fact(&"N".repeat(128))));
            println!("unsandboxed new-fact write result: {result:?}");
            assert!(result.is_err(), "the kernel must reject the write");
            assert_eq!(
                leftover_entries(&base.join("facts/lesson")),
                Vec::<String>::new(),
                "a failed unsandboxed fact create left staging files behind"
            );
        }
        "fact-new" => {
            // The normal fact route: a ULID ids a path that does not exist
            // yet, so production creates rather than replaces. Nothing stored
            // is lost here, and at main the failure left a truncated file at
            // the new path that mem_export still returned with an empty
            // frontmatter map -- silent corruption. The assertions are the
            // two halves of that: no residue after the failure, and no
            // truncated file either.
            let (_tmp, svc) = setup();
            let writer = FactWriter::new(&svc.mount.root).with_root_fd(svc.mount.root_fd.clone());
            let result = with_short_writes(|| writer.write(&fact(&"N".repeat(128))));
            println!("sandboxed new-fact write result: {result:?}");
            assert!(result.is_err(), "the kernel must reject the write");
            assert_eq!(
                leftover_entries(&svc.mount.root.join("facts/lesson")),
                Vec::<String>::new(),
                "a failed new-fact write left a truncated file behind"
            );
            let allowed = ["audit.log", "manifest.toml", "session-logs"];
            let meta_leftovers: Vec<String> = leftover_entries(&svc.mount.meta_dir)
                .into_iter()
                .filter(|name| !allowed.contains(&name.as_str()))
                .collect();
            assert!(
                meta_leftovers.is_empty(),
                "a failed new-fact write left staging files in the meta dir: {meta_leftovers:?}"
            );
        }
        other => panic!("unknown child operation: {other}"),
    }
    println!("atomic-upsert-child-completed");
}

/// Every entry in a directory, sorted, so an assertion can name the exact set.
fn leftover_entries(dir: &std::path::Path) -> Vec<String> {
    // A failed create can leave the category directory absent altogether,
    // which is the same answer as empty for every caller here.
    if !dir.exists() {
        return Vec::new();
    }
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
