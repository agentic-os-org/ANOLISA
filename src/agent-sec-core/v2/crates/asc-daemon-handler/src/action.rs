//! Code-scan protocol projection over the daemon Action application service.

use asc_action_runtime::ExecutionControl;
use asc_action_types::CodeScanRequest;
use asc_daemon_core::{ActionService, PeerCredentials};
use asc_daemon_protocol::{
    CodeScanParams, DaemonResponse, MAX_DAEMON_ERROR_MESSAGE_BYTES, RequestId, error_code,
};
use asc_daemon_service::DispatchControl;
use std::sync::Arc;

const INVALID_PARAMETER_MESSAGE: &str = "request parameters are invalid";

/// Code-scan protocol adapter backed by the shared action runtime.
pub(super) struct CodeScanHandler {
    application: Arc<ActionService>,
}

impl CodeScanHandler {
    pub(super) fn new(application: Arc<ActionService>) -> Self {
        Self { application }
    }

    /// Runs one scan and projects its result or a parameter failure.
    ///
    /// A scan that produces an error verdict is still a successful request: the
    /// scan ran and returned a verdict the caller must act on. Only malformed
    /// parameters or an unsupported language name become protocol errors.
    pub(super) fn handle(
        &self,
        request_id: RequestId,
        peer: PeerCredentials,
        control: &DispatchControl,
        params: serde_json::Value,
    ) -> DaemonResponse {
        let params: CodeScanParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return DaemonResponse::error(
                    request_id,
                    error_code::INVALID_REQUEST,
                    &bounded_parameter_error(&error),
                );
            }
        };

        let request = CodeScanRequest {
            code: params.code,
            language: params.language,
            rules: params.rules,
            mode: params.mode,
        };
        let Ok(outcome) = self.application.code_scan(
            peer,
            &ExecutionControl {
                deadline: control.deadline(),
                cancelled: control.is_cancelled(),
            },
            &request,
        ) else {
            return DaemonResponse::error(
                request_id,
                error_code::INTERNAL,
                "capability execution failed",
            );
        };
        if outcome.error_type == "ErrUnsupportedLang" {
            let message = outcome
                .error
                .as_deref()
                .and_then(|error| error.strip_prefix("scan error: "))
                .unwrap_or(INVALID_PARAMETER_MESSAGE);
            return DaemonResponse::error(request_id, error_code::INVALID_ARGUMENT, message);
        }
        match project_value(serde_json::Value::Object(outcome.data)) {
            Ok(value) => DaemonResponse::success(request_id, value),
            // A ScanResult is a fixed, bounded shape of owned strings; failing
            // to serialize it would be an internal invariant break, not caller
            // input, so it is projected as an internal error.
            Err(()) => DaemonResponse::error(
                request_id,
                error_code::INTERNAL,
                "scan result is unprojectable",
            ),
        }
    }
}

/// Validates the capability's owned JSON object for transport projection.
fn project_value(value: serde_json::Value) -> Result<serde_json::Value, ()> {
    if value.is_object() {
        Ok(value)
    } else {
        Err(())
    }
}

fn bounded_parameter_error(error: &serde_json::Error) -> String {
    let message = error.to_string();
    if message.len() > MAX_DAEMON_ERROR_MESSAGE_BYTES {
        INVALID_PARAMETER_MESSAGE.to_owned()
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use asc_action_runtime::{ActionRuntime, SecurityEventSink, testing::audit_finalizer};
    use asc_action_types::ActionId;
    use asc_capability_code_scan::{CodeScanAuditProjector, CodeScanExecutor};
    use asc_security_events::SecurityEvent;

    use super::*;

    struct NoopSink;

    impl SecurityEventSink for NoopSink {
        fn write(&self, _: &SecurityEvent) {}
    }

    #[derive(Default)]
    struct RecordingSink(std::sync::Mutex<Vec<SecurityEvent>>);

    impl SecurityEventSink for RecordingSink {
        fn write(&self, event: &SecurityEvent) {
            self.0.lock().expect("sink lock").push(event.clone());
        }
    }

    fn with_sink(sink: Arc<dyn SecurityEventSink>) -> CodeScanHandler {
        CodeScanHandler::new(Arc::new(ActionService::new(
            ActionRuntime::new(
                ActionId::CodeScan,
                CodeScanExecutor,
                CodeScanAuditProjector,
                audit_finalizer(sink),
            ),
            ActionRuntime::new(
                ActionId::PiiScan,
                asc_capability_pii_scan::PiiScanExecutor::new(Arc::new(
                    asc_capability_pii_scan::PiiRuleSet::builtin().unwrap(),
                )),
                asc_capability_pii_scan::PiiAuditProjector,
                asc_action_runtime::testing::discarding_finalizer(),
            ),
        )))
    }

    fn handler() -> CodeScanHandler {
        with_sink(Arc::new(NoopSink))
    }

    fn response(params: serde_json::Value) -> DaemonResponse {
        let control = DispatchControl::new(Instant::now() + Duration::from_secs(1));
        handler().handle(
            RequestId::new("test").expect("non-empty request id"),
            PeerCredentials::new(1000, 1000, 1000),
            &control,
            params,
        )
    }

    fn success_value(response: DaemonResponse) -> serde_json::Value {
        match response {
            DaemonResponse::Success(response) => response.result,
            DaemonResponse::Error(response) => {
                panic!("expected success, got error {}", response.error.message())
            }
        }
    }

    fn error_code_of(response: DaemonResponse) -> String {
        match response {
            DaemonResponse::Error(response) => response.error.code.as_str().to_owned(),
            DaemonResponse::Success(_) => panic!("expected an error response"),
        }
    }

    #[test]
    fn clean_code_scans_to_a_pass_verdict() {
        let value = success_value(response(
            serde_json::json!({"code": "echo hi", "language": "bash"}),
        ));
        assert_eq!(value["ok"], serde_json::json!(true));
        assert_eq!(value["verdict"], serde_json::json!("pass"));
    }

    #[test]
    fn dangerous_code_reports_findings() {
        let value = success_value(response(
            serde_json::json!({"code": "rm -rf /tmp/x", "language": "bash"}),
        ));
        assert_eq!(value["verdict"], serde_json::json!("warn"));
    }

    #[test]
    fn an_error_verdict_is_still_a_success_response() {
        let value = success_value(response(
            serde_json::json!({"code": "   ", "language": "python"}),
        ));
        assert_eq!(value["verdict"], serde_json::json!("error"));
    }

    #[test]
    fn an_unsupported_language_is_invalid_argument() {
        assert_eq!(
            error_code_of(response(
                serde_json::json!({"code": "puts 1", "language": "ruby"})
            )),
            error_code::INVALID_ARGUMENT
        );
    }

    #[test]
    fn missing_required_fields_are_invalid_request() {
        assert_eq!(
            error_code_of(response(serde_json::json!({"language": "bash"}))),
            error_code::INVALID_REQUEST
        );
    }

    #[test]
    fn event_uses_kernel_peer_identity_not_daemon_identity() {
        let sink = Arc::new(RecordingSink::default());
        let handler = with_sink(sink.clone());
        let control = DispatchControl::new(Instant::now() + Duration::from_secs(1));
        let response = handler.handle(
            RequestId::new("test").expect("non-empty request id"),
            PeerCredentials::new(1001, 1002, 1003),
            &control,
            serde_json::json!({"code": "echo hi", "language": "bash"}),
        );

        let _ = success_value(response);
        let events = sink.0.lock().expect("sink lock");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].uid, 1001);
        assert_eq!(events[0].pid, 1003);
        assert_eq!(events[0].details["request"]["code"], "echo hi");
    }

    struct PanickingExecutor;
    impl asc_action_runtime::CapabilityExecutor for PanickingExecutor {
        type Request = CodeScanRequest;
        fn execute(
            &self,
            _: &ExecutionControl,
            _: &CodeScanRequest,
        ) -> asc_action_types::ActionOutcome {
            panic!("SECRET_EXECUTOR_PAYLOAD")
        }
    }

    #[test]
    fn unexpected_execution_failure_is_a_safe_core_error_after_finalization() {
        let sink = Arc::new(RecordingSink::default());
        let handler = CodeScanHandler::new(Arc::new(ActionService::new(
            ActionRuntime::new(
                ActionId::CodeScan,
                PanickingExecutor,
                CodeScanAuditProjector,
                audit_finalizer(sink.clone()),
            ),
            ActionRuntime::new(
                ActionId::PiiScan,
                asc_capability_pii_scan::PiiScanExecutor::new(Arc::new(
                    asc_capability_pii_scan::PiiRuleSet::builtin().unwrap(),
                )),
                asc_capability_pii_scan::PiiAuditProjector,
                asc_action_runtime::testing::discarding_finalizer(),
            ),
        )));
        let response = handler.handle(
            RequestId::new("test").unwrap(),
            PeerCredentials::new(1001, 1002, 1003),
            &DispatchControl::new(Instant::now() + Duration::from_secs(1)),
            serde_json::json!({"code":"SECRET_REQUEST", "language":"bash"}),
        );
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["error"]["code"], error_code::INTERNAL);
        assert_eq!(value["error"]["message"], "capability execution failed");
        assert!(!value.to_string().contains("SECRET"));
        let events = sink.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].details["error_type"], "InternalExecutionError");
        assert!(
            !serde_json::to_string(&events[0])
                .unwrap()
                .contains("SECRET")
        );
    }

    #[test]
    fn llm_mode_yields_an_engine_unavailable_verdict_not_a_protocol_error() {
        let value = success_value(response(
            serde_json::json!({"code": "echo hi", "language": "bash", "mode": "llm"}),
        ));
        assert_eq!(
            value["summary"],
            serde_json::json!("scan error: LLM model not available")
        );
    }
}
