//! Agent-scope isolation contract for the sovereignty tools: under
//! `agent_scope = "isolated"`, one agent's `memory_about` must not list —
//! and its `memory_forget` must never delete — another agent's memories.
//! `memory_search` already enforces this; these tests pin the same contract
//! for the about/forget paths and the shared scope-resolution helper.

use agent_memory::config::AppConfig;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_search::resolve_effective_scope;
use agent_memory::tools::memory_sovereignty::{memory_about, memory_forget};
use std::sync::Mutex;

/// Serialize tests that mutate MCP_CLIENT_NAME (process-global env).
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn setup_with_scope(scope: &str) -> (tempfile::TempDir, MemoryService) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.index.enabled = true;
    cfg.memory.agent_scope = scope.to_string();
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

fn wait_for_index(svc: &MemoryService, expected_min: usize) -> bool {
    svc.index
        .as_ref()
        .map(|h| h.wait_until_at_least(expected_min, 4000))
        .unwrap_or(false)
}

// ---------- resolve_effective_scope (unit) ----------

#[test]
fn resolve_effective_scope_none_when_shared() {
    let (_tmp, svc) = setup_with_scope("shared");
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "alpha") };
    assert_eq!(resolve_effective_scope(&svc, None), None);
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
}

#[test]
fn resolve_effective_scope_invalid_config_warns_to_shared() {
    let (_tmp, svc) = setup_with_scope("typo");
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "alpha") };
    assert_eq!(resolve_effective_scope(&svc, None), None);
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
}

#[test]
fn resolve_effective_scope_isolated_needs_client_name() {
    let (_tmp, svc) = setup_with_scope("isolated");
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
    assert_eq!(resolve_effective_scope(&svc, None), None);
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "alpha") };
    assert_eq!(
        resolve_effective_scope(&svc, None),
        Some("isolated:alpha".to_string())
    );
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
}

#[test]
fn resolve_effective_scope_explicit_wins() {
    let (_tmp, svc) = setup_with_scope("shared");
    assert_eq!(
        resolve_effective_scope(&svc, Some("filter:beta")),
        Some("filter:beta".to_string())
    );
}

// ---------- sovereignty isolation (integration) ----------

#[test]
fn about_and_forget_stay_within_the_calling_agent_scope() {
    let (_tmp, svc) = setup_with_scope("isolated");
    let _guard = ENV_LOCK.lock().unwrap();

    // alpha writes two memories.
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "alpha") };
    svc.write(
        "notes/auth-a.md",
        "authentication design notes alpha",
        false,
    )
    .unwrap();
    svc.write("notes/deploy-a.md", "deployment checklist alpha", false)
        .unwrap();
    assert!(wait_for_index(&svc, 3), "alpha's memories must be indexed");

    // beta (same store, isolated scope) must not see them via about...
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "beta") };
    let about = memory_about(&svc, "authentication", 10).unwrap();
    assert!(
        !about.contains("auth-a"),
        "beta's memory_about leaked alpha's memories: {about}"
    );

    // ...and must not delete them via forget.
    let forget = memory_forget(&svc, "authentication", true).unwrap();
    assert!(
        !forget.contains("removed") || !forget.contains("auth-a"),
        "beta's memory_forget must not remove alpha's files: {forget}"
    );
    // alpha's file is still on disk.
    assert!(
        svc.mount.root.join("notes/auth-a.md").exists(),
        "alpha's memory file was deleted by another agent"
    );
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
}

#[test]
fn shared_scope_keeps_today_behavior_for_sovereignty() {
    // Default (shared) deployments are unchanged: about/forget see all
    // memories exactly as before the scoping fix.
    let (_tmp, svc) = setup_with_scope("shared");
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
    svc.write("notes/shared-topic.md", "shared topic content", false)
        .unwrap();
    assert!(wait_for_index(&svc, 2), "memory must be indexed");

    let about = memory_about(&svc, "shared topic", 10).unwrap();
    assert!(
        about.contains("shared-topic") || about.contains("1 mem"),
        "shared scope must keep today's visibility: {about}"
    );
}
