//! Runtime authorization protocol projection; no target execution in the daemon.

use asc_daemon_core::{AuthorizationError, AuthorizationService, PeerCredentials};
use asc_daemon_protocol::{
    AuthorizationRequest, DaemonResponse, ExecutionReceipt, RequestId, error_code,
    method::AuthorizationMethod,
};
use asc_daemon_service::DispatchControl;
use serde_json::Value;

pub(crate) fn handle(
    id: RequestId,
    peer: PeerCredentials,
    control: &DispatchControl,
    service: Option<&AuthorizationService>,
    method: AuthorizationMethod,
    params: Value,
) -> DaemonResponse {
    if control.is_cancelled() {
        return DaemonResponse::error(
            id,
            error_code::DEADLINE_EXCEEDED,
            "request dispatch deadline expired",
        );
    }
    let Some(service) = service else {
        return DaemonResponse::error(
            id,
            error_code::UNAVAILABLE,
            "authorization service unavailable",
        );
    };
    let result = dispatch(service, peer, method, params);
    match result {
        Ok(value) => DaemonResponse::success(id, value),
        Err(error) => {
            let code = match error {
                AuthorizationError::InvalidArgument => error_code::INVALID_ARGUMENT,
                AuthorizationError::Forbidden => error_code::PERMISSION_DENIED,
                AuthorizationError::Conflict => error_code::CONFLICT,
                AuthorizationError::Unavailable => error_code::UNAVAILABLE,
                AuthorizationError::ResourceExhausted => error_code::RESOURCE_EXHAUSTED,
                AuthorizationError::Storage | AuthorizationError::Internal => error_code::INTERNAL,
            };
            DaemonResponse::error(id, code, &error.to_string())
        }
    }
}

fn dispatch(
    service: &AuthorizationService,
    peer: PeerCredentials,
    method: AuthorizationMethod,
    params: Value,
) -> Result<Value, AuthorizationError> {
    match method {
        AuthorizationMethod::Evaluate => {
            let request: AuthorizationRequest =
                serde_json::from_value(params).map_err(|_| AuthorizationError::InvalidArgument)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| AuthorizationError::Internal)?
                .as_secs();
            serde_json::to_value(service.evaluate(peer, &request, now)?)
                .map_err(|_| AuthorizationError::Internal)
        }
        AuthorizationMethod::Report => {
            let receipt: ExecutionReceipt =
                serde_json::from_value(params).map_err(|_| AuthorizationError::InvalidArgument)?;
            serde_json::to_value(service.report(peer, &receipt)?)
                .map_err(|_| AuthorizationError::Internal)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_daemon_core::{AuthorizationEvaluator, AuthorizationReceiptSink};
    use asc_daemon_protocol::Decision;
    use asc_security_events::SecurityEvent;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    struct Unavailable;
    impl AuthorizationEvaluator for Unavailable {
        fn evaluate(
            &self,
            _: &AuthorizationRequest,
            _: u64,
        ) -> Result<Decision, AuthorizationError> {
            Err(AuthorizationError::Unavailable)
        }
    }
    impl AuthorizationReceiptSink for Unavailable {
        fn record(&self, _: &SecurityEvent) -> Result<(), AuthorizationError> {
            Err(AuthorizationError::Storage)
        }
    }
    #[test]
    fn unconfigured_rpc_and_malformed_payload_fail_without_execution() {
        let control = DispatchControl::new(Instant::now() + Duration::from_secs(1));
        let peer = PeerCredentials::new(1000, 1000, 10);
        let response = handle(
            RequestId::new("call-1").unwrap(),
            peer,
            &control,
            None,
            AuthorizationMethod::Evaluate,
            serde_json::json!({}),
        );
        let DaemonResponse::Error(response) = response else {
            panic!("expected error")
        };
        assert_eq!(response.error.code.as_str(), error_code::UNAVAILABLE);
        let service = AuthorizationService::new(Arc::new(Unavailable), Arc::new(Unavailable));
        for method in [AuthorizationMethod::Evaluate, AuthorizationMethod::Report] {
            let response = handle(
                RequestId::new("call-2").unwrap(),
                peer,
                &control,
                Some(&service),
                method,
                serde_json::json!({"policy":"permit(principal,action,resource);"}),
            );
            let DaemonResponse::Error(response) = response else {
                panic!("expected error")
            };
            assert_eq!(response.error.code.as_str(), error_code::INVALID_ARGUMENT);
        }
    }
}
