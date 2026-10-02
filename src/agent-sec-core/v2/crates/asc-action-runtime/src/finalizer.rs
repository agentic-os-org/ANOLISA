//! One common terminal emission path for every registered capability runtime.
use crate::{Diagnostic, DiagnosticSink, SecurityEventSink, TelemetrySink};
use asc_action_types::{ActionId, ActionOutcome, AuditProjection, CallerIdentity};
use asc_security_events::{EventResult, SecurityEvent};
use asc_telemetry::{ScanTelemetryInput, TelemetryRecord};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

/// Process-owned outputs shared by all capability runtimes.
#[derive(Clone)]
pub struct Finalizer {
    audit: Arc<dyn SecurityEventSink>,
    telemetry: Arc<dyn TelemetrySink>,
    diagnostics: Arc<dyn DiagnosticSink>,
}

impl Finalizer {
    /// Requires explicit outputs; production never defaults to discarding records.
    #[must_use]
    pub fn new(
        audit: Arc<dyn SecurityEventSink>,
        telemetry: Arc<dyn TelemetrySink>,
        diagnostics: Arc<dyn DiagnosticSink>,
    ) -> Self {
        Self {
            audit,
            telemetry,
            diagnostics,
        }
    }

    pub(crate) fn diagnostic(&self, diagnostic: Diagnostic) {
        let _ = catch_unwind(AssertUnwindSafe(|| self.diagnostics.record(&diagnostic)));
    }

    pub(crate) fn finalize(
        &self,
        action: ActionId,
        caller: &CallerIdentity,
        outcome: &ActionOutcome,
        projection: AuditProjection,
        unhandled: bool,
    ) {
        // Freeze attribution once while the request context is active, before either sink runs.
        let mut context = asc_observability::snapshot();
        let mut event = SecurityEvent::new(
            action.event_type(),
            action.category(),
            projection.into_details(),
        );
        event.result = if outcome.success {
            EventResult::Succeeded
        } else {
            EventResult::Failed
        };
        event.pid = caller.pid;
        event.uid = caller.uid;
        // This event schema retains the opaque compatibility label, not SDK TraceId.
        event.trace_id = context.compatibility.trace_id.unwrap_or_default();
        event.session_id = context.agent.remove("session_id");
        event.run_id = context.agent.remove("run_id");
        event.call_id = context.agent.remove("call_id");
        event.tool_call_id = context.agent.remove("tool_call_id");
        // No shared transaction: the second destination is attempted even if the first unwinds.
        let audit = catch_unwind(AssertUnwindSafe(|| self.audit.write(&event)));
        self.diagnostic(if audit.is_ok() {
            Diagnostic::AuditAttempted(action)
        } else {
            Diagnostic::AuditSinkFailed(action)
        });
        let telemetry = catch_unwind(AssertUnwindSafe(|| {
            if !self.telemetry.enabled() {
                return crate::TelemetryStatus::Skipped;
            }
            // Project original finalized facts, independent of audit projector/write success.
            let record = TelemetryRecord::for_scan(&ScanTelemetryInput {
                event_type: action.event_type(),
                category: action.category(),
                succeeded: outcome.success,
                timestamp: &event.timestamp,
                result: &outcome.data,
                error_type: &outcome.error_type,
                exit_code: (!unhandled).then_some(outcome.exit_code),
                agent_name: context.agent.get("agent_name").map(String::as_str),
            });
            self.telemetry.write(&record)
        }));
        self.diagnostic(match telemetry {
            Ok(status) => Diagnostic::Telemetry { action, status },
            Err(_) => Diagnostic::TelemetryFailed(action),
        });
    }
}
