use std::os::fd::AsFd;
use std::path::Path;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::paths::{relative_to_mount, resolve_for_create};
use crate::safe_fs;
use crate::service::MemoryService;

const TOOL: &str = "mem_append";

/// Append `content` to a file. Creates the file (and parents) if missing.
pub fn append(svc: &MemoryService, path: &str, content: &str) -> Result<u64> {
    // Per-call payload cap (analogous to max_write_bytes). Total file
    // size is not bounded by this — append is meant for log-style writes
    // where the file may grow large over many calls; the cap just
    // prevents a single call from doing it in one shot.
    let cap = svc.config.memory.max_append_bytes;
    if content.len() as u64 > cap {
        let err = MemoryError::InvalidArgument(format!(
            "mem_append content {} bytes exceeds limit {} bytes",
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

    if let Ok(meta) = safe_fs::metadata(svc.mount.root_fd.as_fd(), rel_path) {
        if meta.is_dir() {
            let err = MemoryError::InvalidArgument(format!("'{rel}' is a directory"));
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
            return Err(err);
        }
    }

    // Symlink check FIRST — if the parent contains a symlink, fail before
    // create_dir_all has a chance to walk through it. Matches the ordering
    // in write.rs; the previous reverse order left a TOCTOU window where
    // create_dir_all could materialise directories along an attacker-planted
    // symlink chain.
    if let Some(parent_str) = Path::new(&rel)
        .parent()
        .and_then(|p| (!p.as_os_str().is_empty()).then(|| p.to_string_lossy().into_owned()))
    {
        if let Err(e) =
            safe_fs::assert_no_symlink_traversal(svc.mount.root_fd.as_fd(), Path::new(&parent_str))
        {
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
            return Err(e);
        }
    }
    if let Some(parent) = resolved.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let bytes = match safe_fs::append(svc.mount.root_fd.as_fd(), rel_path, content.as_bytes()) {
        Ok(n) => n,
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
            return Err(e);
        }
    };
    svc.audit_log(AuditEntry::new(TOOL).path(rel).bytes(bytes));

    Ok(bytes)
}
