//! Code-scan Action adapter invoked by the daemon dispatcher.
//!
//! Translates one `action.code_scan` request into a capability call and back.
//! The capability is pure and stateless, so this handler holds no application
//! port: it decodes parameters, resolves the language, runs the scan, and
//! projects the scan result as the method result.

use asc_action_runtime::{ActionRuntime, ExecutionControl, Finalizer};
use asc_action_types::{ActionAttribution, ActionId, CallerIdentity, Correlation};
use asc_capability_code_scan::{CodeScanAuditProjector, CodeScanExecutor, CodeScanRequest};
use asc_daemon_core::PeerCredentials;
use asc_daemon_protocol::{
    CodeScanParams, DaemonResponse, MAX_DAEMON_ERROR_MESSAGE_BYTES, RequestId, error_code,
};
use asc_daemon_service::DispatchControl;

const INVALID_PARAMETER_MESSAGE: &str = "request parameters are invalid";

/// Code-scan protocol adapter backed by the shared action runtime.
pub(super) struct CodeScanHandler {
    runtime: ActionRuntime<CodeScanExecutor, CodeScanAuditProjector>,
}

impl CodeScanHandler {
    pub(super) fn new(finalizer: Finalizer) -> Self {
        Self {
            runtime: ActionRuntime::new(
                ActionId::CodeScan,
                CodeScanExecutor,
                CodeScanAuditProjector,
                finalizer,
            ),
        }
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
        let attribution = ActionAttribution {
            caller: CallerIdentity {
                uid: peer.uid(),
                gid: peer.gid(),
                pid: peer.pid(),
            },
            correlation: Correlation::default(),
        };
        let outcome = self.runtime.invoke(
            &ExecutionControl {
                deadline: control.deadline(),
                cancelled: control.is_cancelled(),
            },
            &attribution,
            &request,
        );
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

    use asc_action_runtime::SecurityEventSink;
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

    fn handler() -> CodeScanHandler {
        CodeScanHandler::new(Finalizer::new(Arc::new(NoopSink)))
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
        let handler = CodeScanHandler::new(Finalizer::new(sink.clone()));
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
