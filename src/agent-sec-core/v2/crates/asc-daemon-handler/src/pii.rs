//! Authorized PII method adapter; request failures share action finalization.

use std::sync::Arc;

use asc_action_runtime::ExecutionControl;
use asc_action_types::{ActionTraceContext, PiiScanOptions, PiiScanRequest, Source};
use asc_daemon_core::{ActionService, PeerCredentials};
use asc_daemon_protocol::{DaemonResponse, PiiScanParams, RequestId, error_code};
use asc_daemon_service::DispatchControl;
use serde_json::Value;

pub(super) struct PiiScanHandler {
    application: Arc<ActionService>,
}

impl PiiScanHandler {
    pub(super) fn new(application: Arc<ActionService>) -> Self {
        Self { application }
    }

    pub(super) fn handle(
        &self,
        request_id: RequestId,
        peer: PeerCredentials,
        control: &DispatchControl,
        params: Value,
    ) -> DaemonResponse {
        // Trace normalization is independent of scan-parameter validation. It
        // never accepts caller UID/GID/PID or raw exception text as attribution.
        let trace =
            ActionTraceContext::from_payload(params.get("traceContext").and_then(Value::as_object));
        let Ok(params) = serde_json::from_value::<PiiScanParams>(params) else {
            return self.reject(request_id, peer, trace, error_code::INVALID_REQUEST);
        };
        let Ok(source) = serde_json::from_value::<Source>(Value::String(params.source.clone()))
        else {
            return self.reject(request_id, peer, trace, error_code::INVALID_ARGUMENT);
        };
        if !valid_limits(&params) {
            return self.reject(request_id, peer, trace, error_code::INVALID_ARGUMENT);
        }
        let request = PiiScanRequest {
            text: params.text,
            options: PiiScanOptions {
                source,
                include_low_confidence: params.include_low_confidence,
                raw_evidence: params.raw_evidence,
                redact_output: params.redact_output,
                max_bytes: params.max_bytes,
                input_truncated: params.input_truncated,
                input_bytes_scanned: params.input_bytes_scanned,
            },
            agent_name: trace.agent_name,
        };
        let Ok(outcome) = self.application.pii_scan(
            peer,
            &ExecutionControl {
                deadline: control.deadline(),
                cancelled: control.is_cancelled(),
            },
            trace.correlation,
            &request,
        ) else {
            return DaemonResponse::error(
                request_id,
                error_code::INTERNAL,
                "capability execution failed",
            );
        };
        DaemonResponse::success(request_id, Value::Object(outcome.data))
    }

    fn reject(
        &self,
        request_id: RequestId,
        peer: PeerCredentials,
        trace: ActionTraceContext,
        code: &str,
    ) -> DaemonResponse {
        self.application
            .reject_pii_scan(peer, trace.correlation, trace.agent_name);
        DaemonResponse::error(request_id, code, "PII scan parameters are invalid")
    }
}

fn valid_limits(params: &PiiScanParams) -> bool {
    params.max_bytes != Some(0)
        && params.input_bytes_scanned.is_none_or(|count| {
            count >= params.text.len()
                && count
                    <= params
                        .text
                        .len()
                        .saturating_add(if params.input_truncated { 3 } else { 0 })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_action_runtime::{
        ActionRuntime, CapabilityExecutor, SecurityEventSink,
        testing::{audit_finalizer, discarding_finalizer},
    };
    use asc_action_types::{ActionId, ActionOutcome};
    use asc_capability_code_scan::{CodeScanAuditProjector, CodeScanExecutor};
    use asc_capability_pii_scan::PiiAuditProjector;
    use asc_security_events::SecurityEvent;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Sink(Mutex<Vec<SecurityEvent>>);
    impl SecurityEventSink for Sink {
        fn write(&self, event: &SecurityEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }
    struct PanickingExecutor;
    impl CapabilityExecutor for PanickingExecutor {
        type Request = PiiScanRequest;
        fn execute(&self, _: &ExecutionControl, _: &PiiScanRequest) -> ActionOutcome {
            panic!("PRIVATE_EXECUTOR_PAYLOAD")
        }
    }

    #[test]
    fn unexpected_execution_failure_is_finalized_before_safe_protocol_error() {
        let sink = Arc::new(Sink::default());
        let handler = PiiScanHandler::new(Arc::new(ActionService::new(
            ActionRuntime::new(
                ActionId::CodeScan,
                CodeScanExecutor,
                CodeScanAuditProjector,
                discarding_finalizer(),
            ),
            ActionRuntime::new(
                ActionId::PiiScan,
                PanickingExecutor,
                PiiAuditProjector,
                audit_finalizer(sink.clone()),
            ),
        )));
        let response = handler.handle(
            RequestId::new("test").unwrap(), PeerCredentials::new(1001,1002,1003),
            &DispatchControl::new(Instant::now() + Duration::from_secs(1)),
            serde_json::json!({"text":"PRIVATE_REQUEST", "traceContext":{"traceId":"opaque","agentName":"codex"}}),
        );
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["error"]["code"], error_code::INTERNAL);
        assert_eq!(value["error"]["message"], "capability execution failed");
        assert!(!value.to_string().contains("PRIVATE"));
        let records = sink.0.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].uid, 1001);
        assert_eq!(records[0].pid, 1003);
        assert_eq!(records[0].trace_id, "opaque");
        assert_eq!(records[0].details["error_type"], "InternalExecutionError");
        assert!(
            !serde_json::to_string(&records[0])
                .unwrap()
                .contains("PRIVATE")
        );
    }
}
