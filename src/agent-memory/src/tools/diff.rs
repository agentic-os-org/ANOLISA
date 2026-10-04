use std::os::fd::AsFd;
use std::path::Path;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::paths::{relative_to_mount, resolve_path};
use crate::safe_fs;
use crate::service::MemoryService;

const TOOL: &str = "mem_diff";

/// Return a unified-diff string between two files. Both must exist and be
/// UTF-8 text.
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

    // Reject inputs exceeding the configured read cap before either file
    // is read — same reasoning as mem_read: both bodies (and the patch)
    // live in memory at once, so an uncapped multi-GB blob would exhaust
    // it. diffy never sees oversized input because we never read it.
    let cap = svc.config.memory.max_read_bytes;
    for rel in [&rel1, &rel2] {
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
        if meta.len() > cap {
            let err = MemoryError::InvalidArgument(format!(
                "file '{rel}' exceeds read limit: {} > {} bytes",
                meta.len(),
                cap
            ));
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
            return Err(err);
        }
    }

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
