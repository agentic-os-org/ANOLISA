use crate::audit::AuditEntry;
use crate::error::Result;
use crate::service::MemoryService;
use crate::snapshot::SnapshotInfo;

const TOOL: &str = "mem_snapshot_list";

/// Return all snapshots stored under `<mount>/.agentos/snapshots/`,
/// ordered oldest → newest.
pub fn snapshot_list(svc: &MemoryService) -> Result<Vec<SnapshotInfo>> {
    match crate::snapshot::list(&svc.mount) {
        Ok(infos) => {
            svc.audit_log(AuditEntry::new(TOOL).bytes(infos.len() as u64));
            Ok(infos)
        }
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).error(e.to_string()));
            Err(e)
        }
    }
}
