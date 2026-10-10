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

/// The durable sinks plus the shutdown decisions about the final passes.
///
/// The observability lifecycle is independent of the security store, so its
/// close always runs. The security final pass is skipped only when the
/// retention task failed to join and an orphaned pass may still hold the
/// store mutex - skipping it must not take the worker drain or the
/// observability close down with it. Both gated maintenance passes must
/// finish by `final_pass_deadline` and are skipped when the remaining stop
/// budget cannot fit them (`None` runs them without a deadline).
pub(crate) struct ShutdownPlan {
    sinks: DurableSinks,
    security_final_pass: bool,
    final_pass_deadline: Option<std::time::Instant>,
}

impl ShutdownPlan {
    /// Builds a plan from the retention join outcome.
    ///
    /// `security_final_pass` is `true` only when the retention task joined
    /// within the shared stop deadline (or never started). The final passes
    /// must finish by `final_pass_deadline`.
    pub(crate) fn new(
        sinks: DurableSinks,
        security_final_pass: bool,
        final_pass_deadline: Option<std::time::Instant>,
    ) -> Self {
        Self {
            sinks,
            security_final_pass,
            final_pass_deadline,
        }
    }

    /// Both sink lifecycles close (no retention task is outstanding).
    pub(crate) fn full(sinks: DurableSinks) -> Self {
        Self::new(sinks, true, None)
    }

    /// Closes what the shutdown decision still allows.
    ///
    /// The observability close is unconditional; the security final pass
    /// runs only when the retention task joined within the shared stop
    /// deadline, so it can never contend with an orphaned maintenance pass.
    /// Both are skipped when the remaining stop budget cannot fit them.
    pub(crate) fn close(&self) {
        self.sinks
            .observability
            .close_with_deadline(self.final_pass_deadline);
        if self.security_final_pass {
            self.sinks
                .security
                .close_with_deadline(self.final_pass_deadline);
        }
    }
}

#[cfg(test)]
mod tests {
    use asc_event_sink::ConfiguredObservabilitySinks;
    use asc_observability::ObservabilityRecord;
    use serde_json::json;

    use super::*;

    /// Warms both sink lifecycles so each close leaves an observable trace
    /// (the `.maintenance` gate marker next to its database).
    fn warmed_sinks() -> (tempfile::TempDir, DurableSinks) {
        let dir = tempfile::tempdir().expect("temp dir");
        let security = Arc::new(asc_event_sink::ConfiguredSecurityEventSinks::new(
            dir.path().join("security.jsonl"),
            dir.path().join("security.db"),
        ));
        let observability = Arc::new(ConfiguredObservabilitySinks::new(
            dir.path().join("observability.jsonl"),
            dir.path().join("observability.db"),
        ));
        security.warm_sqlite().expect("warm security sqlite");
        let record = ObservabilityRecord::from_json_value(&json!({
            "hook": "before_agent_run",
            "observedAt": "2026-01-01T00:00:00Z",
            "metadata": {"sessionId": "s-1", "runId": "r-1"},
            "metrics": {"prompt": "x"},
        }))
        .expect("observability record");
        observability.record(&record).expect("record one event");
        (
            dir,
            DurableSinks {
                security,
                observability,
            },
        )
    }

    // DJOB-RET-013: a retention join timeout skips only the security final
    // pass - the observability lifecycle is independent of the security
    // store and still closes.
    #[test]
    fn a_retention_join_timeout_skips_only_the_security_final_pass() {
        let (dir, sinks) = warmed_sinks();
        let plan = ShutdownPlan::new(sinks, false, None);
        plan.close();
        assert!(
            !dir.path().join("security.db.maintenance").exists(),
            "the security final pass must be skipped"
        );
        assert!(
            dir.path().join("observability.db.maintenance").exists(),
            "the observability close must still run"
        );
    }

    #[test]
    fn a_joined_retention_task_closes_both_sink_lifecycles() {
        let (dir, sinks) = warmed_sinks();
        let plan = ShutdownPlan::full(sinks);
        plan.close();
        assert!(
            dir.path().join("security.db.maintenance").exists(),
            "the security final pass must run"
        );
        assert!(
            dir.path().join("observability.db.maintenance").exists(),
            "the observability close must run"
        );
    }
}
