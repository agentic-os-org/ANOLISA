//! Regression: the persistent session-log mirror must not be world-readable.
//!
//! `SessionLogService::start_in` forces the in-session root to 0700 because it
//! holds `meta.toml` and `log.jsonl` (per-tool-call path/bytes/errors), but the
//! mirror copy it writes under `<mount>/.anolisa/session-logs/` was opened with
//! default permissions (0666 & ~umask = 0644) inside a default-mode (0755)
//! directory, so any local user who can traverse the mount's ancestors could
//! read the same tool-call history.

use std::os::unix::fs::PermissionsExt;

use agent_memory::audit::AuditEntry;
use agent_memory::session::{SessionId, SessionLogService};

fn mode_of(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn session_log_mirror_is_owner_only() {
    // Simulate the default umask (022) explicitly.
    unsafe { nix::libc::umask(0o022) };

    let tmp = tempfile::tempdir().unwrap();
    let mirror = tmp.path().join("mount/.anolisa/session-logs");

    let svc = SessionLogService::start(
        tmp.path(),
        SessionId::from_string("ses_mode_probe").unwrap(),
        "alice",
        Some("alpha"),
        "user-alice",
        Some(&mirror),
    )
    .unwrap();

    // The mirror must actually receive the log entries.
    svc.append_log(AuditEntry::new("mem_write").path("secret.md").bytes(7))
        .unwrap();

    let mirror_file = mirror.join("ses_mode_probe.jsonl");
    let content = std::fs::read_to_string(&mirror_file).unwrap();

    eprintln!("mirror file mode : {:o}", mode_of(&mirror_file));
    eprintln!("mirror dir mode  : {:o}", mode_of(&mirror));
    eprintln!("session root mode: {:o}", mode_of(svc.root()));

    assert!(
        content.contains("secret.md"),
        "mirror did not receive the log entry: {content:?}"
    );
    assert_eq!(
        mode_of(svc.root()),
        0o700,
        "control: the in-session root is owner-only"
    );
    assert_eq!(
        mode_of(&mirror),
        0o700,
        "mirror session-log directory is not owner-only"
    );
    assert_eq!(
        mode_of(&mirror_file),
        0o600,
        "mirror session log is world-readable"
    );
}

#[test]
fn session_log_mirror_tightens_preexisting_world_readable_file() {
    unsafe { nix::libc::umask(0o022) };

    let tmp = tempfile::tempdir().unwrap();
    let mirror = tmp.path().join("mount/.anolisa/session-logs");
    // Simulate a mirror directory and file created by an older version with
    // default permissions, already containing a session's history.
    std::fs::create_dir_all(&mirror).unwrap();
    std::fs::set_permissions(&mirror, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mirror_file = mirror.join("ses_legacy_probe.jsonl");
    std::fs::write(
        &mirror_file,
        "{\"tool\":\"mem_read\",\"path\":\"kept.md\"}\n",
    )
    .unwrap();
    std::fs::set_permissions(&mirror_file, std::fs::Permissions::from_mode(0o644)).unwrap();

    let svc = SessionLogService::start(
        tmp.path(),
        SessionId::from_string("ses_legacy_probe").unwrap(),
        "alice",
        Some("alpha"),
        "user-alice",
        Some(&mirror),
    )
    .unwrap();
    svc.append_log(AuditEntry::new("mem_write").path("new.md").bytes(3))
        .unwrap();

    let content = std::fs::read_to_string(&mirror_file).unwrap();
    assert!(
        content.contains("kept.md"),
        "existing mirror history must be preserved (append mode): {content:?}"
    );
    assert!(
        content.contains("new.md"),
        "new entries must still reach the mirror: {content:?}"
    );
    assert_eq!(
        mode_of(&mirror),
        0o700,
        "pre-existing mirror directory was not tightened"
    );
    assert_eq!(
        mode_of(&mirror_file),
        0o600,
        "pre-existing world-readable mirror file was not tightened"
    );
}
