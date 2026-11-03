//! Agent-scope attribution of the synchronous reindex behind `memory_observe`.
//!
//! The identity is read from `MCP_CLIENT_NAME`, which is process-global, so
//! this binary holds exactly one test: nothing else in it may observe the
//! variable in a different state. The store-level contract (a row tagged for
//! one agent is invisible to another) is covered without any environment in
//! `tests/tier_b_test.rs::reindex_file_tags_the_row_with_the_observing_agent`.

use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::service::MemoryService;

/// An observed memory must be recallable by the observing agent *immediately*
/// — before the notify watcher's ~200 ms debounce — and must not leak to other
/// agents. Both halves broke when the synchronous upsert hard-coded
/// `agent_id = None`: the row landed unscoped, so `isolated:<agent>` (which
/// matches on `agent_id = ?`) missed it while `filter:<agent>` (which also
/// returns unscoped rows) served it to everybody else.
#[test]
fn observed_memory_is_immediately_visible_to_its_isolated_agent() {
    // SAFETY: set once, before any service or watcher thread exists, in a
    // binary that runs no other test.
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "alpha") };

    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();

    let path = svc
        .memory_observe("the deploy codename is zephyr", None, None)
        .unwrap();

    // No wait_for_index here on purpose: the synchronous upsert inside
    // `memory_observe` is what makes the note searchable now.
    let own_hits = svc
        .memory_search("zephyr", 5, Some("bm25"), None, Some("isolated:alpha"))
        .unwrap();
    let own: Vec<&str> = own_hits.iter().map(|h| h.path.as_str()).collect();
    assert!(
        own.contains(&path.as_str()),
        "isolated:alpha cannot see the note it just observed: {own:?}"
    );

    let leaked_hits = svc
        .memory_search("zephyr", 5, Some("bm25"), None, Some("filter:beta"))
        .unwrap();
    let leaked: Vec<&str> = leaked_hits.iter().map(|h| h.path.as_str()).collect();
    assert!(
        !leaked.contains(&path.as_str()),
        "filter:beta sees alpha's observation: {leaked:?}"
    );
}
