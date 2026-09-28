//! Explicit-path event sink adapter for the daemon composition root.

use std::sync::Arc;

use asc_action_runtime::SecurityEventSink;
use asc_event_sink::ConfiguredSecurityEventSinks;
use asc_security_events::SecurityEvent;

/// Bridges the action runtime's port to configured durable event sinks.
#[derive(Clone)]
pub(crate) struct EventSinkAdapter {
    sinks: Arc<ConfiguredSecurityEventSinks>,
}

impl EventSinkAdapter {
    /// Wraps explicit-path configured sinks.
    pub(crate) fn new(sinks: Arc<ConfiguredSecurityEventSinks>) -> Self {
        Self { sinks }
    }
}

impl SecurityEventSink for EventSinkAdapter {
    fn write(&self, event: &SecurityEvent) {
        self.sinks.log_event(event);
    }
}

/// Telemetry is independent of the security-event destinations.
pub(crate) struct TelemetryAdapter(pub asc_event_sink::telemetry::TelemetryWriter);
impl asc_action_runtime::TelemetrySink for TelemetryAdapter {
    fn enabled(&self) -> bool {
        self.0.enabled()
    }
    fn write(
        &self,
        record: &asc_telemetry::TelemetryRecord,
    ) -> asc_action_runtime::TelemetryStatus {
        self.0.write(record)
    }
}

/// Safe diagnostics use stderr/journald without serializing capability payloads.
pub(crate) struct LifecycleDiagnostics<F>(pub F);
impl<F: Fn(&str) + Send + Sync> asc_action_runtime::DiagnosticSink for LifecycleDiagnostics<F> {
    fn record(&self, diagnostic: &asc_action_runtime::Diagnostic) {
        use asc_action_runtime::{Diagnostic, TelemetryStatus};
        let value = match diagnostic {
            Diagnostic::Started(action) => serde_json::json!({
                "component":"action_lifecycle", "phase":"started", "action":action.event_type()}),
            Diagnostic::Completed {
                action,
                succeeded,
                duration,
            } => serde_json::json!({
                "component":"action_lifecycle", "phase":"completed", "action":action.event_type(),
                "succeeded":succeeded, "duration_ms":duration.as_secs_f64() * 1000.0}),
            Diagnostic::Telemetry { action, status } => serde_json::json!({
                "component":"action_lifecycle", "phase":"telemetry", "action":action.event_type(),
                "status":match status { TelemetryStatus::Written => "written", TelemetryStatus::Skipped => "skipped", TelemetryStatus::Failed => "failed" }}),
            other => {
                let (action, code) = match other {
                    Diagnostic::AuditProjectionFailed(action) => {
                        (action, "audit_projection_failed")
                    }
                    Diagnostic::AuditAttempted(action) => (action, "audit_attempted"),
                    Diagnostic::AuditSinkFailed(action) => (action, "audit_sink_failed"),
                    Diagnostic::TelemetryFailed(action) => (action, "telemetry_failed"),
                    _ => return,
                };
                serde_json::json!({"component":"action_lifecycle", "action":action.event_type(), "code":code})
            }
        };
        // Diagnostics cannot turn successful scanning into a broken-stderr panic.
        (self.0)(&value.to_string());
    }
}

/// Foreground observability storage reports failure to its caller.
pub(crate) struct ObservabilitySinkAdapter(pub Arc<asc_event_sink::ConfiguredObservabilitySinks>);

impl asc_daemon_core::ObservabilitySink for ObservabilitySinkAdapter {
    fn write(
        &self,
        record: &asc_observability::ObservabilityRecord,
    ) -> Result<(), asc_daemon_core::ObservabilityWriteError> {
        self.0
            .record(record)
            .map_err(|_| asc_daemon_core::ObservabilityWriteError::Storage)
    }
}

/// Keeps both storage lifecycles alive through transport and blocking-task drain.
pub(crate) struct DurableSinks {
    pub security: Arc<asc_event_sink::ConfiguredSecurityEventSinks>,
    pub observability: Arc<asc_event_sink::ConfiguredObservabilitySinks>,
}

impl DurableSinks {
    pub fn close(&self) {
        self.security.close();
        self.observability.close();
    }
}
