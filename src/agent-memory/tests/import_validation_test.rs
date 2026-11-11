//! Import validation contract: AMA archives only ever contain `.md` memory
//! paths (the writer side filters its walk to markdown), so a non-`.md`
//! entry, an empty path, or a duplicate path is malformed input that must be
//! reported and skipped — never written, since writing it would clobber
//! non-memory files (the fact log, the generated index) with markdown.

use agent_memory::config::AppConfig;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_import::{ImportStrategy, memory_import};
use std::os::unix::fs::{PermissionsExt, symlink};
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
    // Overwrite is the reachable strategy that actually truncates the file
    // on the unfixed code; with skip-existing the entry was skipped for an
    // unrelated reason and the corruption assert passed either way.
    let out = memory_import(&svc, &json, ImportStrategy::Overwrite, false).unwrap();

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
    // Pin the new code path's message — ns::paths already rejected "" with
    // "empty path" before this change, so that assert was green on main.
    assert!(
        out.contains("archive entry has an empty path"),
        "summary: {out}"
    );
    assert!(
        out.contains("<empty>: "),
        "empty entries render a placeholder label: {out}"
    );
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
fn import_dedups_path_spellings_that_resolve_to_one_file() {
    // "notes/a.md/" and "notes//a.md" pass the .md check and resolve to the
    // same inode as "notes/a.md"; the raw-string seen-set treated them as
    // distinct and silently double-wrote (or phantom-skipped). The
    // normalized key must report the second spelling as a duplicate.
    let (_tmp, svc) = setup();
    let json = archive(&format!("{},{}", entry("notes/a.md"), entry("notes/a.md/")));
    let out = memory_import(&svc, &json, ImportStrategy::Merge, false).unwrap();
    assert!(out.contains("duplicate"), "summary: {out}");
    assert!(out.contains("1 imported"), "summary: {out}");
    assert!(out.contains("1 errors"), "summary: {out}");
    assert_eq!(
        std::fs::read_to_string(svc.mount.root.join("notes/a.md")).unwrap(),
        "---\nid: e\n---\n\nbody\n",
        "exactly one write reached the file"
    );

    // The double-slash spelling collapses the same way.
    let (_tmp, svc) = setup();
    let json = archive(&format!("{},{}", entry("notes/a.md"), entry("notes//a.md")));
    let out = memory_import(&svc, &json, ImportStrategy::Merge, false).unwrap();
    assert!(out.contains("duplicate"), "summary: {out}");
    assert!(out.contains("1 imported"), "summary: {out}");
}

#[test]
fn import_overwrite_rejects_invalid_archive_before_deleting_anything() {
    // A 100%-invalid archive under the overwrite strategy must be rejected
    // before the store is wiped: the pre-pass returns an error and no
    // existing .md file is touched.
    let (_tmp, svc) = setup();
    svc.write("notes/keep.md", "keep me", false).unwrap();
    let json = archive(&entry("facts/facts.jsonl"));
    let out = memory_import(&svc, &json, ImportStrategy::Overwrite, false).unwrap();
    // The invalid entry is reported per-entry (no whole-archive rejection:
    // valid entries in a partly-malformed archive still import), and the
    // destructive wipe never ran.
    assert!(out.contains("1 errors"), "summary: {out}");
    assert!(out.contains("0 imported"), "summary: {out}");
    assert!(
        svc.mount.root.join("notes/keep.md").exists(),
        "an invalid archive must not delete existing memories"
    );
    assert_eq!(
        std::fs::read_to_string(svc.mount.root.join("notes/keep.md")).unwrap(),
        "keep me"
    );
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

#[test]
fn overwrite_preserves_memories_for_unsafe_memory_and_task_paths() {
    for path in [
        "/tmp/import-outside.md",
        "../escape.md",
        ".anolisa/inject.md",
        ".git/config.md",
    ] {
        for bucket in ["memories", "tasks"] {
            for mixed in [false, true] {
                let (_tmp, svc) = setup();
                svc.write("notes/keep.md", "ORIGINAL\n", false).unwrap();
                let mut json: serde_json::Value = serde_json::from_str(&archive("")).unwrap();
                json[bucket] = serde_json::json!([
                    {"path": path, "frontmatter": {}, "content": "invalid"}
                ]);
                if mixed {
                    json["memories"].as_array_mut().unwrap().insert(
                        0,
                        serde_json::json!({
                            "path": "notes/legal.md", "frontmatter": {}, "content": "NEW\n"
                        }),
                    );
                }
                let out = memory_import(&svc, &json.to_string(), ImportStrategy::Overwrite, false)
                    .unwrap();
                assert!(out.contains("1 errors"), "{path}, {bucket}: {out}");
                assert_eq!(
                    std::fs::read(svc.mount.root.join("notes/keep.md")).unwrap(),
                    b"ORIGINAL\n",
                    "{path}, {bucket}, mixed={mixed}"
                );
                assert!(!out.contains("Backup:"), "no destructive wipe: {out}");
                if mixed {
                    assert!(out.contains("1 imported"), "{out}");
                    assert_eq!(
                        std::fs::read(svc.mount.root.join("notes/legal.md")).unwrap(),
                        b"NEW\n"
                    );
                } else {
                    assert!(out.contains("0 imported"), "{out}");
                }
            }
        }
    }
}

#[test]
fn overwrite_preserves_memories_when_an_entry_traverses_a_symlink() {
    let (_tmp, svc) = setup();
    let outside = tempdir().unwrap();
    svc.write("notes/keep.md", "ORIGINAL\n", false).unwrap();
    symlink(outside.path(), svc.mount.root.join("link")).unwrap();
    // A missing immediate parent still has an existing symlink ancestor.
    let json = archive(&entry("link/new/escape.md"));
    let out = memory_import(&svc, &json, ImportStrategy::Overwrite, false).unwrap();
    assert!(out.contains("1 errors"), "{out}");
    assert_eq!(
        std::fs::read(svc.mount.root.join("notes/keep.md")).unwrap(),
        b"ORIGINAL\n"
    );
    assert!(!outside.path().join("new").exists());
}

#[test]
fn overwrite_stops_when_backup_export_fails() {
    let (_tmp, svc) = setup();
    svc.write("notes/keep.md", "ORIGINAL\n", false).unwrap();
    let unreadable = svc.mount.root.join("notes/invalid-utf8.md");
    std::fs::write(&unreadable, b"\xff\xfe").unwrap();
    let error = memory_import(
        &svc,
        &archive(&entry("notes/new.md")),
        ImportStrategy::Overwrite,
        false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("backup"), "{error}");
    assert!(error.to_string().contains("invalid-utf8.md"), "{error}");
    assert_eq!(std::fs::read(unreadable).unwrap(), b"\xff\xfe");
    assert_eq!(
        std::fs::read(svc.mount.root.join("notes/keep.md")).unwrap(),
        b"ORIGINAL\n"
    );
    assert!(!svc.mount.root.join("notes/new.md").exists());
}

#[test]
fn overwrite_stops_when_backup_cannot_be_persisted() {
    let (_tmp, svc) = setup();
    svc.write("notes/keep.md", "ORIGINAL\n", false).unwrap();
    // A regular file at the metadata path fails regardless of the test UID.
    let metadata = svc.mount.root.join("saved-metadata");
    std::fs::rename(&svc.mount.meta_dir, &metadata).unwrap();
    std::fs::write(&svc.mount.meta_dir, "not a directory").unwrap();
    let error = memory_import(
        &svc,
        &archive(&entry("notes/new.md")),
        ImportStrategy::Overwrite,
        false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("backup"), "{error}");
    assert_eq!(
        std::fs::read(svc.mount.root.join("notes/keep.md")).unwrap(),
        b"ORIGINAL\n"
    );
    assert!(!svc.mount.root.join("notes/new.md").exists());
}

#[test]
fn overwrite_preserves_file_ancestors_excluded_from_the_wipe() {
    for ancestor in ["notes/data.txt", ".github/old.md"] {
        let (_tmp, svc) = setup();
        svc.write("notes/keep.md", "ORIGINAL\n", false).unwrap();
        svc.write(ancestor, "DATA\n", false).unwrap();
        let json = archive(&entry(&format!("{ancestor}/new.md")));
        for dry_run in [true, false] {
            let out = memory_import(&svc, &json, ImportStrategy::Overwrite, dry_run).unwrap();
            assert!(out.contains("1 errors"), "{out}");
            assert!(!out.contains("Backup:"), "{out}");
            assert_eq!(
                std::fs::read(svc.mount.root.join(ancestor)).unwrap(),
                b"DATA\n"
            );
            assert_eq!(
                std::fs::read(svc.mount.root.join("notes/keep.md")).unwrap(),
                b"ORIGINAL\n"
            );
        }
    }
}

#[test]
fn malformed_overwrite_does_not_treat_file_ancestors_as_cleared() {
    let (_tmp, svc) = setup();
    svc.write("notes/old.md", "ORIGINAL\n", false).unwrap();
    let json = archive(&format!(
        "{},{}",
        entry("notes/old.md/new.md"),
        entry("../escape.md")
    ));
    for dry_run in [true, false] {
        let out = memory_import(&svc, &json, ImportStrategy::Overwrite, dry_run).unwrap();
        assert!(out.contains("2 errors"), "{out}");
        assert!(out.contains("0 imported"), "{out}");
        assert!(!out.contains("Backup:"), "{out}");
        assert_eq!(
            std::fs::read(svc.mount.root.join("notes/old.md")).unwrap(),
            b"ORIGINAL\n"
        );
    }
}

#[test]
fn overwrite_persists_a_private_recoverable_backup() {
    let (_tmp, svc) = setup();
    svc.write("notes/keep.md", "ORIGINAL\n", false).unwrap();
    let json = archive(&entry("notes/new.md"));
    let preview = memory_import(&svc, &json, ImportStrategy::Overwrite, true).unwrap();
    assert!(!preview.contains("Backup:"), "{preview}");
    let out = memory_import(&svc, &json, ImportStrategy::Overwrite, false).unwrap();
    assert!(out.contains("1 imported"), "{out}");
    assert!(out.contains("0 errors"), "{out}");
    assert!(!svc.mount.root.join("notes/keep.md").exists());
    let backup = std::path::Path::new(out.split_once("\nBackup: ").unwrap().1);
    assert_eq!(backup.parent().unwrap(), svc.mount.meta_dir);
    assert_eq!(
        std::fs::metadata(backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let saved = std::fs::read_to_string(backup).unwrap();
    let (_recovery_tmp, recovery) = setup();
    let restored = memory_import(&recovery, &saved, ImportStrategy::SkipExisting, false).unwrap();
    assert!(restored.contains("0 errors"), "{restored}");
    assert_eq!(
        std::fs::read(recovery.mount.root.join("notes/keep.md")).unwrap(),
        b"ORIGINAL\n"
    );
    assert!(!recovery.mount.root.join("notes/new.md").exists());
    let second = memory_import(&svc, &json, ImportStrategy::Overwrite, false).unwrap();
    assert_ne!(
        second.split_once("\nBackup: ").unwrap().1,
        backup.to_str().unwrap()
    );
    assert_eq!(std::fs::read_to_string(backup).unwrap(), saved);
}
