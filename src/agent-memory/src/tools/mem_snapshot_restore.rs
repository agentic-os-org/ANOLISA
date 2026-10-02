use crate::audit::AuditEntry;
use crate::error::Result;
use crate::service::MemoryService;

const TOOL: &str = "mem_snapshot_restore";

/// Restore a previously-created snapshot, replacing every user-visible file
/// at the mount root with the archive contents. The `.anolisa/` meta dir
/// (audit, index, snapshots themselves) is preserved across the restore.
pub fn snapshot_restore(svc: &MemoryService, id: &str) -> Result<()> {
    match crate::snapshot::restore(&svc.mount, id) {
        Ok(()) => {
            svc.audit_log(AuditEntry::new(TOOL).path(id.to_string()));
            Ok(())
        }
        Err(e) => {
            svc.audit_log(
                AuditEntry::new(TOOL)
                    .path(id.to_string())
                    .error(e.to_string()),
            );
            Err(e)
        }
    }
}
