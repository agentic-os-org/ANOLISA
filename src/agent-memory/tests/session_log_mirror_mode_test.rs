//! Mode tests for the persistent session-log mirror.
//!
//! `SessionLogService` creates the in-session root 0700 because it holds
//! `meta.toml` (owner/agent/mount) and `log.jsonl` (per-tool-call path,
//! bytes, errors). The mirror of that log under
//! `<mount>/.anolisa/session-logs/<sid>.jsonl` has to carry the same
//! owner-only protection: it survives the session and lives in a tree whose
//! ancestors may well be traversable.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use tempfile::tempdir;

use agent_memory::audit::AuditEntry;
use agent_memory::session::{SessionId, SessionLogService};

/// Run with a zero umask so every mode asserted below has to come from an
/// explicit mode argument or chmod, not from the ambient umask.
fn permissive_umask() {
    unsafe { nix::libc::umask(0o000) };
}

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn session_log_mirror_is_owner_only() {
    permissive_umask();

    let base = tempdir().unwrap();
    let mount = tempdir().unwrap();
    let mirror = mount.path().join(".anolisa").join("session-logs");
    let sid = SessionId::from_string("ses_mode_probe").unwrap();

    let svc = SessionLogService::start(
        base.path(),
        sid,
        "alice",
        Some("alpha"),
        "user-alice",
        Some(&mirror),
    )
    .unwrap();
    svc.append_log(AuditEntry::new("mem_write").path("secret.md").bytes(7))
        .unwrap();

    let log = mirror.join("ses_mode_probe.jsonl");
    assert_eq!(mode(&mirror), 0o700, "mirror dir must be owner-only");
    assert_eq!(
        mode(&log),
        0o600,
        "mirror session log is readable beyond its owner: {:o}",
        mode(&log)
    );
    assert_eq!(
        mode(svc.root()),
        0o700,
        "the in-session root must stay owner-only"
    );
    assert!(
        fs::read_to_string(&log).unwrap().contains("secret.md"),
        "the entry must still be mirrored"
    );
}

#[test]
fn session_log_mirror_tightens_a_world_readable_mirror() {
    permissive_umask();

    let base = tempdir().unwrap();
    let mount = tempdir().unwrap();
    let mirror = mount.path().join(".anolisa").join("session-logs");
    // A mirror left behind by an older version: 0755 dir, 0644 log.
    fs::create_dir_all(&mirror).unwrap();
    fs::set_permissions(&mirror, PermissionsExt::from_mode(0o755)).unwrap();
    let log = mirror.join("ses_legacy.jsonl");
    fs::write(&log, "{\"tool\":\"mem_write\"}\n").unwrap();
    fs::set_permissions(&log, PermissionsExt::from_mode(0o644)).unwrap();

    let svc = SessionLogService::start(
        base.path(),
        SessionId::from_string("ses_legacy").unwrap(),
        "alice",
        Some("alpha"),
        "user-alice",
        Some(&mirror),
    )
    .unwrap();
    svc.append_log(AuditEntry::new("mem_read").path("secret.md").bytes(3))
        .unwrap();

    assert_eq!(
        mode(&mirror),
        0o700,
        "an existing mirror dir must be tightened"
    );
    assert_eq!(
        mode(&log),
        0o600,
        "an existing mirror log must be tightened, got {:o}",
        mode(&log)
    );

    let body = fs::read_to_string(&log).unwrap();
    assert!(
        body.starts_with("{\"tool\":\"mem_write\"}"),
        "the mirrored history must be kept: {body}"
    );
    assert!(body.contains("mem_read"), "the new entry must land: {body}");
}
