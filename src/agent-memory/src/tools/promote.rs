use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::service::MemoryService;

const TOOL: &str = "mem_promote";

/// Copy a file from the active session's `scratch/` to the persistent Memory
/// Store. The source path is sandboxed against the session scratch root; the
/// destination is sandboxed against the mount root and must not already exist.
pub fn promote(svc: &MemoryService, session_path: &str, store_path: &str) -> Result<u64> {
    let session = match svc.session.as_ref() {
        Some(s) => s,
        None => {
            let err = MemoryError::NotImplemented(
                "session log unavailable; check MEMORY_SESSION_DIR / /run/anolisa permissions",
            );
            svc.audit_log(AuditEntry::new(TOOL).error(err.to_string()));
            return Err(err);
        }
    };

    match session.promote(
        session_path,
        store_path,
        &svc.mount,
        svc.config.memory.max_read_bytes,
    ) {
        Ok(bytes) => {
            svc.audit_log(
                AuditEntry::new(TOOL)
                    .path(format!("{session_path} -> {store_path}"))
                    .bytes(bytes),
            );
            Ok(bytes)
        }
        Err(e) => {
            svc.audit_log(
                AuditEntry::new(TOOL)
                    .path(format!("{session_path} -> {store_path}"))
                    .error(e.to_string()),
            );
            Err(e)
        }
    }
}
