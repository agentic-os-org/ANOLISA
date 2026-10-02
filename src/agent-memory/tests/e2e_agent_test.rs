//! E2E agent-driven tests for the agent-memory MCP server.
//!
//! A lightweight MCP Client Agent wraps JSON-RPC handshake + tool calls,
//! then drives all 19 tools through realistic agent usage scenarios.

#[path = "common/mod.rs"]
mod common;

use common::McpAgent;
use serde_json::json;
use std::time::Duration;

// ---- Test 1: Tier A/B + snapshots + sandbox ----

#[tokio::test]
async fn full_e2e_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = McpAgent::spawn(tmp.path(), &[]).await;

    // -- Phase 1: Tier A file ops --

    // mem_mkdir: create directory structure
    let text = agent.call("mem_mkdir", json!({"path": "notes"})).await;
    assert!(text.contains("created"), "mkdir notes: {text}");

    let text = agent.call("mem_mkdir", json!({"path": "strategies"})).await;
    assert!(text.contains("created"), "mkdir strategies: {text}");

    // mem_write: seed two files
    let text = agent
        .call(
            "mem_write",
            json!({"path": "notes/day1.md", "content": "Day 1: learned rust ownership model\n"}),
        )
        .await;
    assert!(text.contains("wrote"), "write day1: {text}");

    let text = agent
        .call(
            "mem_write",
            json!({"path": "strategies/rust-plan.md", "content": "# Rust Plan\nGoal: master ownership\n"}),
        )
        .await;
    assert!(text.contains("wrote"), "write rust-plan: {text}");

    // mem_read: verify day1 content
    let text = agent
        .call("mem_read", json!({"path": "notes/day1.md"}))
        .await;
    assert!(text.contains("ownership"), "read day1: {text}");

    // mem_append: add to day1
    let text = agent
        .call(
            "mem_append",
            json!({"path": "notes/day1.md", "content": "Day 2: practiced borrowing rules"}),
        )
        .await;
    assert!(text.contains("appended"), "append day1: {text}");

    // mem_read again: verify appended content
    let text = agent
        .call("mem_read", json!({"path": "notes/day1.md"}))
        .await;
    assert!(text.contains("borrowing"), "read after append: {text}");

    // mem_edit: replace "Goal" in rust-plan
    let text = agent
        .call(
            "mem_edit",
            json!({"path": "strategies/rust-plan.md", "old_str": "Goal: master ownership", "new_str": "Goal: master lifetimes"}),
        )
        .await;
    assert!(text.contains("edited"), "edit rust-plan: {text}");

    let text = agent
        .call("mem_read", json!({"path": "strategies/rust-plan.md"}))
        .await;
    assert!(
        text.contains("lifetimes") && !text.contains("master ownership"),
        "after edit: {text}"
    );

    // mem_list: verify file tree
    let entries = agent
        .call_json("mem_list", json!({"recursive": true}))
        .await;
    let paths: Vec<&str> = entries
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"notes/day1.md"), "list missing day1");
    assert!(
        paths.contains(&"strategies/rust-plan.md"),
        "list missing rust-plan"
    );
    assert!(paths.contains(&"README.md"), "list missing README");

    // mem_grep: search for "ownership" (only in day1, rust-plan now has "lifetimes")
    let hits = agent
        .call_json("mem_grep", json!({"pattern": "ownership"}))
        .await;
    assert_eq!(hits.as_array().unwrap().len(), 1, "grep ownership hits");
    assert_eq!(hits[0]["path"], "notes/day1.md");

    // mem_diff: compare two files
    let text = agent
        .call(
            "mem_diff",
            json!({"path1": "notes/day1.md", "path2": "strategies/rust-plan.md"}),
        )
        .await;
    assert!(
        text.contains("---") && text.contains("+++"),
        "diff output: {text}"
    );

    // -- Phase 2: Tier B structured search --

    // memory_observe: record an observation
    let text = agent
        .call(
            "memory_observe",
            json!({"content": "noticed that lifetimes prevent dangling pointers", "hint": "rust"}),
        )
        .await;
    assert!(
        text.contains("notes/observed/") && text.contains(".md"),
        "observe path: {text}"
    );

    // Wait for the index worker to catch up (200ms debounce + slack).
    tokio::time::sleep(Duration::from_millis(500)).await;

    // memory_search: BM25 lookup
    let hits = agent
        .call_json("memory_search", json!({"query": "ownership", "top_k": 5}))
        .await;
    let search_paths: Vec<&str> = hits
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["path"].as_str())
        .collect();
    assert!(
        search_paths.contains(&"notes/day1.md"),
        "search should find day1, got: {search_paths:?}"
    );

    // memory_get_context: assemble context preview
    let text = agent
        .call("memory_get_context", json!({"max_tokens": 500}))
        .await;
    assert!(!text.is_empty(), "get_context empty");
    assert!(
        text.contains("rust") || text.contains("ownership"),
        "context content: {text}"
    );

    // -- Phase 3: Tier C snapshots --

    // mem_snapshot: create a named snapshot
    let snap = agent
        .call_json("mem_snapshot", json!({"name": "day2-checkpoint"}))
        .await;
    let snap_id = snap["id"].as_str().unwrap();
    assert!(snap_id.starts_with("snap_"), "snapshot id: {snap_id}");

    // mem_snapshot_list: verify snapshot exists
    let listing = agent.call_json("mem_snapshot_list", json!({})).await;
    let found = listing
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"].as_str() == Some(snap_id));
    assert!(found, "snapshot {snap_id} not in list");

    // Overwrite day1, then restore snapshot to roll back.
    agent
        .call(
            "mem_write",
            json!({"path": "notes/day1.md", "content": "OVERWRITTEN", "overwrite": true}),
        )
        .await;
    let text = agent
        .call("mem_read", json!({"path": "notes/day1.md"}))
        .await;
    assert_eq!(text, "OVERWRITTEN", "before restore: {text}");

    agent
        .call("mem_snapshot_restore", json!({"id": snap_id}))
        .await;
    let text = agent
        .call("mem_read", json!({"path": "notes/day1.md"}))
        .await;
    assert!(
        text.contains("ownership"),
        "after restore should have original content: {text}"
    );

    // -- Phase 5: sandbox & auxiliary --

    // mem_remove: delete a file
    let text = agent
        .call("mem_remove", json!({"path": "strategies/rust-plan.md"}))
        .await;
    assert!(text.contains("removed"), "remove: {text}");

    // Verify file gone
    let text = agent
        .call("mem_read", json!({"path": "strategies/rust-plan.md"}))
        .await;
    assert!(
        text.contains("not found") || text.contains("failed"),
        "removed file should be gone: {text}"
    );

    // mem_session_log: verify session log records tool calls
    let text = agent.call("mem_session_log", json!({})).await;
    assert!(
        text.contains("mem_write") || text.contains("mem_read"),
        "session log: {text}"
    );

    // Sandbox: absolute path rejected
    let text = agent
        .call("mem_read", json!({"path": "../../etc/passwd"}))
        .await;
    assert!(
        text.to_lowercase().contains("outside") || text.to_lowercase().contains("invalid"),
        "sandbox escape blocked: {text}"
    );

    // Sandbox: meta dir rejected
    let text = agent
        .call(
            "mem_write",
            json!({"path": ".anolisa/audit.log", "content": "x"}),
        )
        .await;
    assert!(
        text.contains("meta") || text.contains(".anolisa"),
        "meta dir write blocked: {text}"
    );

    agent.cleanup().await;
}

// ---- Test 2: Tier C git + snapshot governance ----

#[tokio::test]
async fn git_snapshot_e2e_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = McpAgent::spawn(
        tmp.path(),
        &[
            ("MEMORY_GIT_ENABLED", "true"),
            ("MEMORY_GIT_AUTO_COMMIT", "true"),
        ],
    )
    .await;

    // Seed a file via mem_write — git auto-commit will record it.
    agent
        .call(
            "mem_write",
            json!({"path": "page.md", "content": "version-1"}),
        )
        .await;

    // Wait for git auto-commit to land.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Overwrite with v2 — another auto-commit.
    agent
        .call(
            "mem_write",
            json!({"path": "page.md", "content": "version-2", "overwrite": true}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // mem_log: should surface at least 2 commits for page.md
    let entries = agent
        .call_json("mem_log", json!({"limit": 10, "path": "page.md"}))
        .await;
    let arr = entries.as_array().unwrap();
    assert!(
        arr.len() >= 2,
        "expected >=2 commits, got {}: {entries}",
        arr.len()
    );

    // mem_snapshot + snapshot_list: governance layer
    let snap = agent
        .call_json("mem_snapshot", json!({"name": "v2-snap"}))
        .await;
    let snap_id = snap["id"].as_str().unwrap();

    let listing = agent.call_json("mem_snapshot_list", json!({})).await;
    assert!(
        listing
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"].as_str() == Some(snap_id)),
        "snapshot {snap_id} not in list"
    );

    // Write v3, then snapshot_restore back to v2
    agent
        .call(
            "mem_write",
            json!({"path": "page.md", "content": "version-3", "overwrite": true}),
        )
        .await;
    agent
        .call("mem_snapshot_restore", json!({"id": snap_id}))
        .await;
    let text = agent.call("mem_read", json!({"path": "page.md"})).await;
    assert!(
        text.contains("version-2"),
        "restore should roll back to v2: {text}"
    );

    // mem_revert: with auto_commit=true every write is automatically committed,
    // so revert always restores to HEAD. Write v3, let it auto-commit, then
    // revert — the file should stay at v3 (revert of HEAD content = same).
    agent
        .call(
            "mem_write",
            json!({"path": "page.md", "content": "version-3", "overwrite": true}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let text = agent.call("mem_revert", json!({"path": "page.md"})).await;
    assert!(text.contains("reverted"), "revert: {text}");

    let text = agent.call("mem_read", json!({"path": "page.md"})).await;
    // Revert restores page.md to HEAD (which auto-committed v3).
    assert!(
        text.contains("version-3"),
        "revert should restore HEAD content: {text}"
    );

    agent.cleanup().await;
}

// ---- Test 3: mem_promote (needs a pre-created session scratch file) ----

#[tokio::test]
async fn promote_e2e_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_root = tmp.path().join("__sessions__");
    let scratch = sessions_root.join("ses_promote_e2e").join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("draft.md"), "promoted from scratch").unwrap();

    let mut agent = McpAgent::spawn(
        tmp.path(),
        &[
            ("MEMORY_SESSION_DIR", sessions_root.to_str().unwrap()),
            ("MEMORY_SESSION_ID", "ses_promote_e2e"),
        ],
    )
    .await;

    // mem_promote: copy draft from session scratch to store
    let text = agent
        .call(
            "mem_promote",
            json!({"session_path": "draft.md", "store_path": "imported.md"}),
        )
        .await;
    assert!(text.contains("promoted"), "promote: {text}");

    // mem_read: verify promoted content in store
    let text = agent.call("mem_read", json!({"path": "imported.md"})).await;
    assert_eq!(text, "promoted from scratch", "promoted content: {text}");

    agent.cleanup().await;
}

#[tokio::test]
async fn mem_consolidate_triggers_and_reports() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = McpAgent::spawn(tmp.path(), &[]).await;

    // Write a couple of files so consolidation has something to analyse.
    agent
        .call(
            "mem_write",
            json!({"path": "notes/a.md", "content": "hello consolidation"}),
        )
        .await;
    agent
        .call(
            "mem_write",
            json!({"path": "notes/b.md", "content": "more facts here"}),
        )
        .await;
    // Read one back to create an audit trail with sufficient entries.
    agent.call("mem_read", json!({"path": "notes/a.md"})).await;

    let text = agent.call("mem_consolidate", json!({})).await;
    assert!(
        text.contains("consolidation complete"),
        "consolidate result: {text}"
    );

    agent.cleanup().await;
}

#[tokio::test]
async fn mem_compact_reports_count() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = McpAgent::spawn(tmp.path(), &[]).await;

    let text = agent.call("mem_compact", json!({})).await;
    assert!(text.contains("compacted"), "compact result: {text}");

    agent.cleanup().await;
}
