use std::os::fd::AsFd;
use std::path::Path;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::paths::{relative_to_mount, resolve_for_create};
use crate::safe_fs;
use crate::service::MemoryService;

const TOOL: &str = "mem_write";

/// Write a file. Creates parent directories. If the file exists and
/// `overwrite == false`, returns `AlreadyExists`.
pub fn write(svc: &MemoryService, path: &str, content: &str, overwrite: bool) -> Result<u64> {
    // Cap the per-call payload. Without this, a misbehaving agent can
    // ship a multi-GB string and fill the disk (cgroup caps RSS, not
    // file size). Audit the rejection so operators see runaway models.
    let cap = svc.config.memory.max_write_bytes;
    if content.len() as u64 > cap {
        let err = MemoryError::InvalidArgument(format!(
            "mem_write content {} bytes exceeds limit {} bytes",
            content.len(),
            cap
        ));
        svc.audit_log(
            AuditEntry::new(TOOL)
                .path(path.to_string())
                .error(err.to_string()),
        );
        return Err(err);
    }

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
    let rel_path = Path::new(&rel);

    // Surface "is a directory" early so users get a clean error instead
    // of EISDIR from open(2).
    if let Ok(meta) = safe_fs::metadata(svc.mount.root_fd.as_fd(), rel_path) {
        if meta.is_dir() {
            let err = MemoryError::InvalidArgument(format!("'{rel}' is a directory"));
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
            return Err(err);
        }
    }

    // Create parent dirs first. The parent path is already validated by
    // resolve_for_create; assert_no_symlink_traversal additionally
    // guards each existing component against symlink swaps.
    if let Some(parent_str) = parent_of(&rel) {
        let parent_path = Path::new(&parent_str);
        if let Err(e) = safe_fs::assert_no_symlink_traversal(svc.mount.root_fd.as_fd(), parent_path)
        {
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
            return Err(e);
        }
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let result = if overwrite {
        safe_fs::write(svc.mount.root_fd.as_fd(), rel_path, content.as_bytes())
    } else {
        safe_fs::write_create_new(svc.mount.root_fd.as_fd(), rel_path, content.as_bytes())
    };
    let bytes = match result {
        Ok(n) => n,
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
            return Err(e);
        }
    };

    svc.audit_log(AuditEntry::new(TOOL).path(rel).bytes(bytes));

    Ok(bytes)
}

fn parent_of(rel: &str) -> Option<String> {
    Path::new(rel).parent().and_then(|p| {
        if p.as_os_str().is_empty() {
            None
        } else {
            Some(p.to_string_lossy().into_owned())
        }
    })
}
