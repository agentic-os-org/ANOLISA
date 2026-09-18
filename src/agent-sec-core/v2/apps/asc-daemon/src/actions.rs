//! The process-owned capability inventory. Handlers never assemble runtimes or sinks.
use asc_action_runtime::{ActionRuntime, Finalizer};
use asc_action_types::ActionId;
use asc_capability_code_scan::{CodeScanAuditProjector, CodeScanExecutor};
use asc_daemon_core::ActionService;
use std::sync::Arc;

/// Composes every implemented scan with the same required finalization infrastructure.
#[must_use]
pub fn scan_application(finalizer: Finalizer) -> Arc<ActionService> {
    Arc::new(ActionService::new(ActionRuntime::new(
        ActionId::CodeScan,
        CodeScanExecutor,
        CodeScanAuditProjector,
        finalizer,
    )))
}
