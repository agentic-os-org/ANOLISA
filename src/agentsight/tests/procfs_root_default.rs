//! `configure()` establishes a process-global procfs root, so each scenario
//! needs its own test binary: this one covers the default-root branch --
//! explicitly configuring `/proc` must be accepted and observable.
//!
//! Runs without eBPF; safe as a normal user.

use std::path::Path;

use agentsight::utils::procfs::{configure, proc_root};

#[test]
fn configure_accepts_the_default_root() {
    configure(Path::new("/proc"));
    assert_eq!(proc_root(), Path::new("/proc"));
}
