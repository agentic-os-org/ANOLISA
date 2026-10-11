//! `mem_import(strategy=overwrite)` must not destroy what it claims to back up.
//!
//! The overwrite path deletes every `.md` file under the mount before writing
//! the incoming archive. It first exports the current store as an AMA JSON —
//! but that export used to be bound to a local, logged for its byte count, and
//! dropped, so nothing survived the delete. These tests pin the contract that
//! replaces it: the backup is a real file under `<mount>/.anolisa/backups/`, it
//! holds the deleted content, its path is reported to the caller and to the
//! audit log, retention is bounded, and an import whose backup cannot be
//! written deletes nothing.

use tempfile::tempdir;

use agent_memory::config::{AppConfig, Profile};
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_import::{ImportStrategy, memory_import};

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

/// An AMA archive carrying exactly one memory, as `mem_export` would emit.
fn one_memory_archive(path: &str, body: &str) -> String {
    format!(
        r#"{{
  "version": "1.0",
  "format": "anolisa-memory-archive",
  "exported_at": "2026-09-24T09:00:00Z",
  "agent_id": "test",
  "user_id": "tester",
  "total_memories": 1,
  "memories": [{{
    "path": "{path}",
    "frontmatter": {{"category": "lesson"}},
    "content": "{body}"
  }}],
  "tasks": [],
  "stats": {{"by_category": {{"lesson": 1}}, "by_source": {{}}, "total_bytes": 8}}
}}"#
    )
}

fn backups_dir(svc: &MemoryService) -> std::path::PathBuf {
    svc.mount.meta_dir.join("backups")
}

fn backup_files(svc: &MemoryService) -> Vec<String> {
    let dir = backups_dir(svc);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with("-pre-import.ama.json"))
        .collect();
    names.sort();
    names
}

#[test]
fn overwrite_import_persists_a_backup_holding_the_deleted_content() {
    let (_t, svc) = setup();
    svc.write(
        "facts/lesson/keepme.md",
        "IRREPLACEABLE PRIOR MEMORY",
        false,
    )
    .unwrap();

    let summary = memory_import(
        &svc,
        &one_memory_archive("facts/lesson/incoming.md", "new body"),
        ImportStrategy::Overwrite,
        false,
    )
    .unwrap();

    // The pre-existing memory is gone — that is what overwrite means.
    assert!(svc.read("facts/lesson/keepme.md").is_err());
    // read() returns the reconstructed file, frontmatter included.
    assert!(
        svc.read("facts/lesson/incoming.md")
            .unwrap()
            .contains("new body"),
        "the incoming memory should have been written"
    );

    // ...so the backup has to be a real file, not a log line.
    let backups = backup_files(&svc);
    assert_eq!(
        backups.len(),
        1,
        "expected exactly one pre-import backup, got {backups:?}"
    );

    let json = std::fs::read_to_string(backups_dir(&svc).join(&backups[0])).unwrap();
    let archive: serde_json::Value = serde_json::from_str(&json).unwrap();
    let paths: Vec<&str> = archive["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(&"facts/lesson/keepme.md"),
        "backup must contain the deleted memory, got paths {paths:?}"
    );
    assert!(
        json.contains("IRREPLACEABLE PRIOR MEMORY"),
        "backup must contain the deleted memory's content"
    );

    // The caller is told where the backup landed.
    assert!(
        summary.contains(&backups[0]),
        "summary should name the backup file, got: {summary}"
    );
}

#[test]
fn overwrite_import_audits_the_backup_and_every_counter() {
    let (_t, svc) = setup();
    svc.write("facts/lesson/old.md", "old body", false).unwrap();

    memory_import(
        &svc,
        &one_memory_archive("facts/lesson/new.md", "new body"),
        ImportStrategy::Overwrite,
        false,
    )
    .unwrap();

    let log = std::fs::read_to_string(svc.mount.audit_log_path()).unwrap();
    let entry = log
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["tool"] == "mem_import")
        .expect("mem_import must be audited");

    let detail = entry["path"].as_str().unwrap();
    assert!(
        detail.contains("1 imported"),
        "audit should count imports, got {detail:?}"
    );
    assert!(
        detail.contains("overwritten"),
        "audit must report the overwritten counter too, got {detail:?}"
    );
    assert!(
        detail.contains("errors"),
        "audit must report the error counter too, got {detail:?}"
    );
    assert!(
        detail.contains("backups/"),
        "audit must point at the backup so an operator can recover, got {detail:?}"
    );
    assert_eq!(entry["ok"], serde_json::json!(true));
}

#[test]
fn a_wholly_failed_import_is_not_audited_as_a_clean_success() {
    let (_t, svc) = setup();

    // `.anolisa` is a reserved first segment, so resolve_for_create rejects
    // every entry: nothing is imported and one error is reported.
    let out = memory_import(
        &svc,
        &one_memory_archive(".anolisa/evil.md", "nope"),
        ImportStrategy::SkipExisting,
        false,
    )
    .unwrap();
    assert!(out.contains("1 errors"), "got: {out}");

    let log = std::fs::read_to_string(svc.mount.audit_log_path()).unwrap();
    let entry = log
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["tool"] == "mem_import")
        .expect("mem_import must be audited");

    assert_eq!(
        entry["ok"],
        serde_json::json!(false),
        "an import that applied nothing and errored must not audit as ok"
    );
    assert!(
        entry["error"].as_str().unwrap_or("").contains(".anolisa"),
        "audit should carry the rejection reason, got {:?}",
        entry["error"]
    );
}

#[test]
fn failed_backup_aborts_the_overwrite_and_deletes_nothing() {
    let (_t, svc) = setup();
    svc.write("facts/lesson/keepme.md", "MUST SURVIVE", false)
        .unwrap();

    // Occupy the backups path with a regular file: create_dir_all then fails
    // for any uid (including root), which is exactly the "cannot persist the
    // backup" case the import must refuse to survive.
    std::fs::create_dir_all(svc.mount.meta_dir.join("backups").parent().unwrap()).unwrap();
    std::fs::write(svc.mount.meta_dir.join("backups"), b"not a directory").unwrap();

    let err = memory_import(
        &svc,
        &one_memory_archive("facts/lesson/incoming.md", "new body"),
        ImportStrategy::Overwrite,
        false,
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("overwrite"),
        "error should say the overwrite was refused, got: {err}"
    );
    assert_eq!(
        svc.read("facts/lesson/keepme.md").unwrap(),
        "MUST SURVIVE",
        "a refused overwrite must leave the store untouched"
    );
    assert!(svc.read("facts/lesson/incoming.md").is_err());
}

#[test]
fn backup_retention_is_bounded() {
    let (_t, svc) = setup();
    svc.write("facts/lesson/seed.md", "seed body", false)
        .unwrap();

    // One more overwrite than the retention bound; each pass re-exports the
    // store, so the newest backup always holds the previous generation.
    for i in 0..12 {
        memory_import(
            &svc,
            &one_memory_archive(&format!("facts/lesson/gen{i}.md"), &format!("body {i}")),
            ImportStrategy::Overwrite,
            false,
        )
        .unwrap();
    }

    let backups = backup_files(&svc);
    assert_eq!(
        backups.len(),
        8,
        "backups must be pruned to the retention bound, got {}: {backups:?}",
        backups.len()
    );
    // Pruning drops the oldest, so the survivors are the most recent ones. The
    // backup taken during the final pass holds the generation that pass then
    // deleted, which is what makes it recoverable.
    let newest = std::fs::read_to_string(backups_dir(&svc).join(backups.last().unwrap())).unwrap();
    assert!(
        newest.contains("gen10.md"),
        "newest backup should hold the generation the last overwrite deleted"
    );
    assert!(svc.read("facts/lesson/gen11.md").is_ok());
}

#[test]
fn dry_run_overwrite_writes_no_backup_and_deletes_nothing() {
    let (_t, svc) = setup();
    svc.write("facts/lesson/keepme.md", "MUST SURVIVE", false)
        .unwrap();

    let out = memory_import(
        &svc,
        &one_memory_archive("facts/lesson/incoming.md", "new body"),
        ImportStrategy::Overwrite,
        true,
    )
    .unwrap();

    assert!(out.starts_with("[DRY RUN]"), "got: {out}");
    assert_eq!(svc.read("facts/lesson/keepme.md").unwrap(), "MUST SURVIVE");
    assert!(
        backup_files(&svc).is_empty(),
        "a dry run mutates nothing, so it must not leave a backup behind"
    );
}

#[test]
fn a_symlinked_backups_dir_refuses_the_overwrite_and_writes_nothing_outside_the_mount() {
    let (t, svc) = setup();
    svc.write("facts/lesson/keepme.md", "MUST SURVIVE", false)
        .unwrap();

    // A co-tenant who can write to the mount points `.anolisa/backups` at a
    // directory outside it. `File::create` follows that link, so the archive
    // lands out of the mount while the import goes on to delete every memory
    // and reports an in-mount recovery path that does not exist.
    let outside = t.path().join("outside-the-mount");
    std::fs::create_dir_all(&outside).unwrap();
    let backups = backups_dir(&svc);
    let _ = std::fs::remove_dir(&backups);
    std::os::unix::fs::symlink(&outside, &backups).unwrap();

    let err = memory_import(
        &svc,
        &one_memory_archive("facts/lesson/incoming.md", "new body"),
        ImportStrategy::Overwrite,
        false,
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("overwrite"),
        "error should say the overwrite was refused, got: {err}"
    );
    assert!(
        std::fs::read_dir(&outside).unwrap().next().is_none(),
        "a refused backup must not write outside the mount, found {:?}",
        std::fs::read_dir(&outside)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        svc.read("facts/lesson/keepme.md").unwrap(),
        "MUST SURVIVE",
        "a refused overwrite must leave the store untouched"
    );
    assert!(svc.read("facts/lesson/incoming.md").is_err());
}

#[test]
fn pruning_never_counts_or_removes_a_planted_link_named_like_a_backup() {
    let (_t, svc) = setup();
    svc.write("facts/lesson/seed.md", "seed body", false)
        .unwrap();

    // Nine real backups, i.e. one over the retention bound.
    for i in 0..9 {
        memory_import(
            &svc,
            &one_memory_archive(&format!("facts/lesson/gen{i}.md"), &format!("body {i}")),
            ImportStrategy::Overwrite,
            false,
        )
        .unwrap();
    }

    // A symlink wearing a backup's name, sorting older than every real one.
    // Pruning enumerates and deletes by name, so an unanchored walk counts it
    // toward retention and unlinks it — a file this import never wrote.
    let planted = backups_dir(&svc).join("00000000T00000000.000000Z-pre-import.ama.json");
    std::os::unix::fs::symlink("/nonexistent-anolisa-target", &planted).unwrap();

    memory_import(
        &svc,
        &one_memory_archive("facts/lesson/final.md", "final body"),
        ImportStrategy::Overwrite,
        false,
    )
    .unwrap();

    assert!(
        std::fs::symlink_metadata(&planted)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false),
        "retention must leave a file it does not own alone"
    );
    // The test helper matches on the name alone, so it still sees the link;
    // count what is actually on disk as a regular file.
    let real = backup_files(&svc)
        .into_iter()
        .filter(|n| {
            backups_dir(&svc)
                .join(n)
                .symlink_metadata()
                .map(|m| m.file_type().is_file())
                .unwrap_or(false)
        })
        .count();
    assert_eq!(real, 8, "the retention bound counts real backups only");
}
