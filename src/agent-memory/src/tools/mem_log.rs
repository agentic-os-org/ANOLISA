use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::git_repo::LogEntry;
use crate::service::MemoryService;

const TOOL: &str = "mem_log";

/// Ceiling for the caller-supplied `limit`. The floor (`1`) already
/// existed; the missing ceiling let a u32::MAX limit drive the revwalk
/// and entry Vec over the entire history — bounded only by the number of
/// commits on disk. No other tool caps exist to match (mem_grep defaults
/// to 200 hits, session tools default to 10/50), so 1000 is the chosen
/// sane maximum: several orders above the 20-commit default, far below
/// anything that would exhaust memory.
const MAX_LOG_LIMIT: usize = 1000;

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

    let limit = limit.clamp(1, MAX_LOG_LIMIT);
    match crate::git_repo::log(&git.root, limit, path) {
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
