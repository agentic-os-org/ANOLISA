use std::os::fd::AsFd;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::paths::{relative_to_mount, resolve_for_create};
use crate::service::MemoryService;

const TOOL: &str = "mem_revert";

/// Restore `path` to the content currently at HEAD, then commit the
/// revert. Useful for undoing the most recent uncommitted edit.
pub fn mem_revert(svc: &MemoryService, path: &str) -> Result<String> {
    let git = match svc.git.as_ref() {
        Some(g) => g,
        None => {
            let err = MemoryError::NotImplemented(
                "git versioning is disabled; set [memory.git].enabled = true",
            );
            svc.audit_log(AuditEntry::new(TOOL).error(err.to_string()));
            return Err(err);
        }
    };

    // Sandbox the path against the mount, then pass the validated
    // mount-relative form to git. Previously the raw user path was
    // passed through, so the resolver check was decorative — a value
    // like `../../etc/passwd` would have been forwarded to
    // `git2::Tree::get_path` (rejected) but also to `root.join(path)`
    // for the write-back, where `Path::join` happily produces an
    // outside-the-root path.
    let resolved = match resolve_for_create(&svc.mount, path) {
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

    match git.revert(svc.mount.root_fd.as_fd(), &rel) {
        Ok(hash) => {
            svc.audit_log(AuditEntry::new(TOOL).path(rel).bytes(hash.len() as u64));
            Ok(hash)
        }
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
            Err(e)
        }
    }
}
