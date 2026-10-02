use std::os::fd::AsFd;
use std::path::Path;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::paths::{relative_to_mount, resolve_path};
use crate::safe_fs;
use crate::service::MemoryService;

const TOOL: &str = "mem_mkdir";

/// Create a directory (creating parents as needed). Idempotent.
pub fn mkdir(svc: &MemoryService, path: &str) -> Result<()> {
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

    // Guard against symlink traversal — std::fs::create_dir_all happily
    // follows symlinks, which would let mkdir create directories
    // outside the mount tree.
    if let Err(e) = safe_fs::assert_no_symlink_traversal(svc.mount.root_fd.as_fd(), rel_path) {
        svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
        return Err(e);
    }

    if let Ok(meta) = safe_fs::metadata(svc.mount.root_fd.as_fd(), rel_path) {
        if meta.is_file() {
            let err = MemoryError::InvalidArgument(format!("'{rel}' is a regular file"));
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
            return Err(err);
        }
    }

    std::fs::create_dir_all(&resolved)?;
    svc.audit_log(AuditEntry::new(TOOL).path(rel));
    Ok(())
}
