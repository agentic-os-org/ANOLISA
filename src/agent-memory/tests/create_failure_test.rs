//! Recoverable exclusive-write failures must not reserve the destination name.

use agent_memory::{config::AppConfig, service::MemoryService};

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

#[test]
fn failed_exclusive_write_allows_retry() {
    const CHILD: &str = "ANOLISA_CREATE_FAILURE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        for (operation, destination) in [
            ("promote", "notes/published.md"),
            ("write", "notes/published.md"),
            ("promote", "genesis.md"),
            ("write", "genesis.md"),
        ] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "failed_exclusive_write_allows_retry",
                    "--nocapture",
                ])
                .env(CHILD, operation)
                .env("ANOLISA_CREATE_FAILURE_DESTINATION", destination)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("create-failure-child-completed")
            );
        }
        return;
    }
    let (_tmp, svc) = setup();
    let content = "important draft\n".repeat(64);
    let source = svc
        .session
        .as_ref()
        .unwrap()
        .scratch_root()
        .join("draft.md");
    std::fs::write(&source, &content).unwrap();
    let operation = std::env::var(CHILD).unwrap();
    let relative = std::env::var("ANOLISA_CREATE_FAILURE_DESTINATION").unwrap();
    let create = || {
        if operation == "promote" {
            svc.promote("draft.md", &relative)
        } else {
            svc.write(&relative, &content, false)
        }
    };
    let mut old_limit = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid local pointers. Only this child changes its resource limit
    // and ignores SIGXFSZ; both settings are restored before assertions.
    let old_signal = unsafe {
        assert_eq!(
            nix::libc::getrlimit(nix::libc::RLIMIT_FSIZE, &mut old_limit),
            0
        );
        let signal = nix::libc::signal(nix::libc::SIGXFSZ, nix::libc::SIG_IGN);
        assert_ne!(signal, nix::libc::SIG_ERR);
        let limit = nix::libc::rlimit {
            rlim_cur: 32.min(old_limit.rlim_max),
            rlim_max: old_limit.rlim_max,
        };
        assert_eq!(nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &limit), 0);
        signal
    };
    let failed = create();
    // SAFETY: restore the process settings captured above in this child.
    unsafe {
        assert_eq!(nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &old_limit), 0);
        assert_ne!(
            nix::libc::signal(nix::libc::SIGXFSZ, old_signal),
            nix::libc::SIG_ERR
        );
    }
    assert!(failed.is_err(), "the kernel must reject the write");
    let destination = svc.mount.root.join(&relative);
    println!(
        "{operation} {relative}: {failed:?}; destination length after error: {:?}",
        std::fs::metadata(&destination).map(|m| m.len())
    );
    assert!(
        !destination.exists(),
        "failed creation left an incomplete file that blocks retries"
    );
    assert_eq!(std::fs::read_to_string(&source).unwrap(), content);
    assert_eq!(create().unwrap(), content.len() as u64);
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), content);
    assert!(
        create().is_err(),
        "a successful destination must not be overwritten"
    );
    assert_eq!(std::fs::read_to_string(destination).unwrap(), content);
    println!("create-failure-child-completed");
}
