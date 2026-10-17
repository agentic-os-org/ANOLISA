//! Sandbox tests for the `memory_sessions` summary listing.
//!
//! `facts/summary/` lives inside the model-writable memory mount, so an
//! entry there can be a symlink pointing anywhere the server process can
//! read. `memory_sessions` is a read-only listing tool and must never
//! follow one: doing so discloses the link target's body to the model
//! through the session's JSON `description` field. These tests pin the
//! guarantee that every summary open is anchored on the mount's `root_fd`
//! with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`, so a planted link is
//! skipped instead of read.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use agent_memory::config::{AppConfig, Profile};
use agent_memory::service::MemoryService;
use agent_memory::tools::session_history::memory_sessions;

/// Content of the file outside the mount that a planted link points at. If
/// it ever shows up in a tool result, the summary read followed a symlink.
const VICTIM_BODY: &str = "SECRET-OUTSIDE-MOUNT-DATA";

/// Build a service over a throwaway base dir plus a second tempdir that
/// stands in for "outside the mount".
fn setup() -> (tempfile::TempDir, tempfile::TempDir, MemoryService) {
    let base = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.profile = Profile::Advanced;
    cfg.memory.paths.base_dir = base.path().to_string_lossy().into();
    cfg.memory.session.base_dir = base.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    // Hermetic: this test only exercises the summary listing path, so the
    // index watcher and git auto-commit are kept off.
    cfg.memory.index.enabled = false;
    cfg.memory.git.enabled = false;
    let svc = MemoryService::new(cfg).unwrap();
    (base, outside, svc)
}

/// The summary directory, as the consolidation writer leaves it.
fn summary_dir(svc: &MemoryService) -> PathBuf {
    let dir = svc.mount.root.join("facts").join("summary");
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn plant_summary(dir: &Path, name: &str, session_id: &str, body: &str) {
    fs::write(
        dir.join(name),
        format!("---\nsession_id: {session_id}\ncreated_at: 2026-10-04T08:00:00Z\n---\n\n{body}\n"),
    )
    .unwrap();
}

/// Session ids in the order the tool returned them.
fn session_ids(out: &str) -> Vec<String> {
    let parsed: serde_json::Value = serde_json::from_str(out)
        .unwrap_or_else(|e| panic!("memory_sessions returned non-JSON ({e}): {out}"));
    parsed
        .as_array()
        .unwrap_or_else(|| panic!("expected a JSON array of sessions: {out}"))
        .iter()
        .map(|s| s["session_id"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn session_history_reads_summary_through_a_symlink() {
    let (_base, outside, svc) = setup();
    let dir = summary_dir(&svc);

    // The victim sits outside the mount: only a symlink-following read can
    // reach it.
    let victim = outside.path().join("id_rsa.md");
    fs::write(&victim, VICTIM_BODY).unwrap();

    plant_summary(
        &dir,
        "ses_ok.md",
        "ses_ok",
        "Session had 3 tool calls.\ntools: mem_read, mem_write",
    );
    symlink(&victim, dir.join("ses_leak.md")).unwrap();

    let out = memory_sessions(&svc, 10).unwrap();

    assert!(
        !out.contains(VICTIM_BODY),
        "memory_sessions leaked the symlink target's body: {out}"
    );
    // The planted entry is refused rather than read, so it is not listed
    // either; the regular summary is unaffected.
    assert_eq!(
        session_ids(&out),
        vec!["ses_ok"],
        "unexpected listing: {out}"
    );
    assert_eq!(
        fs::read_to_string(&victim).unwrap(),
        VICTIM_BODY,
        "the victim outside the mount must be untouched"
    );
}

#[test]
fn session_history_skips_every_non_regular_summary_entry() {
    let (_base, _outside, svc) = setup();
    let dir = summary_dir(&svc);

    plant_summary(
        &dir,
        "ses_ok.md",
        "ses_ok",
        "Session had 1 tool calls.\ntools: mem_read",
    );
    // Dangling link: nothing to leak, but it must not turn into an error.
    symlink(dir.join("nowhere.md"), dir.join("ses_dangling.md")).unwrap();
    // Link to a regular summary inside the mount. RESOLVE_NO_SYMLINKS
    // refuses every link, so the entry is skipped instead of surfacing the
    // same session a second time under a different name.
    symlink(dir.join("ses_ok.md"), dir.join("ses_alias.md")).unwrap();
    // A directory wearing a `.md` name is not a summary either.
    fs::create_dir(dir.join("ses_dir.md")).unwrap();

    let out = memory_sessions(&svc, 10).unwrap();

    assert_eq!(
        session_ids(&out),
        vec!["ses_ok"],
        "unexpected listing: {out}"
    );
}

#[test]
fn session_history_still_lists_regular_summaries() {
    let (_base, _outside, svc) = setup();
    let dir = summary_dir(&svc);

    plant_summary(
        &dir,
        "ses_a.md",
        "ses_a",
        "Session had 7 tool calls.\ntools: mem_read, mem_write\nfiles: notes/a.md",
    );
    plant_summary(&dir, "ses_b.md", "ses_b", "Session had 2 tool calls.");

    let out = memory_sessions(&svc, 10).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    let sessions = parsed.as_array().unwrap();

    assert_eq!(sessions.len(), 2, "expected both summaries: {out}");
    let a = sessions
        .iter()
        .find(|s| s["session_id"] == "ses_a")
        .unwrap_or_else(|| panic!("ses_a missing: {out}"));
    assert_eq!(a["created_at"], "2026-10-04T08:00:00Z");
    assert_eq!(a["tool_calls"], 7);
    assert_eq!(
        a["tools_used"],
        serde_json::json!(["mem_read", "mem_write"])
    );
    assert_eq!(a["files_modified"], serde_json::json!(["notes/a.md"]));
    assert!(
        a["description"].as_str().unwrap().contains("7 tool calls"),
        "description lost the summary body: {out}"
    );
}
