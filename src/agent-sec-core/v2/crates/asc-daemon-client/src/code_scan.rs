//! Typed code scanning over the authenticated daemon transport.

use std::path::PathBuf;
use std::time::Duration;

pub use asc_daemon_protocol::{
    CodeScanParams as ScanRequest, CompatibilityV1, DaemonResponse, ErrorResponse, RequestId,
    SuccessResponse, TraceCarrierV1,
};
use asc_daemon_protocol::{DaemonRequest, method::ACTION_CODE_SCAN};
use serde::{Deserialize, Serialize};

/// Per-call correlation; these labels do not grant authority.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// W3C tracing carrier, subject to the daemon's propagation limits.
    pub trace_context: Option<TraceCarrierV1>,
    /// Compatibility labels for existing audit consumers.
    pub compatibility: Option<CompatibilityV1>,
}

/// Scanner verdict, distinct from transport and daemon admission errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The completed scan permits the input.
    Pass,
    /// The completed scan reports a warning.
    Warn,
    /// The completed scan denies the input.
    Deny,
    /// The scanner could not complete; this is never permission to proceed.
    Error,
}

/// One scanner finding; evidence may contain caller-provided source text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    /// Component rule identifier.
    pub rule_id: String,
    /// Component severity label.
    pub severity: String,
    /// Chinese rule explanation.
    pub desc_zh: String,
    /// English rule explanation.
    pub desc_en: String,
    /// Matching source excerpts; callers control their disclosure.
    pub evidence: Vec<String>,
}

/// Complete component result, including unsuccessful scans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanResult {
    /// Whether the scan completed, not whether execution is authorized.
    pub ok: bool,
    /// Policy outcome or a scanner failure.
    pub verdict: Verdict,
    /// Component summary; may contain sensitive input.
    pub summary: String,
    /// Matching rules and evidence.
    pub findings: Vec<Finding>,
    /// Language selected by the component.
    pub language: String,
    /// Scanner version reported by the daemon.
    pub engine_version: String,
    /// Server-reported elapsed time.
    pub elapsed_ms: u64,
}

/// Local call failures; daemon errors remain [`DaemonResponse::Error`].
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// The authenticated transport could not complete.
    #[error(transparent)]
    Transport(#[from] crate::ClientError),
    /// The component returned an unsupported or malformed result.
    #[error("invalid code scan result: {0}")]
    Decode(#[source] serde_json::Error),
    /// The completion flag contradicts the verdict.
    #[error("code scan completion flag contradicts its verdict")]
    InconsistentResult,
}

/// Reusable daemon address; construction performs no I/O or service management.
#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    /// Binds calls to the supplied daemon socket.
    #[must_use]
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// Scans once using the caller's connection deadline and correlation.
    ///
    /// The caller blocks while the transport verifies UID 0 and performs I/O.
    /// Timeout does not prove server cancellation or make replay safe.
    /// Deny and warn are completed scans; the application decides how to adopt
    /// them. A scanner error is preserved as a result with `ok == false`.
    ///
    /// # Errors
    /// Returns transport, result-shape or contradictory-verdict failures.
    pub fn scan_code(
        &self,
        input: &ScanRequest,
        context: &Context,
        timeout: Duration,
    ) -> Result<DaemonResponse<ScanResult>, ScanError> {
        let request = request(input, context)?;
        decode(crate::call(&self.socket, &request, timeout)?)
    }
}

fn request(input: &ScanRequest, context: &Context) -> Result<DaemonRequest, ScanError> {
    Ok(DaemonRequest {
        method: ACTION_CODE_SCAN.to_owned(),
        params: serde_json::to_value(input).map_err(crate::ClientError::Encode)?,
        trace_context: context.trace_context.clone(),
        compatibility: context.compatibility.clone(),
    })
}

fn decode(response: DaemonResponse) -> Result<DaemonResponse<ScanResult>, ScanError> {
    match response {
        DaemonResponse::Error(error) => Ok(DaemonResponse::Error(error)),
        DaemonResponse::Success(success) => {
            let result: ScanResult =
                serde_json::from_value(success.result).map_err(ScanError::Decode)?;
            if result.ok == (result.verdict == Verdict::Error) {
                return Err(ScanError::InconsistentResult);
            }
            Ok(DaemonResponse::Success(SuccessResponse {
                request_id: success.request_id,
                result,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(ok: bool, verdict: &str) -> DaemonResponse {
        serde_json::from_value(json!({
            "requestId": "scan-1",
            "result": {
                "ok": ok, "verdict": verdict, "summary": "", "findings": [],
                "language": "bash", "engine_version": "1", "elapsed_ms": 2
            }
        }))
        .unwrap()
    }

    #[test]
    fn completed_verdicts_and_scanner_failure_preserve_the_request_id() {
        for (ok, name, verdict) in [
            (true, "pass", Verdict::Pass),
            (true, "warn", Verdict::Warn),
            (true, "deny", Verdict::Deny),
            (false, "error", Verdict::Error),
        ] {
            let DaemonResponse::Success(value) = decode(response(ok, name)).unwrap() else {
                panic!("expected a completed action");
            };
            assert_eq!(value.request_id.as_str(), "scan-1");
            assert_eq!(value.result.verdict, verdict);
            assert_eq!(value.result.ok, ok);
        }
    }

    #[test]
    fn invalid_or_contradictory_results_never_become_pass() {
        for (ok, verdict) in [(false, "pass"), (false, "deny"), (true, "error")] {
            assert!(matches!(
                decode(response(ok, verdict)),
                Err(ScanError::InconsistentResult)
            ));
        }
        assert!(matches!(
            decode(response(true, "unknown")),
            Err(ScanError::Decode(_))
        ));
        let mut value = response(true, "pass");
        if let DaemonResponse::Success(success) = &mut value {
            success.result.as_object_mut().unwrap().remove("findings");
        }
        assert!(matches!(decode(value), Err(ScanError::Decode(_))));
    }

    #[test]
    fn daemon_rejection_keeps_code_message_and_request_id() {
        let value = serde_json::from_value(json!({
            "requestId": "denied-1",
            "error": {"code": "permission_denied", "message": "not allowed"}
        }))
        .unwrap();
        let DaemonResponse::Error(error) = decode(value).unwrap() else {
            panic!("expected a daemon rejection");
        };
        assert_eq!(error.request_id.as_str(), "denied-1");
        assert_eq!(error.error.code.as_str(), "permission_denied");
        assert_eq!(error.error.message(), "not allowed");
    }

    #[test]
    fn typed_request_preserves_component_options_and_context() {
        let input = ScanRequest {
            code: "echo hello".into(),
            language: "bash".into(),
            rules: Some(vec!["rule-1".into()]),
            mode: Some("fast".into()),
        };
        let context = Context {
            compatibility: Some(CompatibilityV1 {
                version: 1,
                trace_id: Some("trace-1".into()),
                invocation_label: Some("call-1".into()),
            }),
            ..Context::default()
        };
        let wire = request(&input, &context).unwrap();
        assert_eq!(wire.method, ACTION_CODE_SCAN);
        assert_eq!(wire.params, serde_json::to_value(input).unwrap());
        assert_eq!(wire.compatibility, context.compatibility);
    }

    #[test]
    fn unavailable_daemon_and_invalid_deadline_remain_errors() {
        let client = Client::new("/does-not-exist/aw-sdk/daemon.sock");
        let input = ScanRequest {
            code: "echo hello".into(),
            language: "bash".into(),
            rules: None,
            mode: None,
        };
        for (timeout, invalid) in [(Duration::ZERO, true), (Duration::from_secs(1), false)] {
            let error = client
                .scan_code(&input, &Context::default(), timeout)
                .unwrap_err();
            assert_eq!(
                matches!(
                    error,
                    ScanError::Transport(crate::ClientError::InvalidTimeout)
                ),
                invalid
            );
            if !invalid {
                assert!(matches!(
                    error,
                    ScanError::Transport(crate::ClientError::Connect(_))
                ));
            }
        }
    }
}
