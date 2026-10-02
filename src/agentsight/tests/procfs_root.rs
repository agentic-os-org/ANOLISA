//! `configure()` establishes a process-global procfs root, so each scenario
//! needs its own test binary: this one covers the normal lifecycle -- set,
//! idempotent re-set, and rejection of a later different root -- plus the
//! public accessors composing under the configured root.
//!
//! Runs without eBPF; safe as a normal user.

use std::path::Path;

use agentsight::utils::procfs::{configure, proc_pid, proc_pid_entry, proc_pid_rooted, proc_root};

/// A scratch directory standing in for a bind-mounted host procfs.
fn fake_procfs(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("agentsight-procfs-{tag}-{}", std::process::id()));
    // A procfs root without a pid-1 entry looks like a misconfiguration to
    // configure(); give it one.
    std::fs::create_dir_all(dir.join("1")).expect("create fake procfs root");
    dir
}

#[test]
fn configure_establishes_the_root_and_rejects_later_overrides() {
    let root = fake_procfs("main");

    configure(&root);
    assert_eq!(proc_root(), root.as_path());

    // The accessors compose under the configured root.
    assert_eq!(proc_pid(7u32), root.join("7"));
    assert_eq!(proc_pid_entry(7u32, "cmdline"), root.join("7/cmdline"));
    let mut expected = root.join("7").into_os_string();
    expected.push("/root/usr/lib/libssl.so");
    assert_eq!(
        proc_pid_rooted(7u32, "/usr/lib/libssl.so"),
        std::path::PathBuf::from(expected)
    );

    // Re-setting the same root is an accepted no-op.
    configure(&root);
    assert_eq!(proc_root(), root.as_path());

    // A different root is rejected; the first one stays in effect.
    configure(Path::new("/nonexistent/elsewhere"));
    assert_eq!(proc_root(), root.as_path());

    let _ = std::fs::remove_dir_all(&root);
}
