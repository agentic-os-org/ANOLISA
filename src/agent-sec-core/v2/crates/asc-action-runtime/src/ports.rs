//! Application execution and output ports; no concrete scanner or writer dependencies.
use asc_action_types::{ActionAttribution, ActionId, ActionOutcome, AuditProjection};
use asc_security_events::SecurityEvent;
use asc_telemetry::{TelemetryRecord, TelemetryStatus};
use std::time::{Duration, Instant};

/// Transport-independent execution lifetime controls.
#[derive(Debug, Clone)]
pub struct ExecutionControl {
    /// Deadline inherited from the transport, not proof that work has stopped.
    pub deadline: Instant,
    /// Cancellation snapshot at dispatch entry.
    pub cancelled: bool,
}

/// Executes a capability without emitting lifecycle outputs itself.
pub trait CapabilityExecutor: Send + Sync {
    /// Typed capability input.
    type Request;
    /// Returns an execution outcome independently of its security verdict.
    fn execute(&self, control: &ExecutionControl, request: &Self::Request) -> ActionOutcome;
}

/// Capability-specific sanitization; new input fields are not implicitly audited.
pub trait AuditProjector: Send + Sync {
    /// Typed capability input.
    type Request;
    /// Projects a returned outcome into safe audit details.
    fn project(&self, request: &Self::Request, outcome: &ActionOutcome) -> AuditProjection;
}

/// Application-facing invocation port implemented by the shared lifecycle runtime.
pub trait Invocation<R>: Send + Sync {
    /// Executes and finalizes before returning, even if the caller stopped waiting.
    ///
    /// # Errors
    /// Returns a payload-free internal error after finalizing an unexpected failure.
    fn invoke(
        &self,
        control: &ExecutionControl,
        attribution: &ActionAttribution,
        request: &R,
    ) -> Result<ActionOutcome, InvokeError>;
}

/// Controlled unhandled execution failure; never contains a panic payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("capability execution failed")]
pub struct InvokeError;

/// Audit destination supplied by the process composition root.
pub trait SecurityEventSink: Send + Sync {
    /// Attempts persistence. Return does not acknowledge a successful insert.
    fn write(&self, event: &SecurityEvent);
}

/// Dedicated telemetry destination; receives only allowlisted records.
pub trait TelemetrySink: Send + Sync {
    /// Checks policy/target readiness before constructing a telemetry record.
    /// Writers must also recheck policy immediately before append.
    fn enabled(&self) -> bool {
        true
    }
    /// Attempts one independent telemetry append.
    fn write(&self, record: &TelemetryRecord) -> TelemetryStatus;
}

/// Safe lifecycle diagnostics; no input, result, path, or error payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Diagnostic {
    /// Invocation entered the shared lifecycle.
    Started(ActionId),
    /// Terminal outcome after output attempts; duration includes finalization.
    Completed {
        /// Scan identity.
        action: ActionId,
        /// Whether execution completed successfully.
        succeeded: bool,
        /// Whole invocation duration, distinct from scanner `elapsed_ms`.
        duration: Duration,
    },
    /// Audit projection failed; a minimal terminal record was used.
    AuditProjectionFailed(ActionId),
    /// The audit sink was called; this is not a persistence success claim.
    AuditAttempted(ActionId),
    /// Audit callback unwound unexpectedly.
    AuditSinkFailed(ActionId),
    /// Telemetry projection or sink unwound unexpectedly.
    TelemetryFailed(ActionId),
    /// Telemetry writer returned a classified status.
    Telemetry {
        /// Scan identity.
        action: ActionId,
        /// Append status.
        status: TelemetryStatus,
    },
}

/// Diagnostic destination independent of both business-data sinks.
pub trait DiagnosticSink: Send + Sync {
    /// Records safe lifecycle information; failures must not change the outcome.
    fn record(&self, diagnostic: &Diagnostic);
}
