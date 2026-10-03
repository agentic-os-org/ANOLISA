use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::git_repo::LogEntry;
use crate::service::MemoryService;

const TOOL: &str = "mem_log";

/// Return recent git commits for this mount, optionally filtered by path.
/// Errors with NotImplemented when git isn't enabled in config.
pub fn mem_log(svc: &MemoryService, limit: usize, path: Option<&str>) -> Result<Vec<LogEntry>> {
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

    match crate::git_repo::log(&git.root, limit.max(1), path) {
        Ok(entries) => {
            svc.audit_log(AuditEntry::new(TOOL).bytes(entries.len() as u64));
            Ok(entries)
        }
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).error(e.to_string()));
            Err(e)
        }
    }
}
