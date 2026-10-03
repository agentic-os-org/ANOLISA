//! Integration test for the cgroup module.
//!
//! cgroup operations in their full form (mkdir under /sys/fs/cgroup,
//! writing memory.max) require root and a writable cgroup v2 tree;
//! those paths are exercised on aos2 e2e. These tests cover the
//! cross-platform-safe surface: parser + Skipped / Failed outcomes so
//! that no test environment can panic the apply pipeline.
//!
//! Phase 6.4 used to have only unit tests inside the module; this file
//! provides the integration-level regression guard called out in review.

use agent_memory::cgroup::{CgroupConfig, CgroupOutcome, apply, parse_memory_max};

#[test]
fn apply_disabled_returns_skipped() {
    // Default config has enabled=false; apply must short-circuit without
    // touching the filesystem so unprivileged / non-Linux test runs
    // remain hermetic.
    let cfg = CgroupConfig::default();
    assert!(!cfg.enabled, "default config should be disabled");
    assert_eq!(apply(&cfg), CgroupOutcome::Skipped);
}

#[test]
fn apply_enabled_without_privilege_returns_failed_not_panic() {
    // Setting enabled=true on a host without cgroup v2 write access
    // (typical CI / unprivileged test sandbox / macOS) must return
    // Failed, NOT panic. The startup pipeline relies on this to "warn
    // and continue" instead of aborting the service.
    let cfg = CgroupConfig {
        enabled: true,
        memory_max: "16M".to_string(),
    };
    match apply(&cfg) {
        CgroupOutcome::Joined { .. } => {
            // Joined is possible if the test was somehow run as root
            // inside a delegated cgroup; that's fine — no panic.
        }
        CgroupOutcome::Failed(msg) => {
            assert!(!msg.is_empty(), "Failed must carry a diagnostic");
        }
        CgroupOutcome::Skipped => panic!("enabled=true should not be Skipped"),
    }
}

#[test]
fn parse_memory_max_accepts_common_units() {
    assert_eq!(parse_memory_max("1024").unwrap(), 1024);
    assert_eq!(parse_memory_max("1K").unwrap(), 1024);
    assert_eq!(parse_memory_max("2M").unwrap(), 2 * 1024 * 1024);
    assert_eq!(parse_memory_max("1G").unwrap(), 1024 * 1024 * 1024);
    // Mixed-case suffix.
    assert_eq!(parse_memory_max("4g").unwrap(), 4 * 1024 * 1024 * 1024);
}

#[test]
fn parse_memory_max_rejects_garbage() {
    assert!(parse_memory_max("").is_err());
    assert!(parse_memory_max("garbage").is_err());
    assert!(parse_memory_max("12X").is_err());
    // Overflow guard: 18 EiB-ish value should be rejected via checked_mul.
    assert!(parse_memory_max("18446744073709551615G").is_err());
}
