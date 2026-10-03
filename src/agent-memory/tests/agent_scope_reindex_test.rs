//! Regression: the synchronous reindex performed by `memory_observe` must
//! attribute the new file to the observing agent.
//!
//! The notify watcher's flush path tags rows with `MCP_CLIENT_NAME`, but
//! `IndexHandle::reindex_file` (the synchronous, "immediately searchable"
//! path) hard-codes `agent_id = None`. With `agent_scope=isolated` that
//! makes the observer's own fresh note invisible to it (the exact gap the
//! synchronous reindex exists to close), and with `filter` scope it makes
//! the row visible to *every* agent until the watcher's ~200 ms debounce
//! flush happens to tag it.

use agent_memory::config::AppConfig;
use agent_memory::mount::MountStrategyKind;
use agent_memory::service::MemoryService;
use tempfile::tempdir;

#[test]
fn observed_memory_is_immediately_visible_to_its_isolated_agent() {
    // Agent identity for this process, matching what the MCP server sees.
    // SAFETY: this test binary runs a single test, so no other thread reads
    // the environment concurrently.
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "alpha") };

    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();

    let path = svc
        .memory_observe("the deploy codename is zephyr", None, None)
        .unwrap();
    assert!(path.starts_with("notes/observed/"), "got {path}");

    // Search immediately — far inside the watcher's 200 ms debounce. The
    // synchronous reindex in memory_observe is what makes this possible; for
    // an isolated agent it must carry the agent's identity.
    let hits = svc
        .memory_search("zephyr", 5, Some("bm25"), None, Some("isolated:alpha"))
        .unwrap();
    let paths: Vec<String> = hits.iter().map(|h| h.path.clone()).collect();

    // ...and it must not be readable by a different agent under `filter`.
    let leaked = svc
        .memory_search("zephyr", 5, Some("bm25"), None, Some("filter:beta"))
        .unwrap();
    let leaked_paths: Vec<String> = leaked.iter().map(|h| h.path.clone()).collect();

    assert_eq!(
        paths,
        vec![path.clone()],
        "isolated:alpha cannot see the note it just observed: {paths:?}"
    );
    assert!(
        leaked_paths.is_empty(),
        "alpha's fresh observation leaked to beta: {leaked_paths:?}"
    );
}
