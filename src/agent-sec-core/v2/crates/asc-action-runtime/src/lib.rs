//! Shared invocation lifecycle: capabilities never own event or telemetry writers.
#![forbid(unsafe_code)]
mod finalizer;
mod ports;
mod runtime;
#[cfg(feature = "testing")]
pub mod testing;
pub use asc_telemetry::TelemetryStatus;
pub use finalizer::Finalizer;
pub use ports::{
    AuditProjector, CapabilityExecutor, Diagnostic, DiagnosticSink, ExecutionControl, Invocation,
    InvokeError, SecurityEventSink, TelemetrySink,
};
pub use runtime::ActionRuntime;
