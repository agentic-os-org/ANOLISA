//! Memory import — deserialize an Anolisa Memory Archive (AMA) JSON into the
//! memory store. Supports merge/overwrite/skip-existing strategies.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;
use std::path::Path;

use chrono::Utc;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::safe_fs;
use crate::service::MemoryService;

use super::memory_export::{AmaArchive, ExportedMemory};

/// Import strategy for handling conflicts with existing memories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStrategy {
    /// Merge: update existing memories, add new ones (default).
    Merge,
    /// Overwrite: delete all existing memories first, then import.
    Overwrite,
    /// Skip existing: only import memories whose path doesn't already exist.
    SkipExisting,
}

impl ImportStrategy {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "merge" => Ok(Self::Merge),
            "overwrite" => Ok(Self::Overwrite),
            "skip-existing" | "skip" => Ok(Self::SkipExisting),
            _ => Err(MemoryError::InvalidArgument(format!(
                "unknown import strategy '{s}'; expected merge, overwrite, or skip-existing"
            ))),
        }
    }
}

/// Result of an import operation.
#[derive(Debug)]
pub struct ImportReport {
    pub imported: usize,
    pub skipped: usize,
    pub overwritten: usize,
    pub errors: Vec<String>,
}

/// Sub-directory of `<mount>/.anolisa/` holding pre-overwrite import backups.
/// Inside the meta dir so every walk of the store — `mem_export`, `mem_list`,
/// snapshot creation — skips it, and so no tool can write to it (`.anolisa` is
/// a reserved first segment).
const BACKUPS_DIR: &str = "backups";

/// Suffix every backup carries, so pruning only ever touches files written
/// here and never a stray file an operator dropped into the same directory.
const BACKUP_SUFFIX: &str = "-pre-import.ama.json";

/// Backups retained per mount. Each overwrite import copies the whole store,
/// so an unbounded directory would let a client calling `mem_import` in a loop
/// fill the disk. The oldest — lowest timestamp-prefixed name — go first.
const MAX_BACKUPS: usize = 8;

/// Errors carried into the audit line. The full list already goes back to the
/// caller in the tool result; the audit log has to stay one short line.
const MAX_AUDITED_ERRORS: usize = 3;

/// Persist `archive_json` (a full AMA export of the store as it stands) and
/// return its mount-relative path.
///
/// Durable-or-fatal, and "durable" carries two requirements here. The bytes
/// go through tmp file → fsync → rename → fsync of the directory, the same
/// sequence `snapshot::tar::create_tarball` uses; a backup sitting only in
/// the page cache can be lost to the very power failure that interrupted the
/// import, which is the one moment it would be needed. And the directory the
/// bytes land in is created through rooted, no-follow descriptors that fsync
/// the parent holding its dirent — `std::fs::create_dir_all` does neither, so
/// the first overwrite could create `.anolisa/backups` and lose both it and
/// the archive to a crash right after the delete.
///
/// Every step is anchored to a descriptor rather than a path for the same
/// reason: `.anolisa/backups` is inside a mount a co-tenant may be able to
/// write to, and `File::create` follows a symlink planted there. That writes
/// the whole archive outside the mount while the import below deletes every
/// note and hands the caller an in-mount recovery path that does not exist.
fn write_pre_overwrite_backup(svc: &MemoryService, archive_json: &str) -> Result<String> {
    let root = svc.mount.root_fd.as_fd();
    let dir_rel = Path::new(svc.mount.meta_dir_name()).join(BACKUPS_DIR);

    safe_fs::create_dir_durable(root, &dir_rel)?;
    let dir = safe_fs::open_dir(root, &dir_rel)?;

    // The timestamp prefix keeps names unique across calls and sorts
    // lexically in chronological order, which is what `prune_backups` relies on.
    let name = format!("{}{BACKUP_SUFFIX}", Utc::now().format("%Y%m%dT%H%M%S%.6fZ"));
    let tmp_name = format!("{name}.partial");

    {
        let mut f = safe_fs::create_new_in_dir(dir.as_fd(), Path::new(&tmp_name))?;
        f.write_all(archive_json.as_bytes())?;
        f.sync_all()?;
    }
    safe_fs::rename_in_dir(dir.as_fd(), Path::new(&tmp_name), Path::new(&name))?;
    dir.sync_all()?;

    prune_backups(&dir);

    Ok(format!(
        "{}/{BACKUPS_DIR}/{name}",
        svc.mount.meta_dir_name()
    ))
}

/// Drop the oldest backups once more than `MAX_BACKUPS` are present.
///
/// Best-effort by design: by the time this runs the backup that matters is
/// already durable, and failing an import over a stale file that could not be
/// unlinked would be the wrong trade. Enumeration and deletion both go
/// through the descriptor the write above used, so retention neither counts
/// nor removes anything it does not own — a planted symlink wearing a backup's
/// name is invisible to it, where a path-based `read_dir` would have unlinked
/// it and mis-counted the window.
fn prune_backups(dir: &File) {
    let mut names: Vec<String> = match safe_fs::regular_file_names(dir) {
        Ok(names) => names
            .into_iter()
            .filter(|n| n.ends_with(BACKUP_SUFFIX))
            .collect(),
        Err(e) => {
            tracing::warn!("could not list import backups to prune: {e}");
            return;
        }
    };
    if names.len() <= MAX_BACKUPS {
        return;
    }
    names.sort();
    for stale in &names[..names.len() - MAX_BACKUPS] {
        if let Err(e) = safe_fs::unlink_in_dir(dir.as_fd(), stale) {
            tracing::warn!("could not prune stale import backup {stale}: {e}");
        }
    }
}

fn summarize_errors(errors: &[String]) -> String {
    let mut msg = errors[..errors.len().min(MAX_AUDITED_ERRORS)].join("; ");
    if errors.len() > MAX_AUDITED_ERRORS {
        msg.push_str(&format!("; +{} more", errors.len() - MAX_AUDITED_ERRORS));
    }
    msg
}

/// Reconstruct markdown from frontmatter map + body.
fn reconstruct_markdown(fm: &HashMap<String, String>, body: &str) -> String {
    if fm.is_empty() {
        return body.to_string();
    }
    let mut out = String::from("---\n");
    let mut keys: Vec<&String> = fm.keys().collect();
    keys.sort();
    for key in keys {
        let value = &fm[key];
        // If value is a JSON array, emit as YAML list items
        if value.starts_with('[') {
            if let Ok(items) = serde_json::from_str::<Vec<String>>(value) {
                out.push_str(&format!("{key}:\n"));
                for item in &items {
                    out.push_str(&format!("  - {item}\n"));
                }
                continue;
            }
        }
        if value.is_empty() {
            out.push_str(&format!("{key}:\n"));
        } else {
            out.push_str(&format!("{key}: {value}\n"));
        }
    }
    out.push_str("---\n\n");
    out.push_str(body);
    if !body.ends_with('\n') && !body.is_empty() {
        out.push('\n');
    }
    out
}

/// Import memories from an AMA JSON string.
pub fn memory_import(
    svc: &MemoryService,
    json_data: &str,
    strategy: ImportStrategy,
    dry_run: bool,
) -> Result<String> {
    // Parse the AMA archive
    let archive: AmaArchive = serde_json::from_str(json_data)
        .map_err(|e| MemoryError::InvalidArgument(format!("invalid AMA JSON: {e}")))?;

    // Validate format
    if archive.format != "anolisa-memory-archive" {
        return Err(MemoryError::InvalidArgument(format!(
            "unknown format '{}'; expected 'anolisa-memory-archive'",
            archive.format
        )));
    }

    // Reject unknown AMA versions so future format changes fail loudly
    // instead of being silently mis-parsed.
    if archive.version != "1.0" {
        return Err(MemoryError::InvalidArgument(format!(
            "unsupported AMA version '{}'; only '1.0' is supported",
            archive.version
        )));
    }

    let mut report = ImportReport {
        imported: 0,
        skipped: 0,
        overwritten: 0,
        errors: Vec::new(),
    };

    // Handle overwrite strategy: persist a backup, then remove all existing
    // .md files.
    let mut backup_rel: Option<String> = None;
    if strategy == ImportStrategy::Overwrite && !dry_run {
        // Fail closed. `remove_all_memories` below is irreversible and nothing
        // else holds the prior store: git auto-commit only records the tool
        // calls it is told about, and snapshots are opt-in. So a backup that
        // cannot be persisted must abort the import — degrading to "proceed
        // without one" is exactly how the store gets wiped with no way back.
        // The previous code exported the store, logged its byte count as
        // "saved", and dropped the buffer.
        let current = crate::tools::memory_export::memory_export(
            svc,
            &crate::tools::memory_export::ExportFilter::default(),
        )
        .map_err(|e| {
            MemoryError::Other(format!("refusing overwrite: pre-import export failed: {e}"))
        })?;
        let rel = write_pre_overwrite_backup(svc, &current).map_err(|e| {
            MemoryError::Other(format!("refusing overwrite: pre-import backup failed: {e}"))
        })?;
        tracing::info!(
            "overwrite backup: {} bytes of current memories saved to {rel}",
            current.len()
        );
        backup_rel = Some(rel);

        let removed = remove_all_memories(svc)?;
        tracing::info!("overwrite strategy: removed {removed} existing memories");
    }

    // Import memories
    for mem in &archive.memories {
        match import_single(svc, mem, strategy, dry_run) {
            Ok(action) => match action {
                ImportAction::Imported => report.imported += 1,
                ImportAction::Overwritten => report.overwritten += 1,
                ImportAction::Skipped => report.skipped += 1,
            },
            Err(e) => {
                report.errors.push(format!("{}: {e}", mem.path));
            }
        }
    }

    // Import tasks
    for task in &archive.tasks {
        match import_single(svc, task, strategy, dry_run) {
            Ok(action) => match action {
                ImportAction::Imported => report.imported += 1,
                ImportAction::Overwritten => report.overwritten += 1,
                ImportAction::Skipped => report.skipped += 1,
            },
            Err(e) => {
                report.errors.push(format!("{}: {e}", task.path));
            }
        }
    }

    let prefix = if dry_run { "[DRY RUN] " } else { "" };
    let mut summary = format!(
        "{prefix}import complete: {} imported, {} overwritten, {} skipped, {} errors",
        report.imported,
        report.overwritten,
        report.skipped,
        report.errors.len()
    );
    if let Some(rel) = &backup_rel {
        summary.push_str(&format!("\npre-overwrite backup: {rel}"));
    }

    if !report.errors.is_empty() {
        tracing::warn!("import errors: {:?}", report.errors);
    }

    // Report every counter. An overwrite import lands as `overwritten`, so the
    // old two-field line audited a whole-store rewrite as "0 imported,
    // 0 skipped" — the durable record showed a no-op. The backup path rides
    // along because it is the one thing an operator needs in order to undo.
    //
    // `ok` flips to false only when the call applied nothing at all. A partial
    // success still mutated the store, and `git_repo::auto_commit_for` skips
    // entries with `ok == false`, so marking one as failed would leave its
    // writes uncommitted — the hole auto-commit exists to close.
    let mut audit_detail = format!(
        "{} imported, {} overwritten, {} skipped, {} errors",
        report.imported,
        report.overwritten,
        report.skipped,
        report.errors.len()
    );
    if let Some(rel) = &backup_rel {
        audit_detail.push_str(&format!(", backup {rel}"));
    }
    let mut entry = AuditEntry::new("mem_import")
        .path(audit_detail)
        .bytes(json_data.len() as u64);
    let applied = report.imported + report.overwritten + report.skipped;
    if applied == 0 && !report.errors.is_empty() {
        entry = entry.error(summarize_errors(&report.errors));
    }
    svc.audit_log(entry);

    // Append error details if any
    if report.errors.is_empty() {
        Ok(summary)
    } else {
        let mut out = summary;
        out.push_str("\n\nErrors:\n");
        for e in &report.errors {
            out.push_str(&format!("  - {e}\n"));
        }
        Ok(out)
    }
}

#[derive(Debug)]
enum ImportAction {
    Imported,
    Overwritten,
    Skipped,
}

fn import_single(
    svc: &MemoryService,
    mem: &ExportedMemory,
    strategy: ImportStrategy,
    dry_run: bool,
) -> Result<ImportAction> {
    // Security: use ns::paths::resolve_for_create to prevent path traversal
    // and enforce reserved-segment checks (.anolisa, .git*, etc.)
    let target_path = crate::ns::paths::resolve_for_create(&svc.mount, &mem.path)?;

    let exists = target_path.exists();

    // Skip-existing: don't touch existing files
    if exists && strategy == ImportStrategy::SkipExisting {
        return Ok(ImportAction::Skipped);
    }

    if dry_run {
        return Ok(if exists {
            ImportAction::Overwritten
        } else {
            ImportAction::Imported
        });
    }

    // Reconstruct the markdown file
    let content = reconstruct_markdown(&mem.frontmatter, &mem.content);

    // Use safe_fs::write for secure write with openat2 + RESOLVE_BENEATH
    let rel_path = crate::ns::paths::relative_to_mount(&svc.mount, &target_path);
    let rel_path_path = Path::new(&rel_path);

    // Ensure parent directory exists (using safe_fs-aware path).
    // Mirrors write.rs: validate each existing parent component against
    // symlink swaps before the unsandboxed create_dir_all call.
    // resolve_for_create already enforced no `..`/absolute at check time;
    // assert_no_symlink_traversal closes the TOCTOU gap.
    if let Some(parent) = target_path.parent() {
        crate::safe_fs::assert_no_symlink_traversal(svc.mount.root_fd.as_fd(), rel_path_path)?;
        std::fs::create_dir_all(parent)?;
    }

    crate::safe_fs::write(svc.mount.root_fd.as_fd(), rel_path_path, content.as_bytes())?;

    Ok(if exists {
        ImportAction::Overwritten
    } else {
        ImportAction::Imported
    })
}

/// Remove all .md files under the mount root (for overwrite strategy).
/// Respects reserved paths (.anolisa, .git*, etc.)
fn remove_all_memories(svc: &MemoryService) -> Result<usize> {
    let meta_dir = svc.mount.meta_dir.clone();
    let mut removed = 0;

    for entry in walkdir::WalkDir::new(&svc.mount.root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip meta directory and reserved paths
            let path = e.path();
            if path.starts_with(&meta_dir) {
                return false;
            }
            // Check for reserved segments (.git*, etc.)
            if let Some(first_segment) = path
                .strip_prefix(&svc.mount.root)
                .ok()
                .and_then(|p| p.components().next())
            {
                let segment_str = first_segment.as_os_str().to_string_lossy();
                if segment_str.starts_with(".git") {
                    return false;
                }
            }
            true
        })
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        // Validate path is within sandbox and no symlink traversal
        let rel_path = crate::ns::paths::relative_to_mount(&svc.mount, path);
        if crate::safe_fs::assert_no_symlink_traversal(
            svc.mount.root_fd.as_fd(),
            Path::new(&rel_path),
        )
        .is_err()
        {
            tracing::warn!("skipping unsafe path during overwrite: {}", path.display());
            continue;
        }
        if std::fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstruct_markdown_roundtrip() {
        let mut fm = HashMap::new();
        fm.insert("id".into(), "abc123".into());
        fm.insert("category".into(), "lesson".into());
        let body = "This is the body content.";

        let md = reconstruct_markdown(&fm, body);
        assert!(md.starts_with("---\n"));
        assert!(md.contains("id: abc123"));
        assert!(md.contains("category: lesson"));
        assert!(md.contains("This is the body content."));
    }

    #[test]
    fn reconstruct_empty_frontmatter() {
        let fm = HashMap::new();
        let body = "Just content.";
        let md = reconstruct_markdown(&fm, body);
        assert_eq!(md, body);
    }

    #[test]
    fn import_strategy_parse() {
        assert_eq!(
            ImportStrategy::parse("merge").unwrap(),
            ImportStrategy::Merge
        );
        assert_eq!(
            ImportStrategy::parse("overwrite").unwrap(),
            ImportStrategy::Overwrite
        );
        assert_eq!(
            ImportStrategy::parse("skip-existing").unwrap(),
            ImportStrategy::SkipExisting
        );
        assert_eq!(
            ImportStrategy::parse("skip").unwrap(),
            ImportStrategy::SkipExisting
        );
        assert!(ImportStrategy::parse("invalid").is_err());
    }

    #[test]
    fn parse_ama_json() {
        let json = r#"{
            "version": "1.0",
            "format": "anolisa-memory-archive",
            "exported_at": "2026-06-11T14:00:00Z",
            "agent_id": "test",
            "user_id": "alice",
            "total_memories": 1,
            "memories": [{
                "path": "facts/lesson/test.md",
                "frontmatter": {"category": "lesson", "id": "abc"},
                "content": "Lesson body"
            }],
            "tasks": [],
            "stats": {"by_category": {"lesson": 1}, "by_source": {}, "total_bytes": 50}
        }"#;
        let archive: AmaArchive = serde_json::from_str(json).unwrap();
        assert_eq!(archive.total_memories, 1);
        assert_eq!(archive.memories[0].path, "facts/lesson/test.md");
    }
}
