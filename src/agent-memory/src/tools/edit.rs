use std::os::fd::AsFd;
use std::path::Path;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::paths::{relative_to_mount, resolve_path};
use crate::safe_fs;
use crate::service::MemoryService;

const TOOL: &str = "mem_edit";

/// String-replacement edit (Anthropic str_replace style).
///
/// - `old_str` MUST occur exactly once in the file. Zero occurrences →
///   `NotFound`-style error; multiple occurrences → ambiguity error.
/// - `new_str` may be empty (deletion).
pub fn edit(svc: &MemoryService, path: &str, old_str: &str, new_str: &str) -> Result<()> {
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

    if old_str.is_empty() {
        let err = MemoryError::InvalidArgument("old_str must not be empty".into());
        svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
        return Err(err);
    }

    // Size-check before slurping the file into RAM. mem_write/mem_append
    // are capped, but a file on disk can still exceed max_read_bytes if
    // populated by an external tool or by raising the write cap mid-flight.
    // Without this, an attacker who can grow the file (or operator who
    // dropped a multi-GB log into the mount) makes mem_edit OOM the
    // process. Use the same cap as mem_read for symmetry.
    let cap = svc.config.memory.max_read_bytes;
    if let Ok(meta) = safe_fs::metadata(svc.mount.root_fd.as_fd(), rel_path) {
        if meta.len() > cap {
            let err = MemoryError::InvalidArgument(format!(
                "file '{rel}' exceeds edit limit: {} > {} bytes",
                meta.len(),
                cap
            ));
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
            return Err(err);
        }
    }

    let body = match safe_fs::read_to_string(svc.mount.root_fd.as_fd(), rel_path) {
        Ok(b) => b,
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
            return Err(e);
        }
    };
    // Short-circuit at the second match — we only care whether the count
    // is 0, 1, or "2+". Walking the full body to count every match wastes
    // CPU for large files when the answer was decided after byte ~N.
    let count = body.match_indices(old_str).take(2).count();
    if count == 0 {
        let err = MemoryError::InvalidArgument(format!("old_str not found in '{rel}'"));
        svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
        return Err(err);
    }
    if count > 1 {
        let err = MemoryError::InvalidArgument(format!(
            "old_str matches multiple occurrences in '{rel}' — provide more context to disambiguate"
        ));
        svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
        return Err(err);
    }

    let updated = body.replacen(old_str, new_str, 1);
    let bytes = match safe_fs::write(svc.mount.root_fd.as_fd(), rel_path, updated.as_bytes()) {
        Ok(n) => n,
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
            return Err(e);
        }
    };

    svc.audit_log(AuditEntry::new(TOOL).path(rel).bytes(bytes));

    Ok(())
}
