//! Root pair administration and authenticated public CA protocol adapter.

use std::sync::Arc;

use asc_daemon_core::{GatewayAdministration, GatewayFailure, GatewayFailureKind, PeerCredentials};
use asc_daemon_protocol::method::GatewayMethod;
use asc_daemon_protocol::{DaemonResponse, GatewayKeyRemoveParams, RequestId, error_code};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Default)]
pub(super) struct GatewayHandler {
    application: Option<Arc<dyn GatewayAdministration>>,
}

impl GatewayHandler {
    pub(super) fn new(application: Arc<dyn GatewayAdministration>) -> Self {
        Self {
            application: Some(application),
        }
    }

    pub(super) fn handle(
        &self,
        request_id: RequestId,
        peer: PeerCredentials,
        method: GatewayMethod,
        params: Value,
    ) -> DaemonResponse {
        if peer.uid() != 0 && !matches!(method, GatewayMethod::Status | GatewayMethod::Ca) {
            return DaemonResponse::error(
                request_id,
                error_code::PERMISSION_DENIED,
                "only root may administer the credential gateway",
            );
        }
        let Some(application) = &self.application else {
            return DaemonResponse::error(
                request_id,
                error_code::UNAVAILABLE,
                "the credential gateway is not configured in this daemon",
            );
        };
        let result = match method {
            GatewayMethod::Enable => no_params(params).and_then(|()| application.enable()),
            GatewayMethod::Disable => no_params(params).and_then(|()| application.disable()),
            GatewayMethod::Status => no_params(params).and_then(|()| application.status(peer)),
            GatewayMethod::KeyList => no_params(params).and_then(|()| application.list_keys()),
            GatewayMethod::KeyRemove => decode::<GatewayKeyRemoveParams>(params)
                .and_then(|params| application.remove_key(&params.pairid)),
            GatewayMethod::Ca => no_params(params).and_then(|()| application.ca(peer)),
        };
        match result {
            Ok(value) => DaemonResponse::success(request_id, value),
            Err(failure) => DaemonResponse::error(request_id, code(failure.kind), &failure.message),
        }
    }
}

fn no_params(params: Value) -> Result<(), GatewayFailure> {
    match params {
        Value::Null => Ok(()),
        Value::Object(map) if map.is_empty() => Ok(()),
        _ => Err(invalid_params()),
    }
}

fn decode<T: DeserializeOwned>(params: Value) -> Result<T, GatewayFailure> {
    serde_json::from_value(params).map_err(|_| invalid_params())
}

fn invalid_params() -> GatewayFailure {
    GatewayFailure {
        kind: GatewayFailureKind::InvalidArgument,
        message: "invalid credential gateway parameters".into(),
    }
}

fn code(kind: GatewayFailureKind) -> &'static str {
    match kind {
        GatewayFailureKind::InvalidArgument => error_code::INVALID_ARGUMENT,
        GatewayFailureKind::NotFound => error_code::NOT_FOUND,
        GatewayFailureKind::Conflict => error_code::CONFLICT,
        GatewayFailureKind::PermissionDenied => error_code::PERMISSION_DENIED,
        GatewayFailureKind::ResourceExhausted => error_code::RESOURCE_EXHAUSTED,
        GatewayFailureKind::Unavailable => error_code::UNAVAILABLE,
        GatewayFailureKind::Internal => error_code::INTERNAL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl Recorder {
        fn record(&self, call: String, result: Value) -> Value {
            self.0.lock().unwrap().push(call);
            result
        }
    }

    impl GatewayAdministration for Recorder {
        fn enable(&self) -> Result<Value, GatewayFailure> {
            Ok(self.record("enable".into(), serde_json::json!({})))
        }
        fn disable(&self) -> Result<Value, GatewayFailure> {
            Err(GatewayFailure {
                kind: GatewayFailureKind::Unavailable,
                message: "unit missing".into(),
            })
        }
        fn status(&self, peer: PeerCredentials) -> Result<Value, GatewayFailure> {
            Ok(self.record(
                format!("status by {}", peer.uid()),
                serde_json::json!({"enabled":true,"ready":true,"scopes":[{"scopeId":"scope-1","bindings":[]}]}),
            ))
        }
        fn remove_key(&self, pairid: &str) -> Result<Value, GatewayFailure> {
            Ok(self.record(
                format!("remove {pairid}"),
                serde_json::json!({"pairid":pairid}),
            ))
        }
        fn list_keys(&self) -> Result<Value, GatewayFailure> {
            Ok(self.record("list".into(), serde_json::json!({"pairids":["pair-1"]})))
        }
        fn ca(&self, peer: PeerCredentials) -> Result<Value, GatewayFailure> {
            Ok(self.record(
                format!("ca by {} gid {} pid {}", peer.uid(), peer.gid(), peer.pid()),
                serde_json::json!({"caBundlePath":"/run/agent-sec-gateway/ca-bundle.pem"}),
            ))
        }
    }

    fn request_id() -> RequestId {
        RequestId::new("test").unwrap()
    }
    fn error_of(response: DaemonResponse) -> String {
        match response {
            DaemonResponse::Error(response) => response.error.code.as_str().to_owned(),
            DaemonResponse::Success(_) => panic!("expected an error"),
        }
    }

    #[test]
    fn pair_requests_and_public_ca_queries_are_bound_to_the_peer() {
        let recorder = Arc::new(Recorder::default());
        let handler = GatewayHandler::new(recorder.clone());
        let root = PeerCredentials::new(0, 0, 42);
        for (method, params) in [
            (GatewayMethod::Enable, Value::Null),
            (
                GatewayMethod::KeyRemove,
                serde_json::json!({"pairid":"pair-1"}),
            ),
            (GatewayMethod::KeyList, Value::Null),
        ] {
            assert!(matches!(
                handler.handle(request_id(), root, method, params),
                DaemonResponse::Success(_)
            ));
        }
        let owner = PeerCredentials::new(1001, 1002, 1003);
        for params in [Value::Null, serde_json::json!({})] {
            assert_eq!(
                handler.handle(request_id(), owner, GatewayMethod::Ca, params),
                DaemonResponse::success(
                    request_id(),
                    serde_json::json!({"caBundlePath":"/run/agent-sec-gateway/ca-bundle.pem"})
                )
            );
        }
        assert_eq!(
            *recorder.0.lock().unwrap(),
            [
                "enable",
                "remove pair-1",
                "list",
                "ca by 1001 gid 1002 pid 1003",
                "ca by 1001 gid 1002 pid 1003",
            ]
        );
        assert_eq!(
            error_of(handler.handle(request_id(), root, GatewayMethod::Disable, Value::Null)),
            error_code::UNAVAILABLE
        );
        for (method, params) in [
            (GatewayMethod::Ca, serde_json::json!({"scopeId":"scope-1"})),
            (GatewayMethod::Ca, serde_json::json!({"uid":0})),
            (GatewayMethod::Ca, serde_json::json!({"gid":0})),
            (GatewayMethod::Ca, serde_json::json!({"pid":1})),
            (GatewayMethod::Ca, serde_json::json!({"secret":"dummy"})),
            (GatewayMethod::Ca, serde_json::json!([])),
            (
                GatewayMethod::KeyRemove,
                serde_json::json!({"pairid":"pair-1","secret":"dummy"}),
            ),
            (GatewayMethod::Enable, serde_json::json!({"force":true})),
        ] {
            assert_eq!(
                error_of(handler.handle(request_id(), root, method, params)),
                error_code::INVALID_ARGUMENT
            );
        }
        assert_eq!(recorder.0.lock().unwrap().len(), 5);
    }

    fn dispatcher(recorder: Arc<Recorder>) -> crate::DaemonDispatcher {
        use asc_action_runtime::{ActionRuntime, testing::discarding_finalizer};
        use asc_action_types::ActionId;
        use asc_capability_code_scan::{CodeScanAuditProjector, CodeScanExecutor};
        use asc_capability_prompt_scan::{
            CachingScannerProvider, PromptScanAuditProjector, PromptScanExecutor, PromptScanWarmup,
        };
        let actions = asc_daemon_core::ActionService::new(
            ActionRuntime::new(
                ActionId::CodeScan,
                CodeScanExecutor::default(),
                CodeScanAuditProjector,
                discarding_finalizer(),
            ),
            ActionRuntime::new(
                ActionId::PiiScan,
                asc_capability_pii_scan::PiiScanExecutor::new(Arc::new(
                    asc_capability_pii_scan::PiiRuleSet::builtin().unwrap(),
                )),
                asc_capability_pii_scan::PiiAuditProjector,
                discarding_finalizer(),
            ),
            ActionRuntime::new(
                ActionId::PromptScan,
                PromptScanExecutor::default(),
                PromptScanAuditProjector,
                discarding_finalizer(),
            ),
            PromptScanWarmup::new(Arc::new(CachingScannerProvider::default())),
        );
        crate::DaemonDispatcher::new(
            asc_pap::PapService::new(Arc::new(
                asc_policy_repository_sqlite::SqlitePolicyRepository::temporary().unwrap(),
            )),
            Arc::new(asc_daemon_core::RootManagedPrincipalPolicy::with_admin_uids([1000])),
            Arc::new(actions),
        )
        .with_gateway(recorder)
    }

    #[test]
    fn delegated_file_policy_administration_does_not_grant_credential_authority() {
        let recorder = Arc::new(Recorder::default());
        let dispatcher = dispatcher(recorder.clone());
        let administrator = PeerCredentials::new(1000, 0, 42);
        for (method, params) in [
            ("gateway.enable", serde_json::json!({})),
            ("gateway.disable", serde_json::json!({})),
            ("gateway.key.list", serde_json::json!({})),
            ("gateway.key.remove", serde_json::json!({"pairid":"pair-1"})),
        ] {
            for peer in [administrator, PeerCredentials::new(1001, 1001, 43)] {
                let response = dispatcher.handle(
                    request_id(),
                    peer,
                    serde_json::from_value(serde_json::json!({"method":method,"params":params}))
                        .unwrap(),
                );
                assert_eq!(error_of(response), error_code::PERMISSION_DENIED);
            }
        }
        assert!(recorder.0.lock().unwrap().is_empty());
        assert!(matches!(
            dispatcher.handle(
                request_id(),
                administrator,
                serde_json::from_value(
                    serde_json::json!({"method":"policy.templates.list","params":{}})
                )
                .unwrap()
            ),
            DaemonResponse::Success(_)
        ));
    }

    #[test]
    fn public_ca_dispatch_accepts_local_peers_but_rejects_retired_routes_and_scope_inputs() {
        let recorder = Arc::new(Recorder::default());
        let dispatcher = dispatcher(recorder.clone());
        let peer = PeerCredentials::new(1001, 1002, 1003);
        assert_eq!(
            dispatcher.handle(
                request_id(),
                peer,
                serde_json::from_value(serde_json::json!({"method":"gateway.ca","params":{}}))
                    .unwrap(),
            ),
            DaemonResponse::success(
                request_id(),
                serde_json::json!({"caBundlePath":"/run/agent-sec-gateway/ca-bundle.pem"})
            )
        );
        for (method, params, error) in [
            (
                "gateway.credentials",
                serde_json::json!({"scopeId":"scope-1"}),
                error_code::UNKNOWN_METHOD,
            ),
            (
                "gateway.key.add",
                serde_json::json!({"secretFile":"/root/t"}),
                error_code::UNKNOWN_METHOD,
            ),
            (
                "gateway.ca",
                serde_json::json!({"scopeId":"scope-1"}),
                error_code::INVALID_ARGUMENT,
            ),
            (
                "gateway.ca",
                serde_json::json!({"uid":0}),
                error_code::INVALID_ARGUMENT,
            ),
        ] {
            assert_eq!(
                error_of(
                    dispatcher.handle(
                        request_id(),
                        peer,
                        serde_json::from_value(
                            serde_json::json!({"method":method,"params":params})
                        )
                        .unwrap(),
                    )
                ),
                error
            );
        }
        assert_eq!(
            *recorder.0.lock().unwrap(),
            ["ca by 1001 gid 1002 pid 1003"]
        );
    }

    #[test]
    fn an_unconfigured_gateway_is_unavailable() {
        for method in [GatewayMethod::Status, GatewayMethod::Ca] {
            let response = GatewayHandler::default().handle(
                request_id(),
                PeerCredentials::new(1001, 1001, 1),
                method,
                Value::Null,
            );
            assert_eq!(error_of(response), error_code::UNAVAILABLE);
        }
    }
}
