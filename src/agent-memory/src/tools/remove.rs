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
            // NON-EMPTY directories, so an empty directory may be removed
            // without it — there is nothing to recurse over.
            //
            // The deletion is fd-anchored rather than re-resolving the
            // absolute path: `safe_fs::remove_empty_dir` opens the parent
            // directory against the mount fd with openat2 RESOLVE_BENEATH |
            // RESOLVE_NO_SYMLINKS, then calls unlinkat(RemoveDir) by parent
            // fd + final-component name. An ancestor component swapped for a
            // symlink between `assert_no_symlink_traversal` (above) and this
            // syscall therefore cannot redirect the rmdir outside the mount —
            // the openat2 resolve step rejects it. unlinkat returns ENOTEMPTY
            // for a non-empty directory, mapped precisely to the
            // "pass recursive=true" contract instead of being hidden behind a
            // probe, and every other error is preserved.
            match safe_fs::remove_empty_dir(svc.mount.root_fd.as_fd(), rel_path) {
                Ok(()) => {}
                Err(e) => {
                    svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
                    return Err(e);
                }
            }
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
