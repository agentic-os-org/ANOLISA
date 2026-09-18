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
    // `end` consumes the service, which closes the descriptor the anchored
    // `root()` path is built on — assert on the operator-facing pathname.
    let root = svc.display_root().to_path_buf();
    assert!(root.exists());
    assert!(svc.root().exists());
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
    let root = svc.display_root().to_path_buf();
    svc.end(EndAction::Keep).unwrap();
    assert!(root.exists());
    assert!(root.join("meta.toml").exists());
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

// ---------- session-dir fallback ----------
//
// This case runs in a child process, and that is not stylistic. The fallback
// chain is built from the *inherited* `XDG_RUNTIME_DIR` / `TMPDIR`, and
// `start_session` reads the inherited `MEMORY_SESSION_ID`, so exercising it
// in-process would probe — and chmod `0700` — the real per-user runtime dir,
// and, if the id it happened to inherit already had a session there, reopen
// that live session, write `scratch/draft.md` into it and let cleanup
// recursively delete the lot, unrelated scratch data included. The child
// gets a runtime dir, a tmp dir and a session id that are all this test's
// own, and the parent plants a decoy session next to it so the assertion
// covers what cleanup is allowed to reach.

const FALLBACK_CHILD_ENV: &str = "ANOLISA_TEST_SESSION_FALLBACK_CHILD";
const FALLBACK_CHILD_STORE: &str = "ANOLISA_TEST_STORE";
const FALLBACK_CHILD_BLOCKER: &str = "ANOLISA_TEST_BLOCKER";
const FALLBACK_CHILD_BASE: &str = "ANOLISA_TEST_FALLBACK_BASE";
const FALLBACK_SID: &str = "ses_fallback_child";
const DECOY_SID: &str = "ses_fallback_decoy";

#[test]
fn unusable_session_dir_falls_back_instead_of_losing_the_session() {
    if std::env::var_os(FALLBACK_CHILD_ENV).is_some() {
        fallback_child();
        return;
    }
    fallback_parent();
}

fn fallback_parent() {
    use std::os::unix::fs::PermissionsExt;

    // Every directory the child can reach is one we hand it.
    let runtime = tempdir().unwrap();
    let child_tmp = tempdir().unwrap();
    let store = tempdir().unwrap();
    let blocker = tempdir().unwrap();

    // Make the configured session base a regular file so `create_dir_all` can
    // never succeed — the same failure a non-root server gets from the
    // shipped default /run/anolisa/sessions, whose parent the RPM creates
    // 0700 root:root and which make install / containers do not create at
    // all (/run is drwxr-xr-x root root).
    let blocking_file = blocker.path().join("not-a-dir");
    std::fs::write(&blocking_file, b"").unwrap();

    // A live session already sitting in the fallback base. The child must
    // neither write into it nor let its cleanup reach it.
    let fallback_base = runtime.path().join("anolisa").join("sessions");
    let decoy_root = fallback_base.join(DECOY_SID);
    std::fs::create_dir_all(decoy_root.join("scratch")).unwrap();
    for d in [&fallback_base, &decoy_root] {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let decoy_file = decoy_root.join("scratch").join("keep.md");
    std::fs::write(&decoy_file, b"someone else's session").unwrap();

    let exe = std::env::current_exe().expect("path to this test binary");
    let out = std::process::Command::new(exe)
        .arg("--exact")
        .arg("unusable_session_dir_falls_back_instead_of_losing_the_session")
        .arg("--nocapture")
        .env(FALLBACK_CHILD_ENV, "1")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("TMPDIR", child_tmp.path())
        .env("MEMORY_SESSION_ID", FALLBACK_SID)
        .env(FALLBACK_CHILD_STORE, store.path())
        .env(FALLBACK_CHILD_BLOCKER, &blocking_file)
        .env(FALLBACK_CHILD_BASE, &fallback_base)
        .output()
        .expect("spawn the isolated child");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "child exited with {:?}\n--- stdout ---\n{stdout}--- stderr ---\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("fallback child ok:"),
        "the child body did not run to completion\n--- stdout ---\n{stdout}--- stderr ---\n{stderr}"
    );

    // The child's cleanup was scoped to its own session id.
    assert!(
        !fallback_base.join(FALLBACK_SID).exists(),
        "the child's own session must be gone after Discard"
    );
    assert_eq!(
        std::fs::read(&decoy_file).unwrap(),
        b"someone else's session",
        "cleanup must not reach a live session sharing the fallback base"
    );

    // The fallback it landed on was the runtime dir we gave it, so the tmp
    // candidate was never needed — and, because both were test-owned, the
    // real per-user directories were never probed or chmod'ed at all.
    let leaked: Vec<String> = std::fs::read_dir(child_tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("anolisa-sessions-"))
        .collect();
    assert!(
        leaked.is_empty(),
        "the tmp fallback must not be reached once the runtime dir works: {leaked:?}"
    );
}

fn fallback_child() {
    use std::path::Path;

    let store = std::env::var(FALLBACK_CHILD_STORE).expect("store dir");
    let blocker = std::env::var(FALLBACK_CHILD_BLOCKER).expect("blocking file");
    let fallback_base = std::env::var(FALLBACK_CHILD_BASE).expect("fallback base");
    let sid = std::env::var("MEMORY_SESSION_ID").expect("session id");

    let mut cfg = AppConfig::default();
    cfg.global.user_id = "carol".into();
    cfg.memory.paths.base_dir = store;
    cfg.memory.session.base_dir = blocker;
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;

    let svc = MemoryService::new(cfg).expect("service should still build");

    // Before the fallback existed this degraded to `session == None` behind
    // a single `warn!`, so every `mem_promote` / `mem_session_log` call
    // errored on a stock install.
    let session = svc
        .session
        .as_ref()
        .expect("session must be recovered from a fallback dir, not lost");

    assert_eq!(session.sid().as_str(), sid, "must honour the pinned id");

    let expected = Path::new(&fallback_base).canonicalize().unwrap().join(&sid);
    assert_eq!(
        session.root().canonicalize().unwrap(),
        expected,
        "the fallback must be the test-owned runtime dir"
    );
    assert_eq!(session.display_root().canonicalize().unwrap(), expected);

    // Both session-dependent tools work end to end through the fallback.
    std::fs::write(session.scratch_root().join("draft.md"), b"fallback scratch").unwrap();
    let n = svc.promote("draft.md", "notes/from-fallback.md").unwrap();
    assert!(n > 0);
    assert!(svc.mount.root.join("notes/from-fallback.md").exists());
    svc.session_log().unwrap();

    // Go through the real shutdown path so the parent can inspect exactly
    // what cleanup touched.
    let action = svc.config.memory.session.end_action;
    svc.try_end_session(action);

    // Proof for the parent that this body really ran to completion — a child
    // that silently matched no test would also exit 0.
    println!("fallback child ok: {}", expected.display());
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
