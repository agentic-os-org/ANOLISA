//! Typed runtime authorization calls over the existing bounded UDS client.

use std::path::Path;
use std::time::Duration;

use crate::ClientError;
use asc_daemon_protocol::{
    AuthorizationRequest, DaemonRequest, DaemonResponse, Decision, ExecutionReceipt, ReceiptAck,
    method,
};

/// Requests a decision without executing a command.
///
/// # Errors
/// Returns encoding, transport or malformed response failures without retrying.
pub fn authorization_evaluate(
    socket: &Path,
    request: &AuthorizationRequest,
    timeout: Duration,
) -> Result<DaemonResponse<Decision>, ClientError> {
    call_typed(socket, method::AUTHORIZATION_EVALUATE, request, timeout)
}

/// Reports one execution result; callers retain the result if transport fails.
///
/// # Errors
/// Returns encoding, transport or malformed response failures without retrying.
pub fn authorization_report(
    socket: &Path,
    receipt: &ExecutionReceipt,
    timeout: Duration,
) -> Result<DaemonResponse<ReceiptAck>, ClientError> {
    call_typed(socket, method::AUTHORIZATION_REPORT, receipt, timeout)
}

fn call_typed<T: serde::Serialize, R: serde::de::DeserializeOwned>(
    socket: &Path,
    method: &str,
    params: &T,
    timeout: Duration,
) -> Result<DaemonResponse<R>, ClientError> {
    let request = DaemonRequest {
        method: method.to_owned(),
        params: serde_json::to_value(params).map_err(ClientError::Encode)?,
        trace_context: None,
        compatibility: None,
    };
    match crate::call(socket, &request, timeout)? {
        DaemonResponse::Success(response) => Ok(DaemonResponse::success(
            response.request_id,
            serde_json::from_value(response.result).map_err(ClientError::Decode)?,
        )),
        DaemonResponse::Error(response) => Ok(DaemonResponse::Error(response)),
    }
}
