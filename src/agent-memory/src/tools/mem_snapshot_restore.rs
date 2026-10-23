use crate::audit::AuditEntry;
use crate::error::Result;
use crate::service::MemoryService;

const TOOL: &str = "mem_snapshot_restore";

/// Restore a previously-created snapshot, replacing every user-visible file
/// at the mount root with the archive contents. The `.anolisa/` meta dir
/// (audit, index, snapshots themselves) is preserved across the restore.
pub fn snapshot_restore(svc: &MemoryService, id: &str) -> Result<()> {
    snapshot_restore_with_outcome(svc, id).map(|_| ())
}

pub(crate) fn snapshot_restore_with_outcome(
    svc: &MemoryService,
    id: &str,
) -> Result<crate::snapshot::RestoreOutcome> {
    match crate::snapshot::restore_with_outcome(&svc.mount, id) {
        Ok(outcome) => {
            svc.audit_log(AuditEntry::new(TOOL).path(id.to_string()));
            if let Some(dir) = &outcome.retained_dir {
                let relative = dir.strip_prefix(&svc.mount.root).unwrap_or(dir);
                svc.audit_log(
                    AuditEntry::new("mem_snapshot_restore_recovery")
                        .path(relative.to_string_lossy().into_owned()),
                );
            }
            Ok(outcome)
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
