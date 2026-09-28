//! Shared test fixtures for daemon handler unit tests.

use std::sync::Arc;

use asc_action_runtime::{
    ActionRuntime, CapabilityExecutor, SecurityEventSink, testing::audit_finalizer,
};
use asc_action_types::{ActionId, CodeScanRequest, PromptScanRequest};
use asc_capability_code_scan::{CodeScanAuditProjector, CodeScanExecutor};
use asc_capability_prompt_scan::{
    CachingScannerProvider, PromptScanAuditProjector, PromptScanExecutor, PromptScanWarmup,
};
use asc_daemon_core::ActionService;
use asc_security_events::SecurityEvent;

/// Sink that discards all security events.
pub struct NoopSink;

impl SecurityEventSink for NoopSink {
    fn write(&self, _: &SecurityEvent) {}
}

/// Sink that records all security events for later inspection.
#[derive(Default)]
pub struct RecordingSink(pub std::sync::Mutex<Vec<SecurityEvent>>);

impl SecurityEventSink for RecordingSink {
    fn write(&self, event: &SecurityEvent) {
        self.0.lock().expect("sink lock").push(event.clone());
    }
}

/// Builds an `ActionService` with real scan capabilities and the given event sink.
pub fn action_service_with_sink(sink: Arc<dyn SecurityEventSink>) -> Arc<ActionService> {
    action_service_with_code_executor(sink, CodeScanExecutor)
}

/// Readiness probe sharing the executor's provider-less default setup; unit
/// tests exercise classification, not cache sharing.
fn prompt_scan_warmup() -> PromptScanWarmup {
    PromptScanWarmup::new(Arc::new(CachingScannerProvider::default()))
}

/// Builds an `ActionService` with a custom code-scan executor.
pub fn action_service_with_code_executor<E>(
    sink: Arc<dyn SecurityEventSink>,
    code_executor: E,
) -> Arc<ActionService>
where
    E: CapabilityExecutor<Request = CodeScanRequest> + 'static,
{
    Arc::new(ActionService::new(
        ActionRuntime::new(
            ActionId::CodeScan,
            code_executor,
            CodeScanAuditProjector,
            audit_finalizer(sink.clone()),
        ),
        ActionRuntime::new(
            ActionId::PromptScan,
            PromptScanExecutor::default(),
            PromptScanAuditProjector,
            audit_finalizer(sink),
        ),
        prompt_scan_warmup(),
    ))
}

/// Builds an `ActionService` with a custom prompt-scan warmup probe.
///
/// The prompt runtime itself stays the real one: the warmup projection is
/// independent of scanning, so only the probe is substituted.
pub fn action_service_with_warmup<W>(warmup: W) -> Arc<ActionService>
where
    W: asc_action_runtime::CapabilityWarmup<Request = asc_action_types::PromptScanWarmupRequest>
        + 'static,
{
    let sink = Arc::new(NoopSink);
    Arc::new(ActionService::new(
        ActionRuntime::new(
            ActionId::CodeScan,
            CodeScanExecutor,
            CodeScanAuditProjector,
            audit_finalizer(sink.clone()),
        ),
        ActionRuntime::new(
            ActionId::PromptScan,
            PromptScanExecutor::default(),
            PromptScanAuditProjector,
            audit_finalizer(sink),
        ),
        warmup,
    ))
}

/// Builds an `ActionService` with a custom prompt-scan executor.
pub fn action_service_with_prompt_executor<E>(
    sink: Arc<dyn SecurityEventSink>,
    prompt_executor: E,
) -> Arc<ActionService>
where
    E: CapabilityExecutor<Request = PromptScanRequest> + 'static,
{
    Arc::new(ActionService::new(
        ActionRuntime::new(
            ActionId::CodeScan,
            CodeScanExecutor,
            CodeScanAuditProjector,
            audit_finalizer(sink.clone()),
        ),
        ActionRuntime::new(
            ActionId::PromptScan,
            prompt_executor,
            PromptScanAuditProjector,
            audit_finalizer(sink),
        ),
        prompt_scan_warmup(),
    ))
}
