//! Regression for the sanitized-marker predicate (PR #7928 review).
//!
//! Rows written before audit sanitization landed recorded the search query
//! itself, and a query may contain colons: `bm25:packet:len=14` recorded
//! `packet:len=14`. Matching the segment after the *last* colon classified such
//! a row as a `<mode>:len=<bytes>` marker, so both readers that mine queries out
//! of the log dropped real evidence. These tests drive the two production entry
//! points — `MemoryService::consolidate()` and `synthesize_profile()` — over a
//! mix of legacy rows and genuine markers.

use serde_json::json;
use tempfile::{TempDir, tempdir};

use agent_memory::audit::{AuditEntry, sanitized_query_path};
use agent_memory::config::AppConfig;
use agent_memory::service::MemoryService;
use agent_memory::tools::user_profile::synthesize_profile;

/// A recorded legacy query whose tail looks exactly like a length marker.
const LEGACY_PATH: &str = "bm25:packet:len=14";
/// The query that row recorded, and the interest fact it must still produce.
const LEGACY_QUERY: &str = "packet:len=14";

fn setup_service() -> (TempDir, TempDir, MemoryService) {
    let store_tmp = tempdir().unwrap();
    let session_tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "alice".into();
    cfg.memory.paths.base_dir = store_tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = session_tmp.path().to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    // Hermetic: no index worker (consolidation then skips conflict detection)
    // and no git auto-commit.
    cfg.memory.index.enabled = false;
    cfg.memory.git.enabled = false;
    cfg.memory.consolidation.enabled = true;
    let svc = MemoryService::new(cfg).unwrap();
    (store_tmp, session_tmp, svc)
}

/// Titles of every fact `consolidate()` persisted to `facts/facts.jsonl`.
fn written_fact_titles(svc: &MemoryService) -> Vec<String> {
    let jsonl = svc.mount.root.join("facts").join("facts.jsonl");
    let content =
        std::fs::read_to_string(&jsonl).unwrap_or_else(|e| panic!("read {}: {e}", jsonl.display()));
    content
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .unwrap_or_else(|e| panic!("parse fact line {l:?}: {e}"))["title"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

#[test]
fn consolidate_keeps_legacy_queries_that_end_in_a_length_marker() {
    let (_store, _sessions, svc) = setup_service();
    let session = svc.session.as_ref().expect("session log should start");

    // Three legacy rows recording `packet:len=14` (consolidation needs at least
    // `min_tool_calls` entries), plus one genuine marker from today's writer.
    for _ in 0..3 {
        session
            .append_log(AuditEntry::new("memory_search").path(LEGACY_PATH))
            .unwrap();
    }
    session
        .append_log(
            AuditEntry::new("memory_search").path(sanitized_query_path("bm25", "quantum lattice")),
        )
        .unwrap();
    assert_eq!(
        session.read_log().unwrap().lines().count(),
        4,
        "the session log should hold all four rows"
    );

    let written = svc.consolidate();
    assert!(written > 0, "consolidation should have written facts");

    let titles = written_fact_titles(&svc);
    assert!(
        titles.iter().any(|t| t == &format!("搜索: {LEGACY_QUERY}")),
        "the recorded legacy query must survive consolidation: {titles:?}"
    );
    assert!(
        !titles.iter().any(|t| t.contains("len=15")),
        "a sanitized marker carries no query and must not become a fact: {titles:?}"
    );
}

#[test]
fn synthesize_profile_keeps_legacy_queries_that_end_in_a_length_marker() {
    let (_store, _sessions, svc) = setup_service();

    // The same rows as they sit in `.anolisa/session-logs/`, which is what
    // profile synthesis scans.
    let logs_dir = svc.mount.meta_dir.join("session-logs");
    std::fs::create_dir_all(&logs_dir).unwrap();
    let row = |path: &str| {
        json!({
            "ts": "2026-10-04T08:00:00Z",
            "tool": "memory_search",
            "path": path,
            "ok": true,
        })
        .to_string()
    };
    let log = [
        row(LEGACY_PATH),
        row(LEGACY_PATH),
        row(LEGACY_PATH),
        row(&sanitized_query_path("bm25", "quantum lattice")),
        row(&sanitized_query_path(
            &agent_memory::audit::fallback_query_mode("hybrid"),
            "kernel",
        )),
    ]
    .join("\n");
    std::fs::write(logs_dir.join("session-legacy.jsonl"), log + "\n").unwrap();

    let profile = synthesize_profile(&svc).unwrap();
    let topics: Vec<&str> = profile
        .context
        .iter()
        .map(|e| e.description.as_str())
        .collect();

    assert!(
        topics.contains(&"interested in: packet"),
        "the recorded legacy query must still synthesize its topic: {topics:?}"
    );
    assert!(
        !topics.iter().any(|t| t.contains("len=")),
        "a sanitized marker must not synthesize a topic: {topics:?}"
    );
}
