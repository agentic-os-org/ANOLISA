//! Regression tests for consolidation provenance (issue #4544).
//!
//! `ConsolidatedFact::to_markdown` — the only writer of consolidation facts —
//! emitted `source_tool` but never a `source` field, while every consumer of
//! the sovereignty view classifies on `source`. The result was that no
//! auto-consolidated memory was ever visible as auto-created:
//! `memory_auto_created` answered "(no auto-created memories found)",
//! `memory_summary.auto_created` stayed at 0 with the facts piling up in
//! `unknown_source`, and `mem_export --source auto-consolidation` was empty.

use tempfile::tempdir;

use agent_memory::config::{AppConfig, Profile};
use agent_memory::consolidation::fact::AUTO_CONSOLIDATION_SOURCE;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_export::{ExportFilter, memory_export};
use agent_memory::tools::memory_sovereignty::memory_auto_created;
use agent_memory::tools::memory_summary_tool::memory_summary;

/// A store with consolidation enabled and a writable session log. The
/// incremental trigger is disabled so `consolidate()` below is the only thing
/// that can produce facts.
fn setup() -> (tempfile::TempDir, tempfile::TempDir, MemoryService) {
    let store = tempdir().unwrap();
    let sessions = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.profile = Profile::Advanced;
    cfg.memory.paths.base_dir = store.path().to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.session.base_dir = sessions.path().to_string_lossy().into();
    cfg.memory.consolidation.enabled = true;
    cfg.memory.consolidation.incremental_interval = 0;
    let svc = MemoryService::new(cfg).unwrap();
    (store, sessions, svc)
}

/// Plain notes written by hand. They carry no frontmatter at all, so they are
/// the only entries `memory_summary` may legitimately report as
/// `unknown_source`.
const SEEDED_NOTES: usize = 4;

/// Enough tool calls to clear `min_tool_calls`, with content the heuristics
/// turn into facts.
fn run_a_session(svc: &MemoryService) -> usize {
    svc.write("notes/a.md", "kernel config notes for the build\n", false)
        .unwrap();
    svc.write("notes/b.md", "kernel override applied twice\n", false)
        .unwrap();
    svc.write("notes/c.md", "kernel extra flags recorded\n", false)
        .unwrap();
    svc.write("notes/d.md", "kernel config summary\n", false)
        .unwrap();
    let facts = svc.consolidate();
    assert!(
        facts > 0,
        "test precondition: consolidation must produce at least one fact"
    );
    facts
}

#[test]
fn consolidated_facts_carry_the_auto_consolidation_source() {
    let (_s, _sess, svc) = setup();
    run_a_session(&svc);

    let archive: serde_json::Value =
        serde_json::from_str(&memory_export(&svc, &ExportFilter::default()).unwrap()).unwrap();
    let facts: Vec<&serde_json::Value> = archive["memories"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["path"].as_str().unwrap().starts_with("facts/"))
        .collect();
    assert!(
        !facts.is_empty(),
        "expected consolidation facts in {archive}"
    );
    for fact in &facts {
        assert_eq!(
            fact["frontmatter"]["source"].as_str(),
            Some(AUTO_CONSOLIDATION_SOURCE),
            "fact {} has no source stamp: {}",
            fact["path"].as_str().unwrap(),
            fact["frontmatter"]
        );
        // `source_tool` is the evidence producer and must survive next to it.
        assert!(
            fact["frontmatter"]["source_tool"].as_str().is_some(),
            "fact {} lost source_tool",
            fact["path"].as_str().unwrap()
        );
    }
}

#[test]
fn memory_auto_created_lists_consolidated_facts() {
    let (_s, _sess, svc) = setup();
    run_a_session(&svc);

    let listing = memory_auto_created(&svc, 20).unwrap();
    assert!(
        !listing.contains("no auto-created memories"),
        "the auto-created view must see consolidation facts, got: {listing}"
    );
    assert!(
        listing.contains("facts/"),
        "expected a fact path in the listing, got: {listing}"
    );
}

#[test]
fn memory_summary_counts_consolidated_facts_as_auto_created() {
    let (_s, _sess, svc) = setup();
    let facts = run_a_session(&svc);

    let summary = memory_summary(&svc, 10).unwrap();
    assert_eq!(
        summary.auto_created, facts,
        "every consolidation fact must be counted as auto-created: {summary:?}"
    );
    assert_eq!(
        summary.by_source.get(AUTO_CONSOLIDATION_SOURCE).copied(),
        Some(facts),
        "by_source must attribute them to {AUTO_CONSOLIDATION_SOURCE}: {summary:?}"
    );
    assert_eq!(
        summary.unknown_source, SEEDED_NOTES,
        "only the hand-written notes lack a source; no consolidation fact may \
         land in unknown_source: {summary:?}"
    );
    assert_eq!(summary.manual_created, 0, "{summary:?}");
}

#[test]
fn source_filtered_export_returns_consolidated_facts() {
    let (_s, _sess, svc) = setup();
    let facts = run_a_session(&svc);

    let archive: serde_json::Value = serde_json::from_str(
        &memory_export(
            &svc,
            &ExportFilter {
                category: None,
                source: Some(AUTO_CONSOLIDATION_SOURCE.into()),
                include_tasks: true,
            },
        )
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        archive["memories"].as_array().unwrap().len(),
        facts,
        "mem_export --source auto-consolidation must not be empty: {archive}"
    );
}
