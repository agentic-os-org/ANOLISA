use agent_memory::config::AppConfig;
use agent_memory::consolidation::{ConsolidatedFact, FactCategory, FactWriter};
use agent_memory::mount::MountStrategyKind;
use agent_memory::service::MemoryService;
use agent_memory::tools::session_context::memory_session_context;
use std::time::{Duration, Instant};

#[test]
fn writer_retired_fact_is_excluded_from_session_context() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "context-writer-probe".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("sessions").to_string_lossy().into();
    cfg.memory.mount.strategy = MountStrategyKind::Userland;
    let svc = MemoryService::new(cfg).unwrap();
    let index = svc.index.as_ref().unwrap();
    let prefix = format!(
        "{:<100}",
        "Deployment endpoint configuration should use the current port confirmed by the service owner"
    );
    assert_eq!(prefix.len(), 100);
    let fact = |suffix: &str, confidence| {
        ConsolidatedFact::new(
            "session",
            FactCategory::WorkingContext,
            "Deployment endpoint".into(),
            format!("{prefix}{suffix}"),
            "mem_write".into(),
            vec![],
            confidence,
        )
    };
    let old = fact("OBSOLETE_CHOICE_OLD", 0.95);
    let current = fact("CURRENT_CHOICE_NEW", 0.9);
    let writer = FactWriter::new(&svc.mount.root);
    writer.write(&old).unwrap();
    let old_rel = format!("facts/working-context/{}.md", old.id);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if index
            .search("OBSOLETE_CHOICE_OLD", 5)
            .unwrap()
            .iter()
            .any(|hit| hit.path == old_rel)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not index first fact"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let query = format!(
        "{} {}",
        current.title,
        current.content.chars().take(100).collect::<String>()
    );
    {
        let shared = index.store_arc();
        let store = shared.lock().unwrap();
        let conflicts = store.detect_conflicts(&query, -2.0).unwrap();
        println!("CONFLICT_QUERY={query:?} CONFLICTS={conflicts:?}");
        assert!(
            conflicts.iter().any(|(path, _)| path == &old_rel),
            "production writer conflict precondition not satisfied"
        );
    }
    writer
        .with_index(index.store_arc(), -2.0)
        .write(&current)
        .unwrap();
    let old_path = svc
        .mount
        .root
        .join("facts/working-context")
        .join(format!("{}.md", old.id));
    let marked = std::fs::read_to_string(old_path).unwrap();
    assert!(
        marked.contains(&format!("superseded_by: {}", current.id)),
        "writer did not supersede old fact: {marked}"
    );
    assert!(index.search("OBSOLETE_CHOICE_OLD", 5).unwrap().is_empty());
    let context = memory_session_context(&svc, Some(5)).unwrap();
    println!("MARKED_OLD={marked}\nCONTEXT={context}");
    assert!(context.contains("CURRENT_CHOICE_NEW"));
    assert!(
        !context.contains("OBSOLETE_CHOICE_OLD"),
        "superseded fact was injected: {context}"
    );
}

fn setup(index_enabled: bool) -> (tempfile::TempDir, MemoryService) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.global.user_id = "active-context-test".into();
    cfg.memory.paths.base_dir = tmp.path().to_string_lossy().into();
    cfg.memory.session.base_dir = tmp.path().join("sessions").to_string_lossy().into();
    cfg.memory.mount.strategy = MountStrategyKind::Userland;
    cfg.memory.index.enabled = index_enabled;
    let svc = MemoryService::new(cfg).unwrap();
    (tmp, svc)
}

fn write_fact(
    svc: &MemoryService,
    name: &str,
    category: &str,
    confidence: f64,
    date: &str,
    marker: &str,
    body: &str,
) -> String {
    let path = format!("facts/{category}/{name}.md");
    let content = format!(
        "---\ncategory: {category}\ntitle: {name}\nconfidence: {confidence}\ncreated_at: {date}\n{marker}---\n\n{body}"
    );
    svc.write(&path, &content, false).unwrap();
    path
}

#[test]
fn disk_retirement_works_without_index_and_keeps_legacy_facts() {
    let (_tmp, svc) = setup(false);
    write_fact(
        &svc,
        "retired",
        "lesson",
        0.99,
        "2026-10-04",
        "superseded_by: replacement\n",
        "RETIRED_BODY",
    );
    write_fact(
        &svc,
        "legacy",
        "lesson",
        0.9,
        "2026-10-01",
        "",
        "LEGACY_BODY",
    );
    write_fact(
        &svc,
        "empty",
        "lesson",
        0.9,
        "2026-10-01",
        "superseded_by: \"\"\n",
        "EMPTY_MARKER_BODY",
    );
    write_fact(
        &svc,
        "ordinary",
        "lesson",
        0.9,
        "2026-10-01",
        "",
        "Body mention superseded_by: does not retire this fact",
    );
    let context = memory_session_context(&svc, Some(10)).unwrap();
    assert!(!context.contains("RETIRED_BODY"));
    assert!(context.contains("LEGACY_BODY"));
    assert!(context.contains("EMPTY_MARKER_BODY"));
    assert!(context.contains("Body mention superseded_by:"));
}

#[test]
fn database_retirement_works_when_disk_marker_is_absent() {
    let (_tmp, svc) = setup(true);
    let rel = write_fact(
        &svc,
        "db-retired",
        "lesson",
        0.99,
        "2026-10-04",
        "",
        "DB_ONLY_RETIRED_BODY",
    );
    write_fact(
        &svc,
        "active",
        "lesson",
        0.9,
        "2026-10-01",
        "",
        "ACTIVE_BODY",
    );
    let index = svc.index.as_ref().unwrap();
    index
        .reindex_file(&rel, "DB_ONLY_RETIRED_BODY", 1, 20)
        .unwrap();
    {
        let shared = index.store_arc();
        let mut store = shared.lock().unwrap();
        store.supersede(&rel, "replacement").unwrap();
    }
    // Model an absent best-effort marker while retaining authoritative DB state.
    let path = svc.mount.root.join(&rel);
    let marked = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, marked.replace("superseded_by: replacement\n", "")).unwrap();
    assert!(index.superseded_paths().unwrap().contains(&rel));
    let context = memory_session_context(&svc, Some(5)).unwrap();
    assert!(!context.contains("DB_ONLY_RETIRED_BODY"));
    assert!(context.contains("ACTIVE_BODY"));
}

#[test]
fn retirement_is_filtered_before_limits_and_sorting() {
    let (_tmp, svc) = setup(false);
    write_fact(
        &svc,
        "old-best",
        "lesson",
        0.99,
        "2026-10-04",
        "superseded_by: new\n",
        "RETIRED_BEST",
    );
    write_fact(
        &svc,
        "new-best",
        "lesson",
        0.9,
        "2026-10-01",
        "",
        "CURRENT_BEST",
    );
    write_fact(
        &svc,
        "lower",
        "lesson",
        0.8,
        "2026-10-01",
        "",
        "LOWER_CONFIDENCE",
    );
    write_fact(
        &svc,
        "old-summary",
        "summary",
        0.9,
        "2026-10-04",
        "superseded_by: new\n",
        "RETIRED_SUMMARY",
    );
    write_fact(
        &svc,
        "new-summary",
        "summary",
        0.9,
        "2026-10-03",
        "",
        "CURRENT_SUMMARY",
    );
    write_fact(
        &svc,
        "older-summary",
        "summary",
        0.9,
        "2026-10-01",
        "",
        "OLDER_SUMMARY",
    );
    let context = memory_session_context(&svc, Some(1)).unwrap();
    assert!(context.contains("CURRENT_BEST"));
    assert!(context.contains("CURRENT_SUMMARY"));
    for excluded in [
        "RETIRED_BEST",
        "RETIRED_SUMMARY",
        "LOWER_CONFIDENCE",
        "OLDER_SUMMARY",
    ] {
        assert!(
            !context.contains(excluded),
            "unexpected {excluded}: {context}"
        );
    }
}

#[test]
fn lifecycle_read_failure_is_visible() {
    let (_tmp, svc) = setup(true);
    let index = svc.index.as_ref().unwrap();
    let connection = rusqlite::Connection::open(index.db_path()).unwrap();
    connection
        .execute("ALTER TABLE files RENAME TO unavailable_files", [])
        .unwrap();
    assert!(memory_session_context(&svc, Some(5)).is_err());
}
