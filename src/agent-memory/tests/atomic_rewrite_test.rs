//! A rewrite that fails midway must not consume the previous contents.
//!
//! `mem_revert`, a merge import that replaces an existing entry, and a consent
//! save all rewrote their destination in place before this probe existed: the
//! file was truncated, the replacement bytes were written, and a short write
//! left a prefix behind while the call still returned an error. The probe forces
//! that short write with `RLIMIT_FSIZE`, which affects the whole process, so each
//! case runs in a dedicated child instead of the shared test runner.

use agent_memory::config::AppConfig;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_import::{ImportStrategy, memory_import};
use agent_memory::tools::memory_sovereignty::memory_consent;

const CHILD_ENV: &str = "ANOLISA_ATOMIC_REWRITE_CHILD";
const ORIGINAL: &str = "original important memory";

fn setup(git: bool) -> (tempfile::TempDir, MemoryService) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.memory.paths.base_dir = tmp.path().join("data").to_string_lossy().into_owned();
    cfg.memory.session.base_dir = tmp.path().join("sessions").to_string_lossy().into_owned();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.index.enabled = false;
    cfg.memory.git.enabled = git;
    (tmp, MemoryService::new(cfg).unwrap())
}

fn archive_with_doc(body: &str) -> String {
    format!(
        r#"{{"version":"1.0","format":"anolisa-memory-archive","exported_at":"2026-11-05T00:00:00Z","agent_id":"tester","user_id":"tester","total_memories":1,"stats":{{"by_category":{{}},"by_source":{{}},"total_bytes":0}},"memories":[{{"path":"notes/doc.md","frontmatter":{{"id":"e"}},"content":"{body}"}}],"tasks":[]}}"#
    )
}

/// Run `operation` with the file-size soft limit lowered to 32 bytes.
///
/// Returns the operation result; the limit and the `SIGXFSZ` disposition are
/// restored before returning so the caller can inspect the destination.
fn with_short_writes<T>(operation: impl FnOnce() -> T) -> T {
    let mut old_limit = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid local pointers. This runs in a dedicated child process that
    // restores both settings before any assertion.
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
fn failed_rewrite_preserves_the_previous_contents() {
    if std::env::var_os(CHILD_ENV).is_none() {
        // Report every case, not just the first one that breaks a file.
        let mut failures = Vec::new();
        for operation in ["revert", "merge-import", "consent"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "failed_rewrite_preserves_the_previous_contents",
                    "--nocapture",
                ])
                .env(CHILD_ENV, operation)
                .output()
                .unwrap();
            print!("{}", String::from_utf8_lossy(&output.stdout));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            if !String::from_utf8_lossy(&output.stdout).contains("atomic-rewrite-child-completed")
                || !output.status.success()
            {
                failures.push(operation);
            }
        }
        assert!(
            failures.is_empty(),
            "a failed rewrite destroyed the previous contents: {failures:?}"
        );
        return;
    }

    let replacement = "N".repeat(128);
    match std::env::var(CHILD_ENV).unwrap().as_str() {
        "revert" => {
            let (_tmp, svc) = setup(true);
            svc.write("doc.md", &replacement, false).unwrap();
            // An uncommitted edit is the only copy of the current contents.
            std::fs::write(svc.mount.root.join("doc.md"), ORIGINAL).unwrap();
            let result = with_short_writes(|| svc.mem_revert("doc.md"));
            println!("revert result: {result:?}");
            assert!(result.is_err(), "the kernel must reject the rewrite");
            assert_eq!(
                svc.read("doc.md").unwrap(),
                ORIGINAL,
                "a failed revert must keep the uncommitted working copy"
            );
        }
        "merge-import" => {
            let (_tmp, svc) = setup(false);
            svc.write("notes/doc.md", ORIGINAL, false).unwrap();
            let json = archive_with_doc(&replacement);
            let result =
                with_short_writes(|| memory_import(&svc, &json, ImportStrategy::Merge, false));
            println!("merge import result: {result:?}");
            match &result {
                Ok(summary) => assert!(
                    summary.contains("1 errors"),
                    "the merge import must report the failed entry: {summary}"
                ),
                Err(e) => assert!(
                    e.to_string().contains("File too large"),
                    "the merge import failed for an unrelated reason: {e}"
                ),
            }
            assert_eq!(
                svc.read("notes/doc.md").unwrap(),
                ORIGINAL,
                "a failed merge import must keep the only stored copy"
            );
        }
        "consent" => {
            let (_tmp, svc) = setup(false);
            memory_consent(&svc, Some("deny"), Some("consolidation")).unwrap();
            let before = std::fs::read(svc.mount.meta_dir.join("consent.toml")).unwrap();
            let result =
                with_short_writes(|| memory_consent(&svc, Some("allow"), Some("consolidation")));
            println!("consent result: {result:?}");
            assert!(result.is_err(), "the kernel must reject the rewrite");
            assert_eq!(
                std::fs::read(svc.mount.meta_dir.join("consent.toml")).unwrap(),
                before,
                "a failed consent save must leave the previous settings intact"
            );
        }
        other => panic!("unknown child operation: {other}"),
    }
    println!("atomic-rewrite-child-completed");
}

/// A revert that has to bring a deleted file back has no destination to
/// replace, so the staged rewrite alone cannot serve it: it returned NotFound
/// and the file stayed gone.
#[test]
fn revert_restores_a_file_deleted_from_the_working_tree() {
    let (_tmp, svc) = setup(true);
    svc.write("doc.md", ORIGINAL, false).unwrap();
    std::fs::remove_file(svc.mount.root.join("doc.md")).unwrap();

    let result = svc.mem_revert("doc.md");

    assert!(
        result.is_ok(),
        "revert must restore a deleted file: {result:?}"
    );
    assert_eq!(svc.read("doc.md").unwrap(), ORIGINAL);
}
