//! Authorized PII method adapter; request failures share action finalization.

use std::sync::Arc;

use asc_action_runtime::{ActionRuntime, ExecutionControl, Finalizer};
use asc_action_types::{ActionAttribution, ActionId, ActionTraceContext, CallerIdentity};
use asc_capability_pii_scan::{
    PiiAuditProjector, PiiRuleSet, PiiScanExecutor, PiiScanOptions, PiiScanRequest, Source,
};
use asc_daemon_core::PeerCredentials;
use asc_daemon_protocol::{DaemonResponse, PiiScanParams, RequestId, error_code};
use asc_daemon_service::DispatchControl;
use serde_json::Value;

pub(super) struct PiiScanHandler {
    runtime: ActionRuntime<PiiScanExecutor, PiiAuditProjector>,
}

impl PiiScanHandler {
    pub(super) fn new(rules: Arc<PiiRuleSet>, finalizer: Finalizer) -> Self {
        Self {
            runtime: ActionRuntime::new(
                ActionId::PiiScan,
                PiiScanExecutor::new(rules),
                PiiAuditProjector,
                finalizer,
            ),
        }
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
        let attribution = ActionAttribution {
            caller: CallerIdentity {
                uid: peer.uid(),
                gid: peer.gid(),
                pid: peer.pid(),
            },
            correlation: trace.correlation,
        };
        let Ok(params) = serde_json::from_value::<PiiScanParams>(params) else {
            return self.reject(request_id, &attribution, error_code::INVALID_REQUEST);
        };
        let Ok(source) = serde_json::from_value::<Source>(Value::String(params.source.clone()))
        else {
            return self.reject(request_id, &attribution, error_code::INVALID_ARGUMENT);
        };
        if !valid_limits(&params) {
            return self.reject(request_id, &attribution, error_code::INVALID_ARGUMENT);
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
        let outcome = self.runtime.invoke(
            &ExecutionControl {
                deadline: control.deadline(),
                cancelled: control.is_cancelled(),
            },
            &attribution,
            &request,
        );
        DaemonResponse::success(request_id, Value::Object(outcome.data))
    }

    fn reject(
        &self,
        request_id: RequestId,
        attribution: &ActionAttribution,
        code: &str,
    ) -> DaemonResponse {
        let (failure, projection) = PiiAuditProjector::invalid_parameters();
        self.runtime.reject(attribution, failure, projection);
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
