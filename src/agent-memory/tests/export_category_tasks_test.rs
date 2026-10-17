//! Regression: a filtered export must keep the task inventory.
//!
//! `mem_export` always requests tasks, and task files carry no `category` or
//! `source` frontmatter, so applying the fact filters before splitting the
//! task section out emptied it — the archive then lost every task on import.

use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_export::{AmaArchive, ExportFilter, ExportedMemory, memory_export};
use agent_memory::tools::memory_import::{ImportStrategy, memory_import};

const LESSON_FACT: &str = "---\nid: 01JLESSON000000000000000\ncategory: lesson\nsource: auto-consolidation\n---\n\nRetry the build once before reporting failure.\n";
const INTEREST_FACT: &str = "---\nid: 01JINTEREST00000000000000\ncategory: interest\nsource: manual-observe\n---\n\nPrefers terse release notes.\n";
/// Mirrors `Task::to_markdown`: no category, no source.
const TASK: &str = "---\nid: 01JTASK0000000000000000000\ntitle: \"Land the export fix\"\nstatus: open\nprogress: 40\ncreated_at: 2026-10-04T08:00:00Z\nupdated_at: 2026-10-04T08:00:00Z\n---\n\nSplit tasks out before filtering.\n";

/// Empty store on a userland mount, with git versioning off so the walk sees
/// only the files this test writes.
fn store(user_id: &str) -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().expect("tempdir");
    let mut cfg = AppConfig::default();
    cfg.global.user_id = user_id.into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("__sessions__").to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.git.enabled = false;
    let svc = MemoryService::new(cfg).expect("service starts");
    (tmp, svc)
}

/// Store holding two facts (one per category/source) plus one task.
fn seeded_store() -> (tempfile::TempDir, MemoryService) {
    let (tmp, svc) = store("export-tester");
    svc.write("facts/lesson/01JLESSON.md", LESSON_FACT, false)
        .expect("write lesson fact");
    svc.write("facts/interest/01JINTEREST.md", INTEREST_FACT, false)
        .expect("write interest fact");
    svc.write("tasks/01JTASK.md", TASK, false)
        .expect("write task");
    (tmp, svc)
}

fn export(svc: &MemoryService, filter: ExportFilter) -> AmaArchive {
    let json = memory_export(svc, &filter).expect("export succeeds");
    serde_json::from_str(&json).expect("export is valid AMA JSON")
}

fn sorted_paths(entries: &[ExportedMemory]) -> Vec<String> {
    let mut paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
    paths.sort();
    paths
}

#[test]
fn category_filter_keeps_tasks() {
    let (_tmp, svc) = seeded_store();

    let archive = export(
        &svc,
        ExportFilter {
            category: Some("lesson".into()),
            ..Default::default()
        },
    );

    assert_eq!(
        sorted_paths(&archive.memories),
        ["facts/lesson/01JLESSON.md"]
    );
    assert_eq!(sorted_paths(&archive.tasks), ["tasks/01JTASK.md"]);
    assert_eq!(archive.total_memories, archive.memories.len());
}

#[test]
fn source_filter_keeps_tasks() {
    let (_tmp, svc) = seeded_store();

    let archive = export(
        &svc,
        ExportFilter {
            source: Some("manual-observe".into()),
            ..Default::default()
        },
    );

    assert_eq!(
        sorted_paths(&archive.memories),
        ["facts/interest/01JINTEREST.md"]
    );
    assert_eq!(sorted_paths(&archive.tasks), ["tasks/01JTASK.md"]);
    assert_eq!(archive.total_memories, archive.memories.len());
}

#[test]
fn unfiltered_export_contains_tasks() {
    let (_tmp, svc) = seeded_store();

    let archive = export(&svc, ExportFilter::default());

    // The mount scaffolding also leaves a README.md in the store root; only
    // the seeded entries are asserted on.
    let memory_paths = sorted_paths(&archive.memories);
    assert!(memory_paths.contains(&"facts/lesson/01JLESSON.md".to_string()));
    assert!(memory_paths.contains(&"facts/interest/01JINTEREST.md".to_string()));
    assert_eq!(sorted_paths(&archive.tasks), ["tasks/01JTASK.md"]);
    // Stats describe the fact set; the task inventory is not counted there.
    assert_eq!(archive.stats.by_category.get("lesson"), Some(&1));
    assert_eq!(archive.stats.by_category.get("interest"), Some(&1));
    assert_eq!(archive.stats.by_category.len(), 2);
    assert_eq!(archive.total_memories, archive.memories.len());
}

#[test]
fn include_tasks_false_still_omits_tasks() {
    let (_tmp, svc) = seeded_store();

    let archive = export(
        &svc,
        ExportFilter {
            category: Some("lesson".into()),
            include_tasks: false,
            ..Default::default()
        },
    );

    assert!(archive.tasks.is_empty());
    assert_eq!(
        sorted_paths(&archive.memories),
        ["facts/lesson/01JLESSON.md"]
    );
}

#[test]
fn filtered_export_round_trips_tasks() {
    let (_src_tmp, src) = seeded_store();
    let json = memory_export(
        &src,
        &ExportFilter {
            category: Some("lesson".into()),
            ..Default::default()
        },
    )
    .expect("export succeeds");

    let (_dst_tmp, dst) = store("import-tester");
    memory_import(&dst, &json, ImportStrategy::SkipExisting, false).expect("import succeeds");

    assert!(
        dst.read("tasks/01JTASK.md").is_ok(),
        "task inventory must survive a filtered export/import round trip"
    );
    assert!(dst.read("facts/lesson/01JLESSON.md").is_ok());
    assert!(dst.read("facts/interest/01JINTEREST.md").is_err());
}
