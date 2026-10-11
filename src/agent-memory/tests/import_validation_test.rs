//! Import validation contract: AMA archives only ever contain `.md` memory
//! paths (the writer side filters its walk to markdown), so a non-`.md`
//! entry, an empty path, or a duplicate path is malformed input that must be
//! reported and skipped — never written, since writing it would clobber
//! non-memory files (the fact log, the generated index) with markdown.

use agent_memory::config::AppConfig;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_import::{ImportStrategy, memory_import};
use tempfile::tempdir;

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

fn archive(entries: &str) -> String {
    format!(
        r#"{{"version":"1.0","format":"anolisa-memory-archive","exported_at":"2026-10-03T00:00:00Z","agent_id":"tester","user_id":"tester","total_memories":1,"stats":{{"by_category":{{}},"by_source":{{}},"total_bytes":0}},"memories":[{entries}],"tasks":[]}}"#
    )
}

fn entry(path: &str) -> String {
    format!(r#"{{"path":"{path}","frontmatter":{{"id":"e"}},"content":"body"}}"#)
}

#[test]
fn import_rejects_non_md_entry_paths() {
    let (_tmp, svc) = setup();
    // The append-only fact log lives at facts/facts.jsonl.
    let facts = svc.mount.root.join("facts/facts.jsonl");
    std::fs::create_dir_all(facts.parent().unwrap()).unwrap();
    std::fs::write(&facts, "{\"fact\":\"one\"}\n").unwrap();

    let json = archive(&entry("facts/facts.jsonl"));
    let out = memory_import(&svc, &json, ImportStrategy::SkipExisting, false).unwrap();

    // (a) the summary reports 1 error naming the path
    assert!(out.contains("1 errors"), "summary: {out}");
    assert!(out.contains("facts/facts.jsonl"), "summary: {out}");
    // (b) the fact log is byte-identical — not truncated into markdown
    assert_eq!(
        std::fs::read_to_string(&facts).unwrap(),
        "{\"fact\":\"one\"}\n"
    );
    // (c) nothing was imported
    assert!(out.contains("0 imported"), "summary: {out}");
}

#[test]
fn import_rejects_empty_entry_path() {
    let (_tmp, svc) = setup();
    let json = archive(&entry(""));
    let out = memory_import(&svc, &json, ImportStrategy::SkipExisting, false).unwrap();
    assert!(out.contains("empty path"), "summary: {out}");
    assert!(out.contains("1 errors"), "summary: {out}");
}

#[test]
fn import_rejects_duplicate_entry_paths() {
    let (_tmp, svc) = setup();
    let e = entry("notes/dup.md");
    let json = archive(&format!("{e},{e}"));
    let out = memory_import(&svc, &json, ImportStrategy::SkipExisting, false).unwrap();
    // The first is imported, the duplicate is reported — not silently
    // double-written (last-wins under merge, phantom-skipped under
    // skip-existing).
    assert!(out.contains("duplicate"), "summary: {out}");
    assert!(out.contains("1 imported"), "summary: {out}");
    assert!(out.contains("1 errors"), "summary: {out}");
}

#[test]
fn import_rejects_duplicate_across_memories_and_tasks() {
    let (_tmp, svc) = setup();
    // A task and a memory with the same path collide on disk: one shared
    // seen-set must report the second.
    let json = format!(
        r#"{{"version":"1.0","format":"anolisa-memory-archive","exported_at":"2026-10-03T00:00:00Z","agent_id":"tester","user_id":"tester","total_memories":1,"stats":{{"by_category":{{}},"by_source":{{}},"total_bytes":0}},
             "memories":[{}],
             "tasks":[{}]}}"#,
        entry("notes/shared.md"),
        entry("notes/shared.md")
    );
    let out = memory_import(&svc, &json, ImportStrategy::SkipExisting, false).unwrap();
    assert!(out.contains("duplicate"), "summary: {out}");
}

#[test]
fn import_dry_run_reports_would_be_rejections() {
    let (_tmp, svc) = setup();
    let facts = svc.mount.root.join("facts/facts.jsonl");
    std::fs::create_dir_all(facts.parent().unwrap()).unwrap();
    std::fs::write(&facts, "{\"fact\":\"one\"}\n").unwrap();

    let json = archive(&entry("facts/facts.jsonl"));
    let out = memory_import(&svc, &json, ImportStrategy::SkipExisting, true).unwrap();
    assert!(out.contains("[DRY RUN]"), "summary: {out}");
    assert!(out.contains("1 errors"), "summary: {out}");
    assert!(std::fs::read_to_string(&facts).unwrap() == "{\"fact\":\"one\"}\n");
}

#[test]
fn import_accepts_real_export_round_trip() {
    // Regression: a genuine export (which contains .md paths only, including
    // MEMORY.md and README.md) still imports cleanly with zero errors.
    let (_tmp, svc) = setup();
    svc.write("notes/a.md", "alpha content", false).unwrap();
    let exported = agent_memory::tools::memory_export::memory_export(
        &svc,
        &agent_memory::tools::memory_export::ExportFilter::default(),
    )
    .unwrap();
    let out = memory_import(&svc, &exported, ImportStrategy::SkipExisting, true).unwrap();
    assert!(
        out.contains("0 errors"),
        "real archive must import without errors: {out}"
    );
}
