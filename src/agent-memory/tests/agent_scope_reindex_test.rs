//! Regression test for the synchronous reindex in `memory_observe`.
//!
//! `memory_observe` upserts the new note into the BM25 index before
//! returning, so an auto-recall hook firing right after the call finds it
//! (the notify watcher's ~200 ms debounce would otherwise leave a window
//! where the note is invisible — #1462). That upsert must attribute the row
//! to the observing agent (`MCP_CLIENT_NAME`), exactly like the watcher's
//! flush/full-scan paths do; an unscoped (`NULL`) row is invisible under
//! `isolated:<agent>` and shared with every agent under `filter:<agent>`
//! (#4538).
//!
//! This file keeps a single `#[test]`: the identity under test is a
//! process-global environment variable, so parallel cases in one binary
//! would race on it.

use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::index::SearchHit;
use agent_memory::service::MemoryService;

fn setup(agent_scope: &str) -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    // Use a sub-temp for sessions so /run/anolisa isn't required.
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.agent_scope = agent_scope.to_string();
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

fn paths(hits: &[SearchHit]) -> Vec<String> {
    hits.iter().map(|h| h.path.clone()).collect()
}

#[test]
fn observed_memory_is_immediately_visible_to_its_agent_only() {
    // SAFETY: this is the only test in the binary, so no other thread reads
    // or mutates the environment while it is set.
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "alpha") };

    let (_tmp, svc) = setup("isolated");

    let path = svc
        .memory_observe("the deploy codename is zephyr", None, None)
        .unwrap();

    // No sleep: the point of the synchronous reindex is that the note is
    // searchable the moment `memory_observe` returns.

    // The observing agent finds its own note — both under the scope it
    // configured and under an explicit `isolated:<agent>` request.
    let configured = svc
        .memory_search("zephyr", 5, Some("bm25"), None, None)
        .unwrap();
    assert_eq!(
        paths(&configured),
        vec![path.clone()],
        "agent_scope=isolated + MCP_CLIENT_NAME=alpha cannot see the note it just observed"
    );

    let isolated = svc
        .memory_search("zephyr", 5, Some("bm25"), None, Some("isolated:alpha"))
        .unwrap();
    assert_eq!(
        paths(&isolated),
        vec![path.clone()],
        "isolated:alpha cannot see the note it just observed"
    );

    // No other agent may see it: `filter:<agent>` shares only unscoped (NULL)
    // rows, so a hit here means the observation leaked.
    for scope in ["filter:beta", "isolated:beta"] {
        let other = svc
            .memory_search("zephyr", 5, Some("bm25"), None, Some(scope))
            .unwrap();
        assert!(
            !paths(&other).contains(&path),
            "{scope} can see alpha's freshly observed note: {:?}",
            paths(&other)
        );
    }
}
