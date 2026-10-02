use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::service::MemoryService;

const TOOL: &str = "mem_session_log";

/// Return this session's running JSONL tool-call log so the model can inspect
/// what it has done so far. Errors gracefully when no session is active.
pub fn session_log(svc: &MemoryService) -> Result<String> {
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

    match session.read_log() {
        Ok(s) => {
            svc.audit_log(AuditEntry::new(TOOL).bytes(s.len() as u64));
            Ok(s)
        }
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).error(e.to_string()));
            Err(e)
        }
    }
}
