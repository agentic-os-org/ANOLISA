//! Summary rankings must survive an AMA export/import round trip.

use std::collections::BTreeMap;

use agent_memory::config::AppConfig;
use agent_memory::mount::MountStrategyKind;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_export::{AmaArchive, ExportFilter, memory_export};
use agent_memory::tools::memory_import::{ImportStrategy, memory_import};
use agent_memory::tools::memory_summary_tool::{MemorySummary, memory_summary};
use tempfile::{TempDir, tempdir};

fn service() -> (TempDir, MemoryService) {
    let tmp = tempdir().unwrap();
    let mut config = AppConfig::default();
    config.global.user_id = "summary-test".into();
    config.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    config.memory.session.base_dir = tmp.path().join("sessions").to_string_lossy().into();
    config.memory.mount.strategy = MountStrategyKind::Userland;
    config.memory.index.enabled = false;
    config.memory.git.enabled = false;
    let svc = MemoryService::new(config).unwrap();
    (tmp, svc)
}

fn rankings(summary: &MemorySummary) -> (BTreeMap<String, usize>, BTreeMap<String, usize>) {
    (
        summary.top_concepts.iter().cloned().collect(),
        summary.top_files.iter().cloned().collect(),
    )
}

#[test]
fn ama_roundtrip_keeps_summary_rankings() {
    let (_source_dir, source) = service();
    source.write(
        "facts/lesson/example.md",
        "---\ntitle: \"Imported lesson\"\nsource: manual-write\ncategory: lesson\nconcepts: [\"rust\", \"memory\"]\nfiles: [\"src/lib.rs\"]\n---\nA useful lesson.\n",
        false,
    ).unwrap();
    let before = memory_summary(&source, 10).unwrap();
    let expected = (
        BTreeMap::from([("rust".into(), 1), ("memory".into(), 1)]),
        BTreeMap::from([("src/lib.rs".into(), 1)]),
    );
    assert_eq!(rankings(&before), expected);
    let mut archive = memory_export(&source, &ExportFilter::default()).unwrap();
    for _ in 0..2 {
        let (_destination_dir, destination) = service();
        let result = memory_import(&destination, &archive, ImportStrategy::Merge, false).unwrap();
        assert!(result.contains("0 errors"), "{result}");
        let imported = destination.read("facts/lesson/example.md").unwrap();
        assert!(imported.contains("concepts:\n  - rust\n  - memory\n"));
        let after = memory_summary(&destination, 10).unwrap();
        assert_eq!(after.total_memories, 1);
        assert_eq!(after.manual_created, 1);
        assert_eq!(after.by_category, before.by_category);
        assert_eq!(after.recent_memories[0].title, "Imported lesson");
        assert_eq!(rankings(&after), expected);
        archive = memory_export(&destination, &ExportFilter::default()).unwrap();
    }
}

#[test]
fn inline_json_lists_preserve_literal_commas_quotes_and_backslashes() {
    let (_source_dir, source) = service();
    let concepts = ["parser, writer", "quoted \"concept\"", "内存"];
    let files = ["src/a,b.rs", r"notes\memory.md", "[draft].md"];
    source
        .write(
            "notes/example.md",
            &format!(
                "---\nconcepts: {}\nfiles: {}\n---\nBody\n",
                serde_json::to_string(&concepts).unwrap(),
                serde_json::to_string(&files).unwrap()
            ),
            false,
        )
        .unwrap();
    let expected = (
        concepts
            .into_iter()
            .map(|item| (item.to_string(), 1))
            .collect(),
        files
            .into_iter()
            .map(|item| (item.to_string(), 1))
            .collect(),
    );
    assert_eq!(rankings(&memory_summary(&source, 10).unwrap()), expected);
    let archive = memory_export(&source, &ExportFilter::default()).unwrap();
    let (_destination_dir, destination) = service();
    memory_import(&destination, &archive, ImportStrategy::Merge, false).unwrap();
    assert_eq!(
        rankings(&memory_summary(&destination, 10).unwrap()),
        expected
    );
    let reexport: AmaArchive =
        serde_json::from_str(&memory_export(&destination, &ExportFilter::default()).unwrap())
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<String>>(
            &reexport
                .memories
                .iter()
                .find(|memory| memory.path == "notes/example.md")
                .unwrap()
                .frontmatter["concepts"]
        )
        .unwrap(),
        concepts
    );
}

#[test]
fn block_lists_count_each_literal_item_without_reading_body_lists() {
    let (_dir, svc) = service();
    svc.write(
        "notes/block.md",
        "---\nconcepts:\n  - parser, writer\n  - rust\nfiles:\n  - src/a,b.rs\n  - notes\\memory.md\n---\nconcepts:\n  - body-only\n",
        false,
    ).unwrap();
    let summary = memory_summary(&svc, 10).unwrap();
    assert_eq!(
        rankings(&summary),
        (
            BTreeMap::from([("parser, writer".into(), 1), ("rust".into(), 1)]),
            BTreeMap::from([("src/a,b.rs".into(), 1), (r"notes\memory.md".into(), 1)]),
        )
    );
}

#[test]
fn legacy_lists_empty_lists_and_unrelated_scalar_fields_keep_their_meaning() {
    let (_dir, svc) = service();
    svc.write(
        "facts/lesson/legacy.md",
        "---\nconcepts: [rust, 'memory']\nfiles: src/lib.rs, src/main.rs\ntitle: \"Legacy title\"\nsource: auto-consolidation\ncategory: lesson\n---\nBody\n",
        false,
    ).unwrap();
    svc.write("notes/empty.md", "---\nconcepts: []\nfiles:\n---\n", false)
        .unwrap();
    svc.write("notes/plain.md", "concepts: [body-only]\n", false)
        .unwrap();
    let summary = memory_summary(&svc, 10).unwrap();
    assert_eq!(summary.total_memories, 3);
    assert_eq!(summary.auto_created, 1);
    assert_eq!(summary.unknown_source, 2);
    assert_eq!(summary.by_source["auto-consolidation"], 1);
    assert_eq!(summary.by_category["lesson"], 1);
    assert!(
        summary
            .recent_memories
            .iter()
            .any(|entry| entry.title == "Legacy title")
    );
    assert_eq!(
        rankings(&summary),
        (
            BTreeMap::from([("rust".into(), 1), ("memory".into(), 1)]),
            BTreeMap::from([("src/lib.rs".into(), 1), ("src/main.rs".into(), 1)]),
        )
    );
}

#[test]
fn import_normalized_control_characters_remain_single_summary_entries() {
    let (_source_dir, source) = service();
    source
        .write(
            "notes/controls.md",
            &format!(
                "---\nconcepts: {}\nfiles: {}\n---\nBody\n",
                serde_json::to_string(&["parser,\nwriter", "tab\tconcept"]).unwrap(),
                serde_json::to_string(&["src/a,\nb.rs"]).unwrap(),
            ),
            false,
        )
        .unwrap();
    let mut archive = memory_export(&source, &ExportFilter::default()).unwrap();
    for _ in 0..2 {
        let (_dir, destination) = service();
        memory_import(&destination, &archive, ImportStrategy::Merge, false).unwrap();
        assert_eq!(
            rankings(&memory_summary(&destination, 10).unwrap()),
            (
                BTreeMap::from([("parser, writer".into(), 1), ("tab concept".into(), 1)]),
                BTreeMap::from([("src/a, b.rs".into(), 1)]),
            )
        );
        archive = memory_export(&destination, &ExportFilter::default()).unwrap();
    }
}
