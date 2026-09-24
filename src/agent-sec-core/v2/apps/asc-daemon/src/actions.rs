//! The process-owned capability inventory. Handlers never assemble runtimes or sinks.
use asc_action_runtime::{ActionRuntime, Finalizer};
use asc_action_types::ActionId;
use asc_capability_code_scan::{CodeScanAuditProjector, CodeScanExecutor};
use asc_capability_prompt_scan::{
    CachingScannerProvider, PromptScanAuditProjector, PromptScanExecutor, PromptScanWarmup,
    ScannerProvider,
};
use asc_daemon_core::ActionService;
use std::sync::Arc;

/// Composes every implemented scan with the same required finalization infrastructure.
#[must_use]
pub fn scan_application(finalizer: Finalizer) -> Arc<ActionService> {
    // The finalizer shares one sink set across capabilities; cloning it forks
    // the Arc handles, not the sinks, so both runtimes finalize identically.
    //
    // The provider builds scanners lazily per mode, so daemon startup
    // never depends on model-service configuration and the rule-set
    // compilation cost is paid once per mode, not per request. The warmup
    // probe shares it, so a successful check warms exactly the scanner
    // instance the next scan of that mode and model reuses.
    let provider: Arc<dyn ScannerProvider> = Arc::new(CachingScannerProvider::default());
    Arc::new(ActionService::new(
        ActionRuntime::new(
            ActionId::CodeScan,
            CodeScanExecutor,
            CodeScanAuditProjector,
            finalizer.clone(),
        ),
        ActionRuntime::new(
            ActionId::PromptScan,
            PromptScanExecutor::new(Arc::clone(&provider)),
            PromptScanAuditProjector,
            finalizer,
        ),
        PromptScanWarmup::new(provider),
    ))
}
