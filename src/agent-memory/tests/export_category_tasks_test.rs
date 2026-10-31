//! Regression tests for `mem_export` filter semantics (issue #5002).
//!
//! `category` and `source` describe memories; the exported task inventory is
//! governed by `include_tasks` alone. The filters used to run before the
//! task/memory split, and task frontmatter carries neither field, so every
//! filtered export shipped `"tasks": []` — and re-importing that archive
//! silently dropped the whole task inventory.

use tempfile::tempdir;

use agent_memory::config::{AppConfig, Profile};
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_export::{ExportFilter, memory_export};
use agent_memory::tools::memory_import::{ImportStrategy, memory_import};
use agent_memory::tools::memory_task::memory_task_save;

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "tester".into();
    cfg.memory.profile = Profile::Advanced;
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

/// Seed one `lesson` fact, one `interest` fact, and one task.
fn seed(svc: &MemoryService) {
    svc.write(
        "facts/lesson/01LESSON.md",
        "---\nid: 01LESSON\ncategory: lesson\nsource: auto-consolidation\n---\n\nRetry the build.\n",
        false,
    )
    .unwrap();
    svc.write(
        "facts/interest/01INTEREST.md",
        "---\nid: 01INTEREST\ncategory: interest\nsource: manual-observe\n---\n\nKernel config.\n",
        false,
    )
    .unwrap();
    memory_task_save(
        svc,
        "Ship the export fix",
        Some("in-progress"),
        Some(40),
        Some(vec!["Add a regression test".into()]),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
}

fn export(svc: &MemoryService, filter: ExportFilter) -> serde_json::Value {
    let json = memory_export(svc, &filter).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn paths(entries: &serde_json::Value) -> Vec<String> {
    entries
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn category_filter_keeps_tasks() {
    let (_t, svc) = setup();
    seed(&svc);

    let archive = export(
        &svc,
        ExportFilter {
            category: Some("lesson".into()),
            source: None,
            include_tasks: true,
        },
    );

    assert_eq!(
        paths(&archive["memories"]),
        vec!["facts/lesson/01LESSON.md".to_string()],
        "the category filter must still select only matching memories"
    );
    let tasks = paths(&archive["tasks"]);
    assert_eq!(
        tasks.len(),
        1,
        "filtered export dropped the tasks: {archive}"
    );
    assert!(
        tasks[0].starts_with("tasks/"),
        "unexpected task path {tasks:?}"
    );
    // `memory_task_save` YAML-quotes the title; the flat frontmatter reader
    // keeps the quotes, so compare on the unquoted value.
    assert_eq!(
        archive["tasks"][0]["frontmatter"]["title"]
            .as_str()
            .unwrap()
            .trim_matches('"'),
        "Ship the export fix"
    );
}

#[test]
fn source_filter_keeps_tasks() {
    let (_t, svc) = setup();
    seed(&svc);

    let archive = export(
        &svc,
        ExportFilter {
            category: None,
            source: Some("auto-consolidation".into()),
            include_tasks: true,
        },
    );

    assert_eq!(
        paths(&archive["memories"]),
        vec!["facts/lesson/01LESSON.md".to_string()]
    );
    assert_eq!(
        paths(&archive["tasks"]).len(),
        1,
        "source-filtered export dropped the tasks: {archive}"
    );
}

#[test]
fn unfiltered_export_still_contains_tasks() {
    let (_t, svc) = setup();
    seed(&svc);

    let archive = export(&svc, ExportFilter::default());

    // The mount scaffold README.md is exported as a memory too, so assert on
    // the seeded facts rather than on the total count.
    let mem = paths(&archive["memories"]);
    assert!(
        mem.contains(&"facts/lesson/01LESSON.md".to_string()),
        "{mem:?}"
    );
    assert!(
        mem.contains(&"facts/interest/01INTEREST.md".to_string()),
        "{mem:?}"
    );
    assert_eq!(paths(&archive["tasks"]).len(), 1);
}

#[test]
fn include_tasks_false_still_drops_tasks_under_a_filter() {
    let (_t, svc) = setup();
    seed(&svc);

    let archive = export(
        &svc,
        ExportFilter {
            category: Some("lesson".into()),
            source: None,
            include_tasks: false,
        },
    );

    assert_eq!(paths(&archive["memories"]).len(), 1);
    assert!(
        archive["tasks"].as_array().unwrap().is_empty(),
        "include_tasks=false must stay authoritative: {archive}"
    );
}

#[test]
fn tasks_never_leak_into_memories_or_stats() {
    let (_t, svc) = setup();
    seed(&svc);

    let archive = export(&svc, ExportFilter::default());

    assert!(
        !paths(&archive["memories"])
            .iter()
            .any(|p| p.starts_with("tasks/")),
        "task entries must not be counted as memories"
    );
    assert_eq!(
        archive["total_memories"].as_u64().unwrap(),
        archive["memories"].as_array().unwrap().len() as u64
    );
    let by_category = archive["stats"]["by_category"].as_object().unwrap();
    assert_eq!(
        by_category.len(),
        2,
        "the task inventory must not add categories: {by_category:?}"
    );
    assert_eq!(by_category["lesson"].as_u64().unwrap(), 1);
    assert_eq!(by_category["interest"].as_u64().unwrap(), 1);
}

#[test]
fn filtered_export_import_roundtrip_keeps_the_task() {
    let (_t, svc) = setup();
    seed(&svc);

    let json = memory_export(
        &svc,
        &ExportFilter {
            category: Some("lesson".into()),
            source: None,
            include_tasks: true,
        },
    )
    .unwrap();

    let (_t2, target) = setup();
    memory_import(&target, &json, ImportStrategy::SkipExisting, false).unwrap();

    let restored: Vec<String> = target
        .list(
            "tasks",
            agent_memory::tools::ListOptions {
                recursive: true,
                ..Default::default()
            },
        )
        .unwrap()
        .into_iter()
        .map(|e| e.path)
        .collect();
    assert_eq!(
        restored.len(),
        1,
        "importing a category-filtered export must restore the task inventory, got {restored:?}"
    );
    assert!(restored[0].starts_with("tasks/"), "{restored:?}");
}
