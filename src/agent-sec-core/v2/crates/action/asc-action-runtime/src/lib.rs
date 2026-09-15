//! Shared execution finalization for daemon actions.
//!
//! Capability crates execute and sanitize their own domain data; this crate
//! emits one terminal event whenever an accepted invocation reaches finalization.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Instant;

use asc_action_types::{ActionAttribution, ActionId, ActionOutcome, AuditProjection, Failure};
use asc_security_events::{EventResult, SecurityEvent};

/// Transport-independent execution lifetime controls.
#[derive(Debug, Clone)]
pub struct ExecutionControl {
    /// Hard deadline inherited from the transport.
    pub deadline: Instant,
    /// Whether the caller has left the transport dispatch lifetime.
    pub cancelled: bool,
}

/// Executes one capability request.
pub trait CapabilityExecutor {
    /// Request type accepted by this capability.
    type Request;

    /// Executes one request and always returns a v1-compatible outcome.
    fn execute(&self, control: &ExecutionControl, request: &Self::Request) -> ActionOutcome;
}

/// Produces sanitized audit data for one capability.
pub trait AuditProjector {
    /// Request type accepted by this capability.
    type Request;

    /// Maps a request and outcome to v1-compatible event details.
    fn project(&self, request: &Self::Request, outcome: &ActionOutcome) -> AuditProjection;
}

/// Durable event destination supplied by the composition root.
pub trait SecurityEventSink: Send + Sync {
    /// Writes one event using the host's failure-isolation policy.
    fn write(&self, event: &SecurityEvent);
}

/// Builds and emits terminal events.
#[derive(Clone)]
pub struct Finalizer {
    sink: Arc<dyn SecurityEventSink>,
}

impl Finalizer {
    /// Creates a finalizer backed by `sink`.
    #[must_use]
    pub fn new(sink: Arc<dyn SecurityEventSink>) -> Self {
        Self { sink }
    }

    /// Emits exactly one v1-compatible event for an accepted invocation.
    ///
    /// Caller cancellation never suppresses finalization while the host keeps the
    /// invocation alive: blocking work may complete after the response timeout.
    /// A bounded daemon shutdown can abort work before it reaches this method.
    pub fn finalize(
        &self,
        action: ActionId,
        attribution: &ActionAttribution,
        outcome: &ActionOutcome,
        projection: AuditProjection,
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
        self.sink.write(&event);
    }
}

/// Composes one capability executor, projector, and shared finalizer.
pub struct ActionRuntime<E, P> {
    action: ActionId,
    executor: E,
    projector: P,
    finalizer: Finalizer,
}

impl<E, P> ActionRuntime<E, P>
where
    E: CapabilityExecutor,
    P: AuditProjector<Request = E::Request>,
{
    /// Creates a runtime for one registered action.
    #[must_use]
    pub fn new(action: ActionId, executor: E, projector: P, finalizer: Finalizer) -> Self {
        Self {
            action,
            executor,
            projector,
            finalizer,
        }
    }

    /// Executes, projects, and finalizes exactly once.
    pub fn invoke(
        &self,
        control: &ExecutionControl,
        attribution: &ActionAttribution,
        request: &E::Request,
    ) -> ActionOutcome {
        let outcome = self.executor.execute(control, request);
        let projection = self.projector.project(request, &outcome);
        self.finalizer
            .finalize(self.action, attribution, &outcome, projection);
        outcome
    }

    /// Finalizes an identified, authorized action rejected before request decoding.
    ///
    /// The capability supplies only sanitized failure fields and audit data.
    /// The entrypoint must return after this call instead of invoking execution.
    /// Envelope, authorization, and transport rejections belong to their ingress.
    pub fn reject(
        &self,
        attribution: &ActionAttribution,
        failure: Failure,
        projection: AuditProjection,
    ) -> ActionOutcome {
        let outcome = ActionOutcome {
            success: false,
            exit_code: failure.exit_code,
            error: failure.error,
            error_type: failure.error_type,
            data: serde_json::Map::new(),
        };
        self.finalizer
            .finalize(self.action, attribution, &outcome, projection);
        outcome
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use asc_action_types::{CallerIdentity, Correlation};
    use serde_json::{Map, Value, json};

    use super::*;

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<SecurityEvent>>);

    impl SecurityEventSink for RecordingSink {
        fn write(&self, event: &SecurityEvent) {
            self.0.lock().expect("sink lock").push(event.clone());
        }
    }

    struct Executor;

    impl CapabilityExecutor for Executor {
        type Request = Map<String, Value>;

        fn execute(&self, _: &ExecutionControl, _: &Self::Request) -> ActionOutcome {
            ActionOutcome {
                success: true,
                exit_code: 0,
                error: None,
                error_type: String::new(),
                data: Map::from_iter([(String::from("verdict"), json!("pass"))]),
            }
        }
    }

    struct Projector;

    impl AuditProjector for Projector {
        type Request = Map<String, Value>;

        fn project(&self, request: &Self::Request, outcome: &ActionOutcome) -> AuditProjection {
            AuditProjection::Completed {
                request: request.clone(),
                result: outcome.data.clone(),
                failure: None,
            }
        }
    }

    #[test]
    fn finalizes_once_with_peer_attribution_after_cancellation() {
        let sink = Arc::new(RecordingSink::default());
        let runtime = ActionRuntime::new(
            ActionId::CodeScan,
            Executor,
            Projector,
            Finalizer::new(sink.clone()),
        );
        let request = Map::from_iter([(String::from("code"), json!("echo hi"))]);
        let attribution = ActionAttribution {
            caller: CallerIdentity {
                uid: 1001,
                gid: 1002,
                pid: 1003,
            },
            correlation: Correlation::default(),
        };
        let outcome = runtime.invoke(
            &ExecutionControl {
                deadline: Instant::now(),
                cancelled: true,
            },
            &attribution,
            &request,
        );

        assert!(outcome.success);
        let records = sink.0.lock().expect("sink lock");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].uid, 1001);
        assert_eq!(records[0].pid, 1003);
        assert_eq!(records[0].trace_id, "");
        assert_eq!(records[0].details["request"], json!({"code": "echo hi"}));
    }
}
