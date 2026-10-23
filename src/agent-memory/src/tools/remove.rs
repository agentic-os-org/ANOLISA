use std::os::fd::AsFd;
use std::path::Path;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::paths::{relative_to_mount, resolve_path};
use crate::safe_fs;
use crate::service::MemoryService;

const TOOL: &str = "mem_remove";

/// Delete a file or directory.
/// - For non-empty directories, `recursive` MUST be true; otherwise
///   `InvalidArgument`. An empty directory may be removed without it
///   (there is nothing to recurse over).
/// - The mount root and `.anolisa` meta dir cannot be targets (path resolver
///   already rejects the meta dir).
pub fn remove(svc: &MemoryService, path: &str, recursive: bool) -> Result<()> {
    let resolved = match resolve_path(&svc.mount, path) {
        Ok(p) => p,
        Err(e) => {
            svc.audit_log(
                AuditEntry::new(TOOL)
                    .path(path.to_string())
                    .error(e.to_string()),
            );
            return Err(e);
        }
    };

    let rel = relative_to_mount(&svc.mount, &resolved);
    let rel_path = Path::new(&rel);

    // Block symlink-based escapes: an attacker who replaces
    // `notes/foo` with a symlink to `~/.ssh` would otherwise have
    // remove_dir_all happily destroy the linked target.
    if let Err(e) = safe_fs::assert_no_symlink_traversal(svc.mount.root_fd.as_fd(), rel_path) {
        svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
        return Err(e);
    }

    let meta = match safe_fs::metadata(svc.mount.root_fd.as_fd(), rel_path) {
        Ok(m) => m,
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
            return Err(e);
        }
    };

    if meta.is_dir() {
        if !recursive {
            // The tool contract says recursive=true is required only for
            // NON-EMPTY directories, so probe emptiness before rejecting:
            // deleting an empty dir has nothing to recurse over.
            //
            // This listing walks the absolute path rather than the openat2
            // fd the recursive path anchors to — the hardening difference is
            // immaterial here because the deletion that follows is
            // `remove_dir` (rmdir(2)), which does not follow a trailing
            // symlink and refuses to remove a non-empty directory, so a
            // directory swapped between probe and removal can only make the
            // call fail safely, never destroy anything it did not probe.
            // Any other IO error while listing counts as non-empty — fail
            // closed on the existing InvalidArgument path.
            let is_empty = match std::fs::read_dir(&resolved) {
                Ok(mut entries) => entries.next().is_none(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // The directory vanished between the metadata probe and
                    // the listing; report the same NotFound the precheck
                    // would have produced.
                    let err = MemoryError::NotFound(rel.clone());
                    svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
                    return Err(err);
                }
                Err(_) => false,
            };
            if !is_empty {
                let err = MemoryError::InvalidArgument(format!(
                    "'{rel}' is a directory; pass recursive=true to remove"
                ));
                svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
                return Err(err);
            }
            std::fs::remove_dir(&resolved)?;
        } else {
            // Use sandboxed recursive delete that refuses to follow symlinks
            // inside the directory — std::fs::remove_dir_all would follow them
            // and could destroy targets outside the mount.
            safe_fs::remove_dir_all_safe(svc.mount.root_fd.as_fd(), rel_path, &resolved)?;
        }
    } else {
        std::fs::remove_file(&resolved)?;
    }

    svc.audit_log(AuditEntry::new(TOOL).path(rel));
    Ok(())
}
