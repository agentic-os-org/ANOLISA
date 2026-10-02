//! Integration test for the audit logger's optional journald fan-out.
//!
//! Phase 6.5 used to have no regression test outside the no-op stub; this
//! file ensures the on-disk JSONL path stays correct regardless of
//! whether journald is enabled, and that enabling journald never panics
//! on hosts where the journald socket is unreachable (e.g. CI sandboxes
//! without systemd, macOS, or containers without /run/systemd/journal).

use agent_memory::audit::{AuditEntry, AuditLogger};
use tempfile::tempdir;

#[test]
fn disabled_journald_still_writes_jsonl() {
    let tmp = tempdir().unwrap();
    let p = tmp.path().join("audit.log");
    let log = AuditLogger::new(p.clone()).unwrap();

    log.log(AuditEntry::new("mem_write").path("a.md").bytes(7))
        .unwrap();

    let contents = std::fs::read_to_string(&p).unwrap();
    assert_eq!(contents.lines().count(), 1);
    let line = contents.lines().next().unwrap();
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["tool"], "mem_write");
    assert_eq!(v["ok"], true);
    assert_eq!(v["bytes"], 7);
}

#[test]
fn enabled_journald_construction_does_not_panic() {
    // The constructor calls journald::probe() once. If the socket is
    // unreachable (typical in test environments without systemd) we
    // must log a warning and continue, NOT panic — the on-disk log is
    // the source of truth.
    let tmp = tempdir().unwrap();
    let p = tmp.path().join("audit.log");
    let _log = AuditLogger::new_with_journald(p, true).expect("construction should succeed");
}

#[test]
fn enabled_journald_does_not_break_on_log_failure() {
    // Even when the underlying journald send fails, log() must succeed
    // (the on-disk file write is what counts) and must not panic.
    let tmp = tempdir().unwrap();
    let p = tmp.path().join("audit.log");
    let log = AuditLogger::new_with_journald(p.clone(), true).unwrap();

    log.log(AuditEntry::new("mem_read").path("a.md").bytes(0))
        .expect("on-disk log must succeed even if journald is unreachable");
    log.log(AuditEntry::new("mem_grep").path("notes").error("e"))
        .expect("on-disk log must succeed for failure entries too");

    let contents = std::fs::read_to_string(&p).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 2);
    let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"], "e");
}
