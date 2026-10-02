use std::os::fd::AsFd;
use std::path::Path;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::paths::{relative_to_mount, resolve_path};
use crate::safe_fs;
use crate::service::MemoryService;

const TOOL: &str = "mem_remove";

/// Delete a file or directory.
/// - For directories, `recursive` MUST be true; otherwise `InvalidArgument`.
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
            let err = MemoryError::InvalidArgument(format!(
                "'{rel}' is a directory; pass recursive=true to remove"
            ));
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
            return Err(err);
        }
        // Use sandboxed recursive delete that refuses to follow symlinks
        // inside the directory — std::fs::remove_dir_all would follow them
        // and could destroy targets outside the mount.
        safe_fs::remove_dir_all_safe(svc.mount.root_fd.as_fd(), rel_path, &resolved)?;
    } else {
        std::fs::remove_file(&resolved)?;
    }

    svc.audit_log(AuditEntry::new(TOOL).path(rel));
    Ok(())
}
