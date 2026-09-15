//! Phase 3: SessionLogService + mem_promote + mem_session_log integration tests.

use tempfile::tempdir;

use agent_memory::audit::AuditEntry;
use agent_memory::config::AppConfig;
use agent_memory::error::MemoryError;
use agent_memory::service::MemoryService;
use agent_memory::session::{EndAction, SessionId, SessionLogService};

fn setup_service() -> (tempfile::TempDir, tempfile::TempDir, MemoryService) {
    let store_tmp = tempdir().unwrap();
    let session_tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "alice".into();
    cfg.memory.paths.base_dir = store_tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = session_tmp.path().to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();
    (store_tmp, session_tmp, svc)
}

// ---------- SessionLogService unit-style ----------

#[test]
fn session_starts_with_meta_and_scratch() {
    let tmp = tempdir().unwrap();
    let svc = SessionLogService::start(
        tmp.path(),
        SessionId::from_string("ses_x").unwrap(),
        "alice",
        Some("test"),
        "user-alice",
        None,
    )
    .unwrap();

    assert!(svc.root().exists());
    assert!(svc.scratch_root().exists());
    assert!(svc.log_path().exists());
    assert!(svc.root().join("meta.toml").exists());

    let meta = std::fs::read_to_string(svc.root().join("meta.toml")).unwrap();
    assert!(meta.contains("ses_x"));
    assert!(meta.contains("alice"));
    assert!(meta.contains("user-alice"));
}

#[test]
fn append_and_read_log_roundtrips() {
    let tmp = tempdir().unwrap();
    let svc = SessionLogService::start(
        tmp.path(),
        SessionId::from_string("ses_log").unwrap(),
        "alice",
        None,
        "user-alice",
        None,
    )
    .unwrap();

    svc.append_log(AuditEntry::new("mem_write").path("a.md").bytes(10))
        .unwrap();
    svc.append_log(AuditEntry::new("mem_read").path("a.md").bytes(10))
        .unwrap();

    let log = svc.read_log().unwrap();
    let lines: Vec<&str> = log.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2);

    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(v["tool"], "mem_write");
    assert_eq!(v["path"], "a.md");
}

#[test]
fn end_discard_removes_dir() {
    let tmp = tempdir().unwrap();
    let svc = SessionLogService::start(
        tmp.path(),
        SessionId::from_string("ses_d").unwrap(),
        "alice",
        None,
        "user-alice",
        None,
    )
    .unwrap();
    let root = svc.root().to_path_buf();
    assert!(root.exists());
    svc.end(EndAction::Discard).unwrap();
    assert!(!root.exists());
}

#[test]
fn meta_toml_escapes_special_chars() {
    // Regression for M4: pre-fix `start()` interpolated owner_user_id
    // via format!, so values containing `"` or `\n` produced invalid TOML
    // (parse failure on next startup) or injected keys.
    let tmp = tempdir().unwrap();
    let svc = SessionLogService::start(
        tmp.path(),
        SessionId::from_string("ses_meta_esc").unwrap(),
        "alice\"\nrogue_key = \"injected",
        Some("agent\\with\"quotes"),
        "user-alice",
        None,
    )
    .unwrap();
    let meta_text = std::fs::read_to_string(svc.root().join("meta.toml")).unwrap();
    let parsed: toml::Value =
        toml::from_str(&meta_text).expect("meta.toml must be valid TOML even with hostile input");
    let table = parsed.as_table().unwrap();
    assert_eq!(
        table.get("owner_user_id").and_then(|v| v.as_str()),
        Some("alice\"\nrogue_key = \"injected")
    );
    // Injected key must not be a top-level field.
    assert!(table.get("rogue_key").is_none());
}

#[test]
fn end_keep_preserves_dir() {
    let tmp = tempdir().unwrap();
    let svc = SessionLogService::start(
        tmp.path(),
        SessionId::from_string("ses_k").unwrap(),
        "alice",
        None,
        "user-alice",
        None,
    )
    .unwrap();
    let root = svc.root().to_path_buf();
    svc.end(EndAction::Keep).unwrap();
    assert!(root.exists());
}

// ---------- mem_promote integration ----------

#[test]
fn promote_copies_scratch_to_store() {
    let (_store_tmp, _session_tmp, svc) = setup_service();
    let session = svc.session.as_ref().expect("session ready");

    // Simulate the model writing to scratch directly (P3 test fixture: write
    // through SessionLogService::scratch_root)
    let src = session.scratch_root().join("draft.md");
    std::fs::write(&src, "hello from session").unwrap();

    let n = svc.promote("draft.md", "notes/promoted.md").unwrap();
    assert_eq!(n, "hello from session".len() as u64);

    // File now visible in store
    let body = svc.read("notes/promoted.md").unwrap();
    assert_eq!(body, "hello from session");
}

#[test]
fn promote_rejects_outside_scratch() {
    let (_store_tmp, _session_tmp, svc) = setup_service();
    let err = svc.promote("../meta.toml", "notes/x.md").unwrap_err();
    assert!(matches!(err, MemoryError::PathOutsideMount(_)));
}

#[test]
fn promote_rejects_existing_store_file() {
    let (_store_tmp, _session_tmp, svc) = setup_service();
    let session = svc.session.as_ref().unwrap();

    let src = session.scratch_root().join("a.md");
    std::fs::write(&src, "x").unwrap();

    svc.write("dst.md", "existing", false).unwrap();
    let err = svc.promote("a.md", "dst.md").unwrap_err();
    assert!(matches!(err, MemoryError::AlreadyExists(_)));
}

#[test]
fn promote_missing_scratch_file_returns_not_found() {
    let (_store_tmp, _session_tmp, svc) = setup_service();
    let err = svc.promote("nope.md", "x.md").unwrap_err();
    assert!(matches!(err, MemoryError::NotFound(_)));
}

// ---------- mem_session_log integration ----------

#[test]
fn session_log_returns_jsonl_of_calls() {
    let (_store_tmp, _session_tmp, svc) = setup_service();

    svc.write("a.md", "hello", false).unwrap();
    svc.read("a.md").unwrap();

    let log = svc.session_log().unwrap();
    assert!(log.contains("\"tool\":\"mem_write\""));
    assert!(log.contains("\"tool\":\"mem_read\""));
    assert!(log.contains("\"path\":\"a.md\""));
}

#[test]
fn session_log_includes_promote_and_prior_session_log_call() {
    let (_store_tmp, _session_tmp, svc) = setup_service();
    let session = svc.session.as_ref().unwrap();
    std::fs::write(session.scratch_root().join("a.md"), "x").unwrap();
    svc.promote("a.md", "p.md").unwrap();

    // First call audits itself AFTER reading; the read() snapshot can't see its own audit.
    let _first = svc.session_log().unwrap();
    // Second call's snapshot DOES contain the first call's audit row.
    let log = svc.session_log().unwrap();
    assert!(
        log.contains("\"tool\":\"mem_promote\""),
        "missing promote: {log}"
    );
    assert!(
        log.contains("\"tool\":\"mem_session_log\""),
        "missing prior session_log: {log}"
    );
}

#[test]
fn unusable_session_dir_falls_back_instead_of_losing_the_session() {
    // Make the session base dir a regular file so create_dir_all can never
    // succeed — the same failure a non-root server gets from the shipped
    // default /run/anolisa/sessions, whose parent the RPM creates 0700
    // root:root and which make install / containers do not create at all
    // (/run is drwxr-xr-x root root).
    //
    // The service must still build, and the session must now be recovered
    // from a per-user fallback instead of being lost: before the fallback
    // this degraded to `session == None` behind a single warn!, so every
    // mem_promote / mem_session_log call errored on a stock install.
    let store_tmp = tempdir().unwrap();
    let blocker = tempdir().unwrap();
    let blocking_file = blocker.path().join("not-a-dir");
    std::fs::write(&blocking_file, "").unwrap();

    let mut cfg = AppConfig::default();
    cfg.global.user_id = "carol".into();
    cfg.memory.paths.base_dir = store_tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = blocking_file.to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;

    let svc = MemoryService::new(cfg).expect("service should still build");
    let session = svc
        .session
        .as_ref()
        .expect("session must be recovered from a fallback dir, not lost");
    assert!(
        !session.root().starts_with(blocker.path()),
        "session root {} must be outside the unusable configured path",
        session.root().display()
    );

    // Both session-dependent tools work end to end through the fallback.
    std::fs::write(session.scratch_root().join("draft.md"), "fallback scratch").unwrap();
    let n = svc.promote("draft.md", "notes/from-fallback.md").unwrap();
    assert!(n > 0);
    assert!(svc.mount.root.join("notes/from-fallback.md").exists());
    svc.session_log().unwrap();

    let _ = std::fs::remove_dir_all(session.root());
}

// ---------- audit double-write ----------

#[test]
fn audit_log_is_mirrored_to_session() {
    let (_store_tmp, _session_tmp, svc) = setup_service();
    svc.write("a.md", "hello", false).unwrap();

    // Both store audit and session log should contain the write
    let store_audit = std::fs::read_to_string(svc.mount.audit_log_path()).unwrap();
    let session_log = svc.session_log().unwrap();
    assert!(store_audit.contains("\"tool\":\"mem_write\""));
    assert!(session_log.contains("\"tool\":\"mem_write\""));
}
