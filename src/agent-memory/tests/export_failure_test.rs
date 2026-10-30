//! An export must not report a complete archive after skipping unreadable notes.

use agent_memory::config::AppConfig;
use agent_memory::error::MemoryError;
use agent_memory::service::MemoryService;
use agent_memory::tools::memory_export::{ExportFilter, memory_export};
use std::os::unix::fs::PermissionsExt;

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.memory.paths.base_dir = tmp.path().join("data").to_string_lossy().into_owned();
    cfg.memory.session.base_dir = tmp.path().join("sessions").to_string_lossy().into_owned();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.index.enabled = false;
    cfg.memory.git.enabled = false;
    (tmp, MemoryService::new(cfg).unwrap())
}

struct RestorePermissions(std::path::PathBuf, std::fs::Permissions);

impl RestorePermissions {
    fn deny(path: &std::path::Path) -> Self {
        let original = std::fs::metadata(path).unwrap().permissions();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        Self(path.to_owned(), original)
    }
}

impl Drop for RestorePermissions {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, self.1.clone());
    }
}

fn assert_failed_export_audit(svc: &MemoryService, path: &str) {
    let log = std::fs::read_to_string(svc.mount.meta_dir.join("audit.log")).unwrap();
    let exports: Vec<serde_json::Value> = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|entry| entry["tool"] == "mem_export")
        .collect();
    assert_eq!(exports.len(), 1, "{exports:?}");
    assert_eq!(exports[0]["ok"], false);
    assert_eq!(exports[0]["path"], path);
    assert!(exports[0]["error"].as_str().unwrap().contains(path));
}

#[test]
fn export_reports_invalid_utf8_instead_of_a_partial_archive() {
    let (_tmp, svc) = setup();
    svc.write("notes/good.md", "keep this memory", false)
        .unwrap();
    std::fs::write(svc.mount.root.join("notes/broken.md"), b"note\xff").unwrap();
    let result = memory_export(&svc, &ExportFilter::default());
    assert!(
        result.is_err(),
        "incomplete export was reported as successful: {result:?}"
    );
    let error = result.unwrap_err();
    assert!(matches!(&error, MemoryError::Io(e) if e.kind() == std::io::ErrorKind::InvalidData));
    assert!(error.to_string().contains("notes/broken.md"), "{error}");
    assert_failed_export_audit(&svc, "notes/broken.md");
}

#[test]
fn export_reports_unreadable_note_when_not_root() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: root bypasses file read permissions");
        return;
    }
    let (_tmp, svc) = setup();
    svc.write("private.md", "must not disappear", false)
        .unwrap();
    let path = svc.mount.root.join("private.md");
    let _permissions = RestorePermissions::deny(&path);
    let result = memory_export(&svc, &ExportFilter::default());
    let error = result
        .expect_err("unreadable memory must fail the export")
        .to_string();
    assert!(error.contains("private.md"), "{error}");
    assert_failed_export_audit(&svc, "private.md");
}

#[test]
fn export_reports_unreadable_subtree_when_not_root() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: root bypasses directory read permissions");
        return;
    }
    let (_tmp, svc) = setup();
    svc.write("notes/private/keep.md", "must not disappear", false)
        .unwrap();
    let path = svc.mount.root.join("notes/private");
    let _permissions = RestorePermissions::deny(&path);
    let result = memory_export(&svc, &ExportFilter::default());
    let error = result.expect_err("unscanned subtree must fail the export");
    assert!(
        matches!(&error, MemoryError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied)
    );
    assert!(error.to_string().contains("notes/private"), "{error}");
    assert_failed_export_audit(&svc, "notes/private");
}

#[test]
fn export_still_excludes_metadata_and_non_markdown_files() {
    let (_tmp, svc) = setup();
    svc.write("notes/good.md", "visible", false).unwrap();
    std::fs::write(svc.mount.meta_dir.join("internal.md"), b"invalid\xff").unwrap();
    std::fs::write(svc.mount.root.join("binary.dat"), b"invalid\xff").unwrap();
    let archive: serde_json::Value =
        serde_json::from_str(&memory_export(&svc, &ExportFilter::default()).unwrap()).unwrap();
    assert!(
        archive["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["path"] == "notes/good.md")
    );
    let empty: serde_json::Value = serde_json::from_str(
        &memory_export(
            &svc,
            &ExportFilter {
                category: Some("absent".into()),
                ..Default::default()
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(empty["total_memories"], 0);
}

#[test]
fn missing_mount_root_is_not_an_empty_export() {
    let (tmp, svc) = setup();
    std::fs::rename(&svc.mount.root, tmp.path().join("detached-store")).unwrap();
    let error = memory_export(&svc, &ExportFilter::default()).unwrap_err();
    assert!(matches!(&error, MemoryError::Io(e) if e.kind() == std::io::ErrorKind::NotFound));
    assert!(
        error.to_string().contains("mount root is missing '.'"),
        "{error}"
    );
    assert!(
        !svc.mount.root.exists(),
        "export must not recreate a missing store"
    );
}

#[test]
fn export_skips_an_unreadable_metadata_directory_when_not_root() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: root bypasses directory read permissions");
        return;
    }
    let (_tmp, svc) = setup();
    svc.write("notes/good.md", "visible", false).unwrap();
    let _permissions = RestorePermissions::deny(&svc.mount.meta_dir);
    let archive: serde_json::Value =
        serde_json::from_str(&memory_export(&svc, &ExportFilter::default()).unwrap()).unwrap();
    assert!(
        archive["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "notes/good.md")
    );
}

fn assert_export_without_tasks(svc: &MemoryService) {
    let archive: serde_json::Value = serde_json::from_str(
        &memory_export(
            svc,
            &ExportFilter {
                include_tasks: false,
                ..Default::default()
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert!(archive["tasks"].as_array().unwrap().is_empty());
    assert!(
        archive["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "notes/good.md")
    );

    let log = std::fs::read_to_string(svc.mount.meta_dir.join("audit.log")).unwrap();
    let exports: Vec<serde_json::Value> = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|entry| entry["tool"] == "mem_export")
        .collect();
    assert_eq!(exports.len(), 1, "{exports:?}");
    assert_eq!(exports[0]["ok"], true);
}

#[test]
fn export_skips_invalid_utf8_tasks_when_excluded() {
    let (_tmp, svc) = setup();
    svc.write("notes/good.md", "keep this memory", false)
        .unwrap();
    svc.write("tasks/nested/broken.md", "task", false).unwrap();
    std::fs::write(svc.mount.root.join("tasks/nested/broken.md"), b"task\xff").unwrap();

    assert_export_without_tasks(&svc);
    let error = memory_export(&svc, &ExportFilter::default()).unwrap_err();
    assert!(matches!(&error, MemoryError::Io(e) if e.kind() == std::io::ErrorKind::InvalidData));
    assert!(error.to_string().contains("tasks/nested/broken.md"));
}

#[test]
fn export_skips_unreadable_task_files_when_excluded() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: root bypasses file read permissions");
        return;
    }
    let (_tmp, svc) = setup();
    svc.write("notes/good.md", "keep this memory", false)
        .unwrap();
    svc.write("tasks/private.md", "task", false).unwrap();
    let _permissions = RestorePermissions::deny(&svc.mount.root.join("tasks/private.md"));

    assert_export_without_tasks(&svc);
    let error = memory_export(&svc, &ExportFilter::default()).unwrap_err();
    assert!(
        matches!(&error, MemoryError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied)
    );
    assert!(error.to_string().contains("tasks/private.md"));
}

#[test]
fn export_prunes_unreadable_task_directories_when_excluded() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: root bypasses directory read permissions");
        return;
    }
    for relative in ["tasks", "tasks/nested"] {
        let (_tmp, svc) = setup();
        svc.write("notes/good.md", "keep this memory", false)
            .unwrap();
        svc.write("tasks/nested/keep.md", "task", false).unwrap();
        let _permissions = RestorePermissions::deny(&svc.mount.root.join(relative));

        assert_export_without_tasks(&svc);
        let error = memory_export(&svc, &ExportFilter::default()).unwrap_err();
        assert!(
            matches!(&error, MemoryError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied)
        );
        assert!(error.to_string().contains(relative), "{error}");
    }
}

#[test]
fn task_exclusion_preserves_similar_memory_paths_and_included_tasks() {
    let (_tmp, svc) = setup();
    for path in [
        "tasks/keep.md",
        "tasks-other/keep.md",
        "notes/tasks/keep.md",
        "tasks.md",
    ] {
        svc.write(path, "keep", false).unwrap();
    }
    for include_tasks in [false, true] {
        let archive: serde_json::Value = serde_json::from_str(
            &memory_export(
                &svc,
                &ExportFilter {
                    include_tasks,
                    ..Default::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
        let tasks = archive["tasks"].as_array().unwrap();
        if include_tasks {
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0]["path"], "tasks/keep.md");
        } else {
            assert!(tasks.is_empty());
        }
        let memories = archive["memories"].as_array().unwrap();
        for path in ["tasks-other/keep.md", "notes/tasks/keep.md", "tasks.md"] {
            assert!(memories.iter().any(|entry| entry["path"] == path), "{path}");
        }
    }
}

#[test]
fn excluding_tasks_does_not_hide_unreadable_memory_metadata() {
    let (_tmp, svc) = setup();
    svc.write("notes/broken.md", "note", false).unwrap();
    std::fs::write(svc.mount.root.join("notes/broken.md"), b"note\xff").unwrap();
    let error = memory_export(
        &svc,
        &ExportFilter {
            include_tasks: false,
            category: Some("unmatched".into()),
            source: Some("unmatched".into()),
        },
    )
    .unwrap_err();
    assert!(matches!(&error, MemoryError::Io(e) if e.kind() == std::io::ErrorKind::InvalidData));
    assert!(error.to_string().contains("notes/broken.md"));
    assert_failed_export_audit(&svc, "notes/broken.md");
}
