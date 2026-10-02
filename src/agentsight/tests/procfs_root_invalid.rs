//! `configure()` establishes a process-global procfs root, so each scenario
//! needs its own test binary: this one covers the misconfiguration branch --
//! a root without a pid-1 entry is kept (failing open would take down the
//! whole pipeline) but must not be confused with a working procfs.
//!
//! Runs without eBPF; safe as a normal user.

use agentsight::utils::procfs::{configure, proc_pid, proc_root};

#[test]
fn configure_keeps_a_root_without_pid_1() {
    let dir = std::env::temp_dir().join(format!("agentsight-procfs-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create empty dir");

    configure(&dir);
    // Kept, not rejected: the warning is the diagnostic, discovery just
    // finds nothing.
    assert_eq!(proc_root(), dir.as_path());
    assert_eq!(proc_pid(7u32), dir.join("7"));

    let _ = std::fs::remove_dir_all(&dir);
}
