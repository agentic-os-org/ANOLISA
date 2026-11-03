//! The process-owned capability inventory. Handlers never assemble runtimes or sinks.
use asc_action_runtime::{ActionRuntime, Finalizer};
use asc_action_types::ActionId;
use asc_capability_code_scan::{CodeScanAuditProjector, CodeScanExecutor};
use asc_capability_pii_scan::{PiiAuditProjector, PiiRuleSet, PiiScanExecutor};
use asc_capability_prompt_scan::{
    CachingScannerProvider, PromptScanAuditProjector, PromptScanExecutor, PromptScanWarmup,
    ScannerProvider,
};
use asc_daemon_core::ActionService;
use std::sync::Arc;

/// Composes every implemented scan with the same required finalization infrastructure.
#[must_use]
pub fn scan_application(finalizer: Finalizer, pii_rules: Arc<PiiRuleSet>) -> Arc<ActionService> {
    Arc::new(scan_service(finalizer, pii_rules))
}

fn scan_service(finalizer: Finalizer, pii_rules: Arc<PiiRuleSet>) -> ActionService {
    // The finalizer shares one sink set across capabilities; cloning it forks
    // the Arc handles, not the sinks, so every runtime finalizes identically.
    //
    // The provider builds scanners lazily per mode, so daemon startup
    // never depends on model-service configuration and the rule-set
    // compilation cost is paid once per mode, not per request. The warmup
    // probe shares it, so a successful check warms exactly the scanner
    // instance the next scan of that mode and model reuses.
    let provider: Arc<dyn ScannerProvider> = Arc::new(CachingScannerProvider::default());
    ActionService::new(
        ActionRuntime::new(
            ActionId::CodeScan,
            CodeScanExecutor::from_env(),
            CodeScanAuditProjector,
            finalizer.clone(),
        ),
        ActionRuntime::new(
            ActionId::PiiScan,
            PiiScanExecutor::new(pii_rules),
            PiiAuditProjector,
            finalizer.clone(),
        ),
        ActionRuntime::new(
            ActionId::PromptScan,
            PromptScanExecutor::new(Arc::clone(&provider)),
            PromptScanAuditProjector,
            finalizer,
        ),
        PromptScanWarmup::new(provider),
    )
}

/// Registers `SkillSec`, Code Scan, PII and prompt scan against the same lifecycle outputs.
#[must_use]
pub fn skill_application(
    finalizer: Finalizer,
    pii_rules: Arc<PiiRuleSet>,
    executor: asc_capability_skill_sec::executor::SkillSecExecutor,
) -> Arc<ActionService> {
    Arc::new(
        scan_service(finalizer.clone(), pii_rules).with_skill_sec(ActionRuntime::new(
            ActionId::SkillSec,
            executor,
            asc_capability_skill_sec::executor::SkillSecAuditProjector,
            finalizer,
        )),
    )
}

/// Isolates a daemon-owned Skill task from any request metadata on the current thread.
///
/// Call once per dequeued notification or startup recovery item. Each nested action
/// still has its own Runtime invocation and finalization.
pub fn skill_task_scope<T>(operation: &'static str, work: impl FnOnce() -> T) -> T {
    let context = asc_observability::Context::new();
    let _parent = context.clone().attach();
    let span = asc_observability::parent_span(
        tracing::info_span!(parent: None, "skillsec.task", operation),
        context,
    );
    span.in_scope(|| {
        let _context = asc_observability::request_context().attach();
        work()
    })
}
