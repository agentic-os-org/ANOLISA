//! Walk-read sandbox regression tests.
//!
//! Every tool that walks the memory mount (or lists a mount directory) and
//! then reads file bodies must route the read through `safe_fs`
//! (openat2 RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS), the same invariant
//! `memory_get_context` was hardened against. A `*.md` / `*.jsonl` symlink
//! planted inside the mount and pointing outside it must never leak the
//! out-of-mount content through tool output — directly (read_dir based
//! walks follow symlinks) or via the symlink-swap TOCTOU window
//! (std::fs::read_to_string re-resolves absolute walk paths).

use std::os::unix::fs::symlink;

use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_export::{ExportFilter, memory_export};
use agent_memory::tools::memory_index::build_index;
use agent_memory::tools::memory_sovereignty::memory_auto_created;
use agent_memory::tools::memory_summary_tool::memory_summary;
use agent_memory::tools::session_history::memory_timeline;
use agent_memory::tools::user_profile::synthesize_profile;

/// Unique marker placed in files OUTSIDE the mount. If it ever shows up in
/// tool output, a planted symlink exfiltrated out-of-mount content.
const MARKER: &str = "ANOLISA-SYMLINK-EXFIL-7f3a9c";

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.index.enabled = false;
    cfg.memory.git.enabled = false;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

/// Create an outside-the-mount secret file and a same-extension symlink
/// pointing at it from `rel` inside the mount.
fn plant_symlink(svc: &MemoryService, outside: &std::path::Path, rel: &str) {
    let target = outside;
    let link = svc.mount.root.join(rel);
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    symlink(target, &link).unwrap();
}

fn secret_md(outside: &tempfile::TempDir) -> std::path::PathBuf {
    let p = outside.path().join("secret.md");
    std::fs::write(
        &p,
        format!("---\nsource: auto-consolidation\n---\nleaked body {MARKER}\n"),
    )
    .unwrap();
    p
}

// ---------- memory_export (walk based) ----------

#[test]
fn export_excludes_symlink_planted_in_mount() {
    let (_tmp, svc) = setup();
    let outside = tempdir().unwrap();
    let secret = secret_md(&outside);
    plant_symlink(&svc, &secret, "facts/lesson/leak.md");
    svc.write("facts/lesson/real.md", "real memory", false)
        .unwrap();

    let json = memory_export(&svc, &ExportFilter::default()).unwrap();
    assert!(
        json.contains("real.md"),
        "real file should be exported: {json}"
    );
    assert!(
        !json.contains(MARKER),
        "symlinked out-of-mount content leaked into export: {json}"
    );
}

// ---------- memory_summary (walk based) ----------

#[test]
fn summary_excludes_symlink_planted_in_mount() {
    let (_tmp, svc) = setup();
    let outside = tempdir().unwrap();
    let secret = secret_md(&outside);
    plant_symlink(&svc, &secret, "facts/lesson/leak.md");

    let summary = memory_summary(&svc, 10).unwrap();
    let out = serde_json::to_string(&summary).unwrap();
    assert!(
        !out.contains(MARKER),
        "symlinked out-of-mount content leaked into summary: {out}"
    );
}

// ---------- memory_auto_created (walk based) ----------

#[test]
fn auto_created_excludes_symlink_planted_in_mount() {
    let (_tmp, svc) = setup();
    let outside = tempdir().unwrap();
    let secret = secret_md(&outside);
    plant_symlink(&svc, &secret, "facts/lesson/leak.md");

    let out = memory_auto_created(&svc, 10).unwrap();
    assert!(
        !out.contains(MARKER),
        "symlinked out-of-mount content leaked into auto_created listing: {out}"
    );
}

// ---------- MEMORY.md index build (walk based) ----------

#[test]
fn index_build_excludes_symlink_planted_in_mount() {
    let (_tmp, svc) = setup();
    let outside = tempdir().unwrap();
    let secret = secret_md(&outside);
    plant_symlink(&svc, &secret, "facts/lesson/leak.md");
    svc.write("facts/lesson/real.md", "real memory", false)
        .unwrap();

    let entries = build_index(&svc).unwrap();
    let out = entries
        .iter()
        .map(|e| format!("{}|{}|{}", e.title, e.path, e.description))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        out.contains("real.md"),
        "real file should be indexed: {out}"
    );
    assert!(
        !out.contains("leak.md") && !out.contains(MARKER),
        "symlinked file leaked into MEMORY.md index build: {out}"
    );
}

// ---------- memory_timeline (direct meta-dir read — directly vulnerable) ----------
//
// memory_sessions' facts/summary/*.md read has the same weakness but is
// tracked separately in #5000 / PR #5006; not covered here.

#[test]
fn timeline_rejects_symlinked_session_log() {
    let (_tmp, svc) = setup();
    let outside = tempdir().unwrap();
    let log = outside.path().join("secret.jsonl");
    std::fs::write(
        &log,
        format!("{{\"ts\":\"t1\",\"tool\":\"mem_read\",\"path\":\"{MARKER}\",\"ok\":true}}\n"),
    )
    .unwrap();
    plant_symlink(&svc, &log, ".anolisa/session-logs/leak.jsonl");

    // An Err return (symlink rejected outright by safe_fs) is equally
    // acceptable — no leak either way.
    if let Ok(out) = memory_timeline(&svc, "leak", 10) {
        assert!(
            !out.contains(MARKER),
            "symlinked out-of-mount log leaked into memory_timeline: {out}"
        );
    }
}

// ---------- synthesize_profile (read_dir based — directly vulnerable) ----------

#[test]
fn profile_synthesis_skips_symlinked_inputs() {
    let (_tmp, svc) = setup();
    let outside = tempdir().unwrap();

    // facts/<category>/*.md symlink → preferences leak
    let secret = secret_md(&outside);
    plant_symlink(&svc, &secret, "facts/lesson/leak.md");

    // notes/observed/*.md symlink → context leak
    let note = outside.path().join("secret-note.md");
    std::fs::write(
        &note,
        format!("---\nhint: preference\n---\nnote body {MARKER}\n"),
    )
    .unwrap();
    plant_symlink(&svc, &note, "notes/observed/leak.md");

    // .anolisa/session-logs/*.jsonl symlink → tool-frequency leak (≥5 calls
    // pushes "frequently uses <tool>" into preferences)
    let log = outside.path().join("secret.jsonl");
    let lines: String = (0..6)
        .map(|i| format!("{{\"ts\":\"t{i}\",\"tool\":\"{MARKER}\",\"path\":\"x\",\"ok\":true}}\n"))
        .collect();
    std::fs::write(&log, lines).unwrap();
    plant_symlink(&svc, &log, ".anolisa/session-logs/leak.jsonl");

    let profile = synthesize_profile(&svc).unwrap();
    let out = serde_json::to_string(&profile).unwrap();
    assert!(
        !out.contains(MARKER),
        "symlinked out-of-mount content leaked into synthesized profile: {out}"
    );
}
