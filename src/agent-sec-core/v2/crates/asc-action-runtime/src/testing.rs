//! Explicit discarding fixtures for transport tests. Never selected by production constructors.
use crate::{
    Diagnostic, DiagnosticSink, Finalizer, SecurityEventSink, TelemetrySink, TelemetryStatus,
};
use asc_security_events::SecurityEvent;
use asc_telemetry::TelemetryRecord;
use std::sync::Arc;

struct Discard;
impl SecurityEventSink for Discard {
    fn write(&self, _: &SecurityEvent) {}
}
impl TelemetrySink for Discard {
    fn write(&self, _: &TelemetryRecord) -> TelemetryStatus {
        TelemetryStatus::Skipped
    }
}
impl DiagnosticSink for Discard {
    fn record(&self, _: &Diagnostic) {}
}
/// Creates an explicitly non-persistent finalizer for tests that only exercise transport.
#[must_use]
pub fn discarding_finalizer() -> Finalizer {
    Finalizer::new(Arc::new(Discard), Arc::new(Discard), Arc::new(Discard))
}
/// Uses a recording audit sink while discarding the other outputs in focused tests.
#[must_use]
pub fn audit_finalizer(audit: Arc<dyn SecurityEventSink>) -> Finalizer {
    Finalizer::new(audit, Arc::new(Discard), Arc::new(Discard))
}
