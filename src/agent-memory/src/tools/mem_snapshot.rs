use crate::audit::AuditEntry;
use crate::error::Result;
use crate::service::MemoryService;
use crate::snapshot::SnapshotInfo;

const TOOL: &str = "mem_snapshot";

/// Create a point-in-time snapshot of the namespace mount root. Excludes
/// `.anolisa/` (audit, index, prior snapshots) so the archive stays small
/// and idempotent. `name` is an optional human label; the OS still picks
/// a stable id (`snap_<ULID>`).
pub fn snapshot(svc: &MemoryService, name: Option<&str>) -> Result<SnapshotInfo> {
    match crate::snapshot::create(&svc.mount, name) {
        Ok(info) => {
            svc.audit_log(AuditEntry::new(TOOL).path(info.id.clone()).bytes(info.size));
            Ok(info)
        }
        Err(e) => {
            svc.audit_log(AuditEntry::new(TOOL).error(e.to_string()));
            Err(e)
        }
    }
}
