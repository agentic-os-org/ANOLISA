use std::os::fd::AsFd;
use std::path::Path;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::paths::{relative_to_mount, resolve_path};
use crate::safe_fs;
use crate::service::MemoryService;

const TOOL: &str = "mem_read";

/// Read a file's contents as UTF-8 text.
pub fn read(svc: &MemoryService, path: &str) -> Result<String> {
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

    // metadata() refuses to follow symlinks; if the path is a dir we
    // surface InvalidArgument rather than a noisy I/O error from read.
    let meta = match safe_fs::metadata(svc.mount.root_fd.as_fd(), rel_path) {
        Ok(m) => m,
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
            return Err(e);
        }
    };
    if !meta.is_file() {
        let err = MemoryError::InvalidArgument(format!("'{rel}' is not a file"));
        svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
        return Err(err);
    }

    // Reject files exceeding the configured read cap to prevent multi-GB
    // blobs from exhausting memory in the JSON-RPC response.
    let cap = svc.config.memory.max_read_bytes;
    if meta.len() > cap {
        let err = MemoryError::InvalidArgument(format!(
            "file '{rel}' exceeds read limit: {} > {} bytes",
            meta.len(),
            cap
        ));
        svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
        return Err(err);
    }

    let body = match safe_fs::read_to_string(svc.mount.root_fd.as_fd(), rel_path) {
        Ok(b) => b,
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).path(rel).error(e.to_string()));
            return Err(e);
        }
    };
    let bytes = body.len() as u64;

    svc.audit_log(AuditEntry::new(TOOL).path(rel).bytes(bytes));

    Ok(body)
}
