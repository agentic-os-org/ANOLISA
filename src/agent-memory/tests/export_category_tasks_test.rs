//! Regression: a category/source filter on `mem_export` must not silently
//! drop the task inventory.
//!
//! `ExportFilter::include_tasks` (default `true`) is the knob that decides
//! whether tasks travel in the AMA archive, and the task frontmatter has no
//! `category`/`source` field at all — so the fact-metadata filters must not
//! apply to tasks. They were applied before the task/memory split, which
//! made a filtered export (e.g. `mem_export(category="lesson")`) a
//! tasks-free archive: an import of that archive into a fresh store loses
//! every task while the tool reports success.

use serde_json::Value;
use tempfile::tempdir;

use agent_memory::config::AppConfig;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_export::{ExportFilter, memory_export};

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.index.enabled = false;
    cfg.memory.git.enabled = false;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

fn seed(svc: &MemoryService) {
    // One lesson fact and one interest fact.
    svc.write(
        "facts/lesson/01A.md",
        "---\ncategory: lesson\nsource: auto-consolidation\n---\n\nA lesson\n",
        false,
    )
    .unwrap();
    svc.write(
        "facts/interest/01B.md",
        "---\ncategory: interest\nsource: auto-consolidation\n---\n\nAn interest\n",
        false,
    )
    .unwrap();
    agent_memory::tools::memory_task::memory_task_save(
        svc,
        "Ship the widget",
        Some("in-progress"),
        Some(30),
        Some(vec!["step one".into()]),
        None,
        None,
        None,
        Some("task context"),
        Some("task-1"),
    )
    .unwrap();
}

#[test]
fn category_filter_keeps_tasks() {
    let (_tmp, svc) = setup();
    seed(&svc);

    let filter = ExportFilter {
        category: Some("lesson".into()),
        source: None,
        include_tasks: true,
    };
    let archive = memory_export(&svc, &filter).unwrap();
    let v: Value = serde_json::from_str(&archive).unwrap();

    // The category filter still narrows the memories...
    assert_eq!(v["memories"].as_array().unwrap().len(), 1, "{archive}");
    assert_eq!(v["memories"][0]["path"], "facts/lesson/01A.md");
    // ...but include_tasks governs tasks, so the inventory must survive.
    assert_eq!(
        v["tasks"].as_array().unwrap().len(),
        1,
        "category filter dropped the task from the archive: {archive}"
    );
    assert_eq!(v["tasks"][0]["path"], "tasks/task-1.md");
}

#[test]
fn source_filter_keeps_tasks() {
    let (_tmp, svc) = setup();
    seed(&svc);

    let filter = ExportFilter {
        category: None,
        source: Some("auto-consolidation".into()),
        include_tasks: true,
    };
    let archive = memory_export(&svc, &filter).unwrap();
    let v: Value = serde_json::from_str(&archive).unwrap();

    assert_eq!(v["memories"].as_array().unwrap().len(), 2, "{archive}");
    assert_eq!(
        v["tasks"].as_array().unwrap().len(),
        1,
        "source filter dropped the task from the archive: {archive}"
    );
}

#[test]
fn unfiltered_export_still_contains_tasks() {
    let (_tmp, svc) = setup();
    seed(&svc);

    let archive = memory_export(&svc, &ExportFilter::default()).unwrap();
    let v: Value = serde_json::from_str(&archive).unwrap();
    assert_eq!(v["tasks"].as_array().unwrap().len(), 1, "{archive}");
}
