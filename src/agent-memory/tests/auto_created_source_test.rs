//! Regression test for the auto-created/sovereignty view of consolidated facts.
//!
//! `ConsolidatedFact::to_markdown` used to emit `source_tool` only, while every
//! consumer of the "who created this memory" question reads the `source`
//! frontmatter key: `memory_auto_created` filters on
//! `source.starts_with("auto-")`, `memory_summary` counts
//! `auto-consolidation`/`auto-capture` as `auto_created`, and `mem_export`
//! matches `--source` against the same key. Consolidation-produced facts
//! therefore landed in `unknown_source`: the audit list came back as
//! "(no auto-created memories found)" and a source-filtered export was empty.
//!
//! This drives the real path end to end — write, consolidate, then read back
//! through all three consumers.

use std::path::PathBuf;

use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::mount::MountStrategyKind;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_export::{ExportFilter, memory_export};
use agent_memory::tools::memory_sovereignty::memory_auto_created;
use agent_memory::tools::memory_summary_tool::memory_summary;

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    // Keep the session log inside the temp dir; the default base wants
    // /run/anolisa, which an unprivileged test run cannot create.
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = MountStrategyKind::Userland;
    // Index and git are irrelevant here and would add watchers/commits.
    cfg.memory.index.enabled = false;
    cfg.memory.git.enabled = false;
    cfg.memory.consolidation.enabled = true;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

/// Fact markdown files consolidation wrote under `<mount root>/facts/`.
fn fact_files(svc: &MemoryService) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(svc.mount.root.join("facts"))
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        .map(|e| e.path().to_path_buf())
        .collect();
    files.sort();
    files
}

#[test]
fn consolidation_facts_are_reported_as_auto_created() {
    let (_tmp, svc) = setup();

    // Three writes under one directory: enough tool calls to pass
    // `min_tool_calls` (3) and a shared working context for the heuristics.
    svc.write("notes/kconfig/base.md", "kernel config notes", false)
        .unwrap();
    svc.write("notes/kconfig/override.md", "kernel override", false)
        .unwrap();
    svc.write("notes/kconfig/extra.md", "kernel extra", false)
        .unwrap();

    let written = svc.consolidate();
    assert!(written > 0, "consolidation produced no facts");

    // 1. The writer stamps every fact file, and keeps the evidence origin.
    let files = fact_files(&svc);
    assert_eq!(
        files.len(),
        written,
        "expected {written} fact files under facts/, found {}",
        files.len()
    );
    for path in &files {
        let body = std::fs::read_to_string(path).unwrap();
        assert!(
            body.contains("\nsource: auto-consolidation\n"),
            "{} carries no auto-consolidation source stamp:\n{body}",
            path.display()
        );
        assert!(
            body.contains("\nsource_tool: "),
            "{} lost its source_tool stamp:\n{body}",
            path.display()
        );
    }

    // 2. memory_auto_created lists exactly those facts.
    let listed = memory_auto_created(&svc, 50).unwrap();
    let entries: Vec<serde_json::Value> = serde_json::from_str(&listed).unwrap_or_else(|e| {
        panic!("memory_auto_created returned no JSON ({e}): {listed}");
    });
    assert_eq!(
        entries.len(),
        written,
        "memory_auto_created listed {} of {written} facts: {listed}",
        entries.len()
    );
    for entry in &entries {
        assert_eq!(entry["source"], "auto-consolidation", "{entry}");
        assert!(
            entry["path"]
                .as_str()
                .is_some_and(|p| p.starts_with("facts/")),
            "unexpected entry outside facts/: {entry}"
        );
    }

    // 3. memory_summary counts them as auto-created, per source.
    let summary = memory_summary(&svc, 10).unwrap();
    assert_eq!(
        summary.auto_created, written,
        "summary miscounted auto-created memories: {summary:?}"
    );
    assert_eq!(
        summary.by_source.get("auto-consolidation"),
        Some(&written),
        "by_source has no auto-consolidation bucket: {summary:?}"
    );
    assert_eq!(
        summary.manual_created, 0,
        "consolidated facts counted as manual: {summary:?}"
    );

    // 4. A source-filtered export sees them too.
    let filter = ExportFilter {
        source: Some("auto-consolidation".into()),
        ..Default::default()
    };
    let archive: serde_json::Value =
        serde_json::from_str(&memory_export(&svc, &filter).unwrap()).unwrap();
    assert_eq!(
        archive["total_memories"].as_u64(),
        Some(written as u64),
        "mem_export --source auto-consolidation dropped facts: {archive}"
    );
    assert_eq!(
        archive["stats"]["by_source"]["auto-consolidation"].as_u64(),
        Some(written as u64),
        "export stats miss the auto-consolidation source: {archive}"
    );
}
