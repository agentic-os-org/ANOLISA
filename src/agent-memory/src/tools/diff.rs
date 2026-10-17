use std::os::fd::AsFd;
use std::path::Path;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::paths::{relative_to_mount, resolve_path};
use crate::safe_fs;
use crate::service::MemoryService;

const TOOL: &str = "mem_diff";

/// Enforce the `max_read_bytes` cap on one `mem_diff` input before it is read.
///
/// # Errors
///
/// Returns the `safe_fs::metadata` error when the path cannot be stat'ed
/// (missing file, sandbox violation), or `InvalidArgument` when the file is
/// larger than the configured read cap. Both are audit-logged.
fn check_read_cap(svc: &MemoryService, rel: &str) -> Result<()> {
    let meta = match safe_fs::metadata(svc.mount.root_fd.as_fd(), Path::new(rel)) {
        Ok(m) => m,
        Err(e) => {
            svc.audit_log(
                AuditEntry::new(TOOL)
                    .path(rel.to_string())
                    .error(e.to_string()),
            );
            return Err(e);
        }
    };
    let cap = svc.config.memory.max_read_bytes;
    if meta.len() > cap {
        let err = MemoryError::InvalidArgument(format!(
            "file '{rel}' exceeds read limit: {} > {} bytes",
            meta.len(),
            cap
        ));
        svc.audit_log(
            AuditEntry::new(TOOL)
                .path(rel.to_string())
                .error(err.to_string()),
        );
        return Err(err);
    }
    Ok(())
}

/// Return a unified-diff string between two files. Both must exist and be
/// UTF-8 text.
///
/// # Errors
///
/// Returns `InvalidArgument` when either input exceeds
/// `config.memory.max_read_bytes`, and propagates path-resolution and read
/// errors otherwise.
pub fn diff(svc: &MemoryService, path1: &str, path2: &str) -> Result<String> {
    let r1 = match resolve_path(&svc.mount, path1) {
        Ok(p) => p,
        Err(e) => {
            svc.audit_log(
                AuditEntry::new(TOOL)
                    .path(path1.to_string())
                    .error(e.to_string()),
            );
            return Err(e);
        }
    };
    let r2 = match resolve_path(&svc.mount, path2) {
        Ok(p) => p,
        Err(e) => {
            svc.audit_log(
                AuditEntry::new(TOOL)
                    .path(path2.to_string())
                    .error(e.to_string()),
            );
            return Err(e);
        }
    };

    let rel1 = relative_to_mount(&svc.mount, &r1);
    let rel2 = relative_to_mount(&svc.mount, &r2);

    // Gate both inputs before either body is read. mem_diff slurps two whole
    // files and then builds a patch that can be larger than either, so it needs
    // the same cap mem_read and mem_edit enforce — and checking only the file
    // about to be read would let an oversized second input ride along behind a
    // small first one.
    check_read_cap(svc, &rel1)?;
    check_read_cap(svc, &rel2)?;

    let body1 = match safe_fs::read_to_string(svc.mount.root_fd.as_fd(), Path::new(&rel1)) {
        Ok(b) => b,
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).path(rel1).error(e.to_string()));
            return Err(e);
        }
    };
    let body2 = match safe_fs::read_to_string(svc.mount.root_fd.as_fd(), Path::new(&rel2)) {
        Ok(b) => b,
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).path(rel2).error(e.to_string()));
            return Err(e);
        }
    };
    let patch = diffy::create_patch(&body1, &body2);
    let formatted = format!("--- {rel1}\n+++ {rel2}\n{patch}");

    svc.audit_log(
        AuditEntry::new(TOOL)
            .path(format!("{rel1} <-> {rel2}"))
            .bytes(formatted.len() as u64),
    );

    Ok(formatted)
}
