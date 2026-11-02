//! Handles versioning, protocol decoding, budgets and error mapping only; Cedar validation stays
//! on the core call path.

use asc_daemon_core::PeerCredentials;
use asc_daemon_core::intent::{IntentInput, IntentService, IntentServiceError};
use asc_daemon_protocol::intent::{
    INTENT_REQUEST_BYTES, INTENT_REQUEST_VERSION, INTENT_TASK_BYTES, IntentProposeParams,
    IntentProposeResult,
};
use asc_daemon_protocol::{DaemonRequest, DaemonResponse, RequestId, error_code};
use asc_daemon_service::DispatchControl;

pub(crate) fn handle(
    request_id: RequestId,
    peer: PeerCredentials,
    control: &DispatchControl,
    service: Option<&IntentService>,
    request: DaemonRequest,
) -> DaemonResponse {
    let result = (|| {
        let encoded =
            serde_json::to_vec(&request).map_err(|_| IntentServiceError::InvalidArgument)?;
        if encoded.len() >= INTENT_REQUEST_BYTES {
            return Err(IntentServiceError::ResourceExhausted);
        }
        let params: IntentProposeParams = serde_json::from_value(request.params)
            .map_err(|_| IntentServiceError::InvalidArgument)?;
        let input = decode(params)?;
        let service = service.ok_or(IntentServiceError::Unavailable)?;
        let outcome =
            service.propose(peer, request_id.as_str(), input, control.deadline(), || {
                control.is_cancelled()
            })?;
        let value = serde_json::to_value(outcome).map_err(|_| IntentServiceError::Internal)?;
        let response: IntentProposeResult =
            serde_json::from_value(value).map_err(|_| IntentServiceError::Internal)?;
        serde_json::to_value(response).map_err(|_| IntentServiceError::Internal)
    })();
    match result {
        Ok(result) => DaemonResponse::success(request_id, result),
        Err(error) => {
            let code = match error {
                IntentServiceError::InvalidArgument => error_code::INVALID_ARGUMENT,
                IntentServiceError::ResourceExhausted => error_code::RESOURCE_EXHAUSTED,
                IntentServiceError::Unavailable => error_code::UNAVAILABLE,
                IntentServiceError::DeadlineExceeded => error_code::DEADLINE_EXCEEDED,
                IntentServiceError::Internal => error_code::INTERNAL,
            };
            DaemonResponse::error(request_id, code, &error.to_string())
        }
    }
}

fn decode(params: IntentProposeParams) -> Result<IntentInput, IntentServiceError> {
    if params.format_version != INTENT_REQUEST_VERSION {
        return Err(IntentServiceError::InvalidArgument);
    }
    if params.task.len() > INTENT_TASK_BYTES {
        return Err(IntentServiceError::ResourceExhausted);
    }
    Ok(IntentInput {
        task: params.task,
        ttl: params.ttl,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn task_budget_and_unknown_versions_reject_whole_request() {
        let valid = json!({"format_version":INTENT_REQUEST_VERSION, "task":"review", "ttl":30});
        assert!(decode(serde_json::from_value(valid.clone()).unwrap()).is_ok());
        for version in ["asc.intent.request.v0", "asc.intent.request.v2"] {
            let mut unknown = valid.clone();
            unknown["format_version"] = json!(version);
            assert!(matches!(
                decode(serde_json::from_value(unknown).unwrap()),
                Err(IntentServiceError::InvalidArgument)
            ));
        }
        let mut oversized = valid.clone();
        oversized["task"] = json!("x".repeat(INTENT_TASK_BYTES + 1));
        assert!(matches!(
            decode(serde_json::from_value(oversized).unwrap()),
            Err(IntentServiceError::ResourceExhausted)
        ));
    }
}
