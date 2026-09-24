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

/// Registers `SkillSec` and Code Scan against the same process-owned lifecycle outputs.
#[must_use]
pub fn skill_application(
    finalizer: Finalizer,
    executor: asc_capability_skill_sec::executor::SkillSecExecutor,
) -> Arc<ActionService> {
    Arc::new(
        ActionService::new(ActionRuntime::new(
            ActionId::CodeScan,
            CodeScanExecutor,
            CodeScanAuditProjector,
            finalizer.clone(),
        ))
        .with_skill_sec(ActionRuntime::new(
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
