//! Secret-gateway protocol adapter.

use std::path::PathBuf;
use std::sync::Arc;

use asc_daemon_core::{
    GatewayAdministration, GatewayFailure, GatewayFailureKind, GatewayKeyRequest, PeerCredentials,
};
use asc_daemon_protocol::method::GatewayMethod;
use asc_daemon_protocol::{
    DaemonResponse, GatewayAgentParams, GatewayKeyAddParams, GatewayKeyRemoveParams,
    GatewayRegisterParams, RequestId, error_code,
};
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
        // Policy delegation never grants gateway configuration authority. Check
        // kernel identity before decoding or reaching the privileged application.
        if peer.uid() != 0 && !matches!(method, GatewayMethod::Status | GatewayMethod::Credentials)
        {
            return DaemonResponse::error(
                request_id,
                error_code::PERMISSION_DENIED,
                "only root may administer the secret gateway",
            );
        }
        let Some(application) = &self.application else {
            return DaemonResponse::error(
                request_id,
                error_code::UNAVAILABLE,
                "the secret gateway is not configured in this daemon",
            );
        };
        let result = match method {
            GatewayMethod::Enable => no_params(params).and_then(|()| application.enable()),
            GatewayMethod::Disable => no_params(params).and_then(|()| application.disable()),
            GatewayMethod::Status => no_params(params).and_then(|()| application.status(peer)),
            GatewayMethod::KeyList => no_params(params).and_then(|()| application.list_keys()),
            GatewayMethod::KeyAdd => decode::<GatewayKeyAddParams>(params).and_then(|params| {
                application.add_key(
                    peer,
                    GatewayKeyRequest {
                        agent: params.agent,
                        preset: params.preset,
                        secret_file: PathBuf::from(params.secret_file),
                    },
                )
            }),
            GatewayMethod::KeyRemove => decode::<GatewayKeyRemoveParams>(params)
                .and_then(|params| application.remove_key(&params.agent, &params.preset)),
            GatewayMethod::Register => decode::<GatewayRegisterParams>(params)
                .and_then(|params| application.register(params.uid, params.pid, &params.agent)),
            GatewayMethod::Unregister => decode::<GatewayAgentParams>(params)
                .and_then(|params| application.unregister(&params.agent)),
            GatewayMethod::Credentials => decode::<GatewayAgentParams>(params)
                .and_then(|params| application.credentials(peer, &params.agent)),
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
        message: "invalid secret gateway parameters".into(),
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
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl Recorder {
        #[allow(clippy::unnecessary_wraps)] // Mirrors the port's fallible signature.
        fn record(&self, call: String) -> Result<Value, GatewayFailure> {
            self.0.lock().unwrap().push(call);
            Ok(serde_json::json!({"ok": true}))
        }
    }

    impl GatewayAdministration for Recorder {
        fn enable(&self) -> Result<Value, GatewayFailure> {
            self.record("enable".into())
        }
        fn disable(&self) -> Result<Value, GatewayFailure> {
            Err(GatewayFailure {
                kind: GatewayFailureKind::Unavailable,
                message: "unit missing".into(),
            })
        }
        fn status(&self, peer: PeerCredentials) -> Result<Value, GatewayFailure> {
            self.record(format!(
                "status by {} gid {} pid {}",
                peer.uid(),
                peer.gid(),
                peer.pid()
            ))
        }
        fn add_key(
            &self,
            peer: PeerCredentials,
            request: GatewayKeyRequest,
        ) -> Result<Value, GatewayFailure> {
            self.record(format!(
                "add {} {} {} by {}",
                request.agent,
                request.preset,
                request.secret_file.display(),
                peer.uid()
            ))
        }
        fn remove_key(&self, agent: &str, preset: &str) -> Result<Value, GatewayFailure> {
            self.record(format!("remove {agent} {preset}"))
        }
        fn list_keys(&self) -> Result<Value, GatewayFailure> {
            self.record("list".into())
        }
        fn register(&self, uid: u32, pid: u32, agent: &str) -> Result<Value, GatewayFailure> {
            self.record(format!("register {agent} uid {uid} pid {pid}"))
        }
        fn unregister(&self, agent: &str) -> Result<Value, GatewayFailure> {
            self.record(format!("unregister {agent}"))
        }
        fn credentials(&self, peer: PeerCredentials, agent: &str) -> Result<Value, GatewayFailure> {
            self.record(format!(
                "credentials {agent} by {} gid {} pid {}",
                peer.uid(),
                peer.gid(),
                peer.pid()
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
    fn requests_are_decoded_and_bound_to_the_peer() {
        let recorder = Arc::new(Recorder::default());
        let handler = GatewayHandler::new(recorder.clone());
        let peer = PeerCredentials::new(0, 0, 42);
        let calls = [
            (GatewayMethod::Enable, Value::Null),
            (GatewayMethod::Status, serde_json::json!({})),
            (
                GatewayMethod::KeyAdd,
                serde_json::json!({"agent": "dev", "preset": "github", "secretFile": "/root/t"}),
            ),
            (
                GatewayMethod::KeyRemove,
                serde_json::json!({"agent": "dev", "preset": "github"}),
            ),
            (GatewayMethod::KeyList, Value::Null),
            (
                GatewayMethod::Register,
                serde_json::json!({"agent": "dev", "uid": 1001, "pid": 314}),
            ),
            (
                GatewayMethod::Unregister,
                serde_json::json!({"agent": "dev"}),
            ),
            (
                GatewayMethod::Credentials,
                serde_json::json!({"agent": "dev"}),
            ),
        ];
        for (method, params) in calls {
            assert!(matches!(
                handler.handle(request_id(), peer, method, params),
                DaemonResponse::Success(_)
            ));
        }
        assert_eq!(
            *recorder.0.lock().unwrap(),
            [
                "enable",
                "status by 0 gid 0 pid 42",
                "add dev github /root/t by 0",
                "remove dev github",
                "list",
                "register dev uid 1001 pid 314",
                "unregister dev",
                "credentials dev by 0 gid 0 pid 42",
            ]
        );
        let failed = handler.handle(request_id(), peer, GatewayMethod::Disable, Value::Null);
        assert_eq!(error_of(failed), error_code::UNAVAILABLE);
        let rejected = handler.handle(
            request_id(),
            peer,
            GatewayMethod::Register,
            serde_json::json!({"agent": "dev", "pid": 1}),
        );
        assert_eq!(error_of(rejected), error_code::INVALID_ARGUMENT);
        let rejected = handler.handle(
            request_id(),
            peer,
            GatewayMethod::Enable,
            serde_json::json!({"force": true}),
        );
        assert_eq!(error_of(rejected), error_code::INVALID_ARGUMENT);
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

    fn dispatch(
        dispatcher: &crate::DaemonDispatcher,
        peer: PeerCredentials,
        method: &str,
        params: &Value,
    ) -> DaemonResponse {
        dispatcher.handle(
            request_id(),
            peer,
            serde_json::from_value(serde_json::json!({"method": method, "params": params.clone()}))
                .unwrap(),
        )
    }

    #[test]
    fn configured_policy_administrators_cannot_apply_gateway_configuration() {
        let recorder = Arc::new(Recorder::default());
        let dispatcher = dispatcher(recorder.clone());
        let administrator = PeerCredentials::new(1000, 0, 42);
        for (method, params) in [
            ("gateway.enable", serde_json::json!({})),
            ("gateway.disable", serde_json::json!({})),
            ("gateway.key.list", serde_json::json!({})),
            (
                "gateway.key.add",
                serde_json::json!({"agent": "dev", "preset": "github", "secretFile": "/root/t"}),
            ),
            (
                "gateway.key.remove",
                serde_json::json!({"agent": "dev", "preset": "github"}),
            ),
            (
                "gateway.register",
                serde_json::json!({"agent": "dev", "uid": 1001, "pid": 314}),
            ),
            ("gateway.unregister", serde_json::json!({"agent": "dev"})),
        ] {
            assert_eq!(
                error_of(dispatch(&dispatcher, administrator, method, &params)),
                error_code::PERMISSION_DENIED,
                "{method} must require authenticated UID 0",
            );
        }
        assert!(recorder.0.lock().unwrap().is_empty());
        // Gateway root enforcement must not revoke configured PAP administration.
        assert!(matches!(
            dispatch(
                &dispatcher,
                administrator,
                "policy.templates.list",
                &serde_json::json!({})
            ),
            DaemonResponse::Success(_)
        ));
    }

    #[test]
    fn dispatcher_keeps_target_registration_and_query_peer_identity_separate() {
        let recorder = Arc::new(Recorder::default());
        let dispatcher = dispatcher(recorder.clone());
        let root = PeerCredentials::new(0, 0, 42);
        assert!(matches!(
            dispatch(
                &dispatcher,
                root,
                "gateway.register",
                &serde_json::json!({"agent": "dev", "uid": 1001, "pid": 314})
            ),
            DaemonResponse::Success(_)
        ));
        for peer in [root, PeerCredentials::new(1001, 1002, 1003)] {
            for (method, params) in [
                ("gateway.status", serde_json::json!({})),
                ("gateway.credentials", serde_json::json!({"agent": "dev"})),
            ] {
                assert!(matches!(
                    dispatch(&dispatcher, peer, method, &params),
                    DaemonResponse::Success(_)
                ));
            }
        }
        assert_eq!(
            *recorder.0.lock().unwrap(),
            [
                "register dev uid 1001 pid 314",
                "status by 0 gid 0 pid 42",
                "credentials dev by 0 gid 0 pid 42",
                "status by 1001 gid 1002 pid 1003",
                "credentials dev by 1001 gid 1002 pid 1003",
            ]
        );
        for peer in [root, PeerCredentials::new(1001, 1002, 1003)] {
            assert_eq!(
                error_of(dispatch(
                    &dispatcher,
                    peer,
                    "gateway.run",
                    &serde_json::json!({"agent": "dev"})
                )),
                error_code::UNKNOWN_METHOD,
            );
        }
        for params in [
            serde_json::json!({"agent": "dev", "pid": 314}),
            serde_json::json!({"agent": "dev", "uid": 1001}),
            serde_json::json!({"agent": "dev", "uid": "1001", "pid": 314}),
            serde_json::json!({"agent": "dev", "uid": 1001, "pid": -1}),
            serde_json::json!({"agent": "dev", "uid": 1001, "pid": 314, "callerUid": 0}),
        ] {
            assert_eq!(
                error_of(dispatch(&dispatcher, root, "gateway.register", &params)),
                error_code::INVALID_ARGUMENT,
            );
        }
        for (method, params) in [
            ("gateway.status", serde_json::json!({"uid": 0})),
            (
                "gateway.credentials",
                serde_json::json!({"agent": "dev", "uid": 0}),
            ),
        ] {
            assert_eq!(
                error_of(dispatch(
                    &dispatcher,
                    PeerCredentials::new(1001, 1002, 1003),
                    method,
                    &params
                )),
                error_code::INVALID_ARGUMENT,
            );
        }
        assert_eq!(recorder.0.lock().unwrap().len(), 5);
    }

    #[test]
    fn an_unconfigured_gateway_is_unavailable() {
        let response = GatewayHandler::default().handle(
            request_id(),
            PeerCredentials::new(0, 0, 1),
            GatewayMethod::Status,
            Value::Null,
        );
        assert_eq!(error_of(response), error_code::UNAVAILABLE);
    }
}
