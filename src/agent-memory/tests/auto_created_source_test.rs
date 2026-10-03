//! Regression: facts produced by auto-consolidation must be reported as
//! auto-created by the sovereignty/summary tools.
//!
//! `ConsolidatedFact::to_markdown` writes `source_tool: ...` but never a
//! `source:` field, while `memory_auto_created` filters on
//! `source.starts_with("auto-")` and `memory_summary` counts
//! `source == "auto-consolidation" | "auto-capture"` as auto-created. Every
//! consolidation-produced fact therefore lands in `unknown_source` and is
//! invisible to `memory_auto_created`.

use agent_memory::config::AppConfig;
use agent_memory::mount::MountStrategyKind;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_sovereignty::memory_auto_created;
use agent_memory::tools::memory_summary_tool::memory_summary;
use tempfile::tempdir;

#[test]
fn consolidation_facts_are_reported_as_auto_created() {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = MountStrategyKind::Userland;
    cfg.memory.index.enabled = false;
    cfg.memory.git.enabled = false;
    cfg.memory.consolidation.enabled = true;
    let svc = MemoryService::new(cfg).unwrap();

    // Generate a session log with enough entries for consolidation.
    svc.write("notes/kconfig/base.md", "kernel config notes", false)
        .unwrap();
    svc.write("notes/kconfig/override.md", "kernel override", false)
        .unwrap();
    svc.write("notes/kconfig/extra.md", "kernel extra", false)
        .unwrap();

    let n = svc.consolidate();
    assert!(n > 0, "consolidation produced no facts");

    // The fact file itself carries no `source:` frontmatter.
    let facts_dir = svc.mount.root.join("facts");
    let mut fact_files = Vec::new();
    for e in walkdir::WalkDir::new(&facts_dir).into_iter().filter_map(|e| e.ok()) {
        if e.file_type().is_file() && e.path().extension().and_then(|x| x.to_str()) == Some("md") {
            fact_files.push(e.path().to_path_buf());
        }
    }
    assert!(!fact_files.is_empty(), "no fact files written");
    let body = std::fs::read_to_string(&fact_files[0]).unwrap();
    eprintln!("--- fact file ---\n{body}\n----------------");
    assert!(
        body.contains("source_tool:"),
        "expected a source_tool stamp in the fact file"
    );
    let has_source_stamp = body.contains("source: auto-consolidation");
    let listed = memory_auto_created(&svc, 20).unwrap();
    let summary = memory_summary(&svc, 10).unwrap();

    eprintln!("has_source_stamp = {has_source_stamp}");
    eprintln!("memory_auto_created => {listed}");
    eprintln!(
        "memory_summary: auto_created={} manual_created={} unknown_source={} by_source={:?}",
        summary.auto_created, summary.manual_created, summary.unknown_source, summary.by_source
    );

    assert!(
        has_source_stamp
            && !listed.contains("no auto-created memories found")
            && summary.auto_created > 0,
        "consolidation facts ({n}) are not reported as auto-created: \
         source stamp in fact file={has_source_stamp}, \
         memory_auto_created={listed:?}, \
         memory_summary auto_created={} unknown_source={}",
        summary.auto_created,
        summary.unknown_source
    );
}
