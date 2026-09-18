//! One common terminal emission path for every registered capability runtime.
use crate::{Diagnostic, DiagnosticSink, SecurityEventSink, TelemetrySink};
use asc_action_types::{ActionAttribution, ActionId, ActionOutcome, AuditProjection};
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
        attribution: &ActionAttribution,
        outcome: &ActionOutcome,
        projection: AuditProjection,
        unhandled: bool,
    ) {
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
        event.pid = attribution.caller.pid;
        event.uid = attribution.caller.uid;
        event.trace_id.clone_from(&attribution.correlation.trace_id);
        event
            .session_id
            .clone_from(&attribution.correlation.session_id);
        event.run_id.clone_from(&attribution.correlation.run_id);
        event.call_id.clone_from(&attribution.correlation.call_id);
        event
            .tool_call_id
            .clone_from(&attribution.correlation.tool_call_id);
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
                agent_name: attribution.agent_name.as_deref(),
            });
            self.telemetry.write(&record)
        }));
        self.diagnostic(match telemetry {
            Ok(status) => Diagnostic::Telemetry { action, status },
            Err(_) => Diagnostic::TelemetryFailed(action),
        });
    }
}
