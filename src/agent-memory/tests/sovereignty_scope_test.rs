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

/// Poison-safe: a panicking test must not turn every other test's lock
/// acquisition into a cascading PoisonError (the repo standard, see
/// a458b43f2 "replace Mutex::lock().unwrap() with unwrap_or_else").
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
    let _guard = env_lock();
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "alpha") };
    assert_eq!(resolve_effective_scope(&svc, None).unwrap(), None);
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
}

#[test]
fn resolve_effective_scope_invalid_config_warns_to_shared() {
    let (_tmp, svc) = setup_with_scope("typo");
    let _guard = env_lock();
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "alpha") };
    assert_eq!(resolve_effective_scope(&svc, None).unwrap(), None);
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
}

#[test]
fn resolve_effective_scope_isolated_needs_client_name() {
    let (_tmp, svc) = setup_with_scope("isolated");
    let _guard = env_lock();
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
    assert_eq!(resolve_effective_scope(&svc, None).unwrap(), None);
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "alpha") };
    assert_eq!(
        resolve_effective_scope(&svc, None).unwrap(),
        Some("isolated:alpha".to_string())
    );
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
}

#[test]
fn resolve_effective_scope_explicit_wins() {
    let (_tmp, svc) = setup_with_scope("shared");
    assert_eq!(
        resolve_effective_scope(&svc, Some("filter:beta")).unwrap(),
        Some("filter:beta".to_string())
    );
}

// ---------- sovereignty isolation (integration) ----------

#[test]
fn about_and_forget_stay_within_the_calling_agent_scope() {
    let (_tmp, svc) = setup_with_scope("isolated");
    let _guard = env_lock();

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

    // Positive case first: alpha itself sees its own memories — a scoping
    // regression that returned nothing for everyone would otherwise pass
    // the negative assertions below.
    let about = memory_about(&svc, "authentication", 10).unwrap();
    assert!(
        about.contains("auth-a"),
        "alpha must see its own memories in isolated scope: {about}"
    );
    let preview = memory_forget(&svc, "authentication", false).unwrap();
    assert!(
        preview.contains("auth-a"),
        "alpha's forget preview must list its own file: {preview}"
    );

    // beta (same store, isolated scope) must not see them via about...
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "beta") };
    let about = memory_about(&svc, "authentication", 10).unwrap();
    assert!(
        !about.contains("auth-a"),
        "beta's memory_about leaked alpha's memories: {about}"
    );

    // ...and must not delete them via forget. (The old
    // `!contains("removed") || ...` disjunct was dead: "removed" never
    // appears in forget's output vocabulary.)
    let forget = memory_forget(&svc, "authentication", true).unwrap();
    assert!(
        forget.contains("Nothing to forget"),
        "beta's memory_forget matched alpha's memories: {forget}"
    );
    assert!(
        !forget.contains("auth-a"),
        "beta's memory_forget listed alpha's file: {forget}"
    );
    // alpha's file is still on disk.
    assert!(
        svc.mount.root.join("notes/auth-a.md").exists(),
        "alpha's memory file was deleted by another agent"
    );
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
}

#[test]
fn read_tools_fail_closed_on_an_empty_client_name() {
    // Read-path counterpart of `forget_fails_closed`'s empty-value rows:
    // `MCP_CLIENT_NAME=` used to make the reader degrade to *shared*
    // while the indexer tagged the misconfigured agent's rows with
    // `agent_id = ''`, so that agent's memory_about listed every other
    // agent's memories (the exact leak this PR exists to close).
    // Set-but-empty is a misconfiguration, not an absence: both read
    // tools error loudly instead of silently widening the domain.
    let (_tmp, svc) = setup_with_scope("isolated");
    let _guard = env_lock();

    unsafe { std::env::set_var("MCP_CLIENT_NAME", "alpha") };
    svc.write(
        "notes/auth-a.md",
        "authentication design notes alpha",
        false,
    )
    .unwrap();
    assert!(wait_for_index(&svc, 2), "alpha's memories must be indexed");

    unsafe { std::env::set_var("MCP_CLIENT_NAME", "") };
    let about = memory_about(&svc, "authentication", 10);
    assert!(
        about.is_err(),
        "memory_about must fail closed on an empty MCP_CLIENT_NAME, got: {:?}",
        about
    );
    let search = agent_memory::tools::memory_search::memory_search(
        &svc,
        "authentication",
        10,
        None,
        None,
        None,
    );
    assert!(
        search.is_err(),
        "memory_search must fail closed on an empty MCP_CLIENT_NAME, got: {:?}",
        search
    );
    // The unit helper makes the same distinction: set-but-empty errors,
    // genuinely unset degrades to shared with a warning.
    let scope = resolve_effective_scope(&svc, None);
    assert!(
        scope.is_err(),
        "resolve_effective_scope must error on set-but-empty, got: {:?}",
        scope
    );
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
    assert_eq!(resolve_effective_scope(&svc, None).unwrap(), None);
}

#[test]
fn forget_fails_closed_when_scope_unresolvable() {
    // The destructive tool must error rather than degrade to shared when
    // an isolated scope cannot be resolved: forgetting every agent's
    // memories because MCP_CLIENT_NAME is unset would be the exact
    // catastrophe the isolation config exists to prevent.
    let (_tmp, svc) = setup_with_scope("isolated");
    let _guard = env_lock();
    unsafe { std::env::remove_var("MCP_CLIENT_NAME") };
    svc.write(
        "notes/auth-a.md",
        "authentication design notes alpha",
        false,
    )
    .unwrap();
    assert!(wait_for_index(&svc, 2), "memory must be indexed");

    let result = memory_forget(&svc, "authentication", true);
    assert!(
        result.is_err(),
        "forget must fail closed on an unresolvable isolated scope, got: {:?}",
        result
    );
    assert!(
        svc.mount.root.join("notes/auth-a.md").exists(),
        "nothing may be deleted while the scope is unresolvable"
    );

    // An empty MCP_CLIENT_NAME is set-but-unresolvable, exactly like unset:
    // the indexer would tag every agent's rows with "", collapsing all
    // agents into one `isolated:` bucket.
    unsafe { std::env::set_var("MCP_CLIENT_NAME", "") };
    let result = memory_forget(&svc, "authentication", true);
    assert!(
        result.is_err(),
        "forget must fail closed on an empty MCP_CLIENT_NAME, got: {:?}",
        result
    );
    assert!(
        svc.mount.root.join("notes/auth-a.md").exists(),
        "nothing may be deleted while the agent id is empty"
    );
}

#[test]
fn shared_scope_keeps_today_behavior_for_sovereignty() {
    // Default (shared) deployments are unchanged: about/forget see all
    // memories exactly as before the scoping fix.
    let (_tmp, svc) = setup_with_scope("shared");
    let _guard = env_lock();
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
