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
    let mut options = diffy::DiffOptions::new();
    options
        .set_original_filename(rel1.clone())
        .set_modified_filename(rel2.clone());
    let patch = options.create_patch(&body1, &body2);
    let mut formatted = patch.to_string();
    if rel1.contains(&['\n', '\t', '\r'][..]) || rel2.contains(&['\n', '\t', '\r'][..]) {
        // diffy 0.4 escapes these filename characters as backslash + the
        // literal control, while its parser expects the printable escape.
        // An empty patch gives the exact header prefix, including any embedded
        // newlines, so only filenames are repaired and hunks stay untouched.
        let headers = options.create_patch("", "").to_string();
        let repaired = headers
            .replace("\\\n", "\\n")
            .replace("\\\t", "\\t")
            .replace("\\\r", "\\r");
        let hunks = match formatted.strip_prefix(&headers) {
            Some(hunks) => hunks,
            None => {
                let err = MemoryError::Other("diff formatter returned inconsistent headers".into());
                svc.audit_log(AuditEntry::new(TOOL).error(err.to_string()));
                return Err(err);
            }
        };
        formatted = format!("{repaired}{hunks}");
    }

    svc.audit_log(
        AuditEntry::new(TOOL)
            .path(format!("{rel1} <-> {rel2}"))
            .bytes(formatted.len() as u64),
    );

    Ok(formatted)
}
