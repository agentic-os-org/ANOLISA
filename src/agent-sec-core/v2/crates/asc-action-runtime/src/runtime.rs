//! The only execution path used by registered scan application operations.
use crate::{
    AuditProjector, CapabilityExecutor, Diagnostic, ExecutionControl, Finalizer, Invocation,
    InvokeError,
};
use asc_action_types::{ActionAttribution, ActionId, ActionOutcome, AuditProjection, Failure};
use serde_json::Map;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

/// A typed capability registration with mandatory shared finalization.
pub struct ActionRuntime<E, P> {
    action: ActionId,
    executor: E,
    projector: P,
    finalizer: Finalizer,
}

impl<E, P> ActionRuntime<E, P> {
    /// Registers execution and safe projection with process-owned output infrastructure.
    #[must_use]
    pub fn new(action: ActionId, executor: E, projector: P, finalizer: Finalizer) -> Self {
        Self {
            action,
            executor,
            projector,
            finalizer,
        }
    }
}

impl<E, P> Invocation<E::Request> for ActionRuntime<E, P>
where
    E: CapabilityExecutor,
    P: AuditProjector<Request = E::Request>,
{
    fn invoke(
        &self,
        control: &ExecutionControl,
        attribution: &ActionAttribution,
        request: &E::Request,
    ) -> Result<ActionOutcome, InvokeError> {
        let started = Instant::now();
        self.finalizer.diagnostic(Diagnostic::Started(self.action));
        // Hosts must install a payload-free panic hook. Abort/OOM/process kill cannot be recovered.
        let execution = catch_unwind(AssertUnwindSafe(|| self.executor.execute(control, request)));
        let unhandled = execution.is_err();
        let outcome = execution.unwrap_or_else(|_| ActionOutcome {
            success: false,
            exit_code: 1,
            error: Some("capability execution failed".to_owned()),
            error_type: "InternalExecutionError".to_owned(),
            data: Map::new(),
        });
        let projection = if unhandled {
            minimal_failure("InternalExecutionError")
        } else {
            catch_unwind(AssertUnwindSafe(|| {
                self.projector.project(request, &outcome)
            }))
            .unwrap_or_else(|_| {
                self.finalizer
                    .diagnostic(Diagnostic::AuditProjectionFailed(self.action));
                minimal_failure("AuditProjectionError")
            })
        };
        self.finalizer
            .finalize(self.action, attribution, &outcome, projection, unhandled);
        self.finalizer.diagnostic(Diagnostic::Completed {
            action: self.action,
            succeeded: outcome.success,
            duration: started.elapsed(),
        });
        if unhandled {
            Err(InvokeError)
        } else {
            Ok(outcome)
        }
    }

    fn reject(
        &self,
        attribution: &ActionAttribution,
        failure: Failure,
        projection: AuditProjection,
    ) -> ActionOutcome {
        let started = Instant::now();
        self.finalizer.diagnostic(Diagnostic::Started(self.action));
        let outcome = ActionOutcome {
            success: false,
            exit_code: failure.exit_code,
            error: failure.error,
            error_type: failure.error_type,
            data: Map::new(),
        };
        self.finalizer
            .finalize(self.action, attribution, &outcome, projection, false);
        self.finalizer.diagnostic(Diagnostic::Completed {
            action: self.action,
            succeeded: false,
            duration: started.elapsed(),
        });
        outcome
    }
}

fn minimal_failure(error_type: &str) -> AuditProjection {
    // Never serialize a request or panic payload on an unreviewed failure path.
    AuditProjection::Failed {
        request: Map::new(),
        error: "internal failure".to_owned(),
        error_type: error_type.to_owned(),
    }
}
