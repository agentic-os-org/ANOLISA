//! Handles versioning, protocol decoding, budgets and error mapping only; Cedar validation stays
//! on the core call path.

use asc_daemon_core::PeerCredentials;
use asc_daemon_core::intent::{
    CommandInput, CommandPolicyOutcome, IntentInput, IntentService, IntentServiceError,
};
use asc_daemon_protocol::intent::{
    INTENT_COMMAND_REQUEST_VERSION, INTENT_REQUEST_BYTES, INTENT_REQUEST_VERSION,
    INTENT_TASK_BYTES, IntentCommandParams, IntentCommandResult, IntentProposeParams,
    IntentProposeResult,
};
use asc_daemon_protocol::{DaemonRequest, DaemonResponse, RequestId, error_code, method};
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
        if request.method == method::INTENT_PROPOSE_COMMAND {
            let params: IntentCommandParams = serde_json::from_value(request.params)
                .map_err(|_| IntentServiceError::InvalidArgument)?;
            if params.format_version != INTENT_COMMAND_REQUEST_VERSION {
                return Err(IntentServiceError::InvalidArgument);
            }
            let input = CommandInput {
                command: params.command,
                cwd: params.cwd,
            };
            let outcome = service
                .ok_or(IntentServiceError::Unavailable)?
                .propose_command(peer, &input, control.deadline(), || control.is_cancelled())?;
            let response = match outcome {
                CommandPolicyOutcome::Candidate { policy, caveats } => {
                    IntentCommandResult::Candidate { policy, caveats }
                }
                CommandPolicyOutcome::NeedsClarification { reasons } => {
                    IntentCommandResult::NeedsClarification { reasons }
                }
            };
            return serde_json::to_value(response).map_err(|_| IntentServiceError::Internal);
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
    use asc_model_client::provider::{ModelProvider, ProviderError};
    use serde_json::{Value, json};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};

    const COMMAND: &str = "read -r line < report.csv";
    const POLICY: &str = "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when { resource.path == \"/work/report.csv\" };";

    struct Reply {
        content: Mutex<String>,
        calls: AtomicUsize,
        late: AtomicBool,
    }

    impl Reply {
        fn new() -> Self {
            Self {
                content: Mutex::new(json!({"policy": POLICY, "unresolved": []}).to_string()),
                calls: AtomicUsize::new(0),
                late: AtomicBool::new(false),
            }
        }

        fn set(&self, value: &Value) {
            *self.content.lock().unwrap() = value.to_string();
        }
    }

    impl ModelProvider for Reply {
        fn complete(
            &self,
            model: &str,
            system: &str,
            input: &str,
            deadline: Instant,
        ) -> Result<String, ProviderError> {
            assert_eq!(model, "fixture");
            assert!(system.contains(asc_intent::CEDAR_SCHEMA));
            // A retry carries the previous local rejection on a second line.
            let (payload, _) = input.split_once('\n').unwrap_or((input, ""));
            assert_eq!(
                serde_json::from_str::<Value>(payload).unwrap(),
                json!({"command": COMMAND, "cwd": "/work"})
            );
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.late.load(Ordering::Relaxed) {
                std::thread::sleep(
                    deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(1),
                );
            }
            Ok(self.content.lock().unwrap().clone())
        }
    }

    fn dispatcher(service: Arc<IntentService>) -> crate::DaemonDispatcher {
        use asc_action_runtime::{ActionRuntime, testing::discarding_finalizer};
        use asc_action_types::ActionId;
        use asc_capability_prompt_scan::{
            CachingScannerProvider, PromptScanAuditProjector, PromptScanExecutor, PromptScanWarmup,
        };
        let actions = asc_daemon_core::ActionService::new(
            ActionRuntime::new(
                ActionId::CodeScan,
                asc_capability_code_scan::CodeScanExecutor::default(),
                asc_capability_code_scan::CodeScanAuditProjector,
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
            Arc::new(asc_daemon_core::RootManagedPrincipalPolicy::with_admin_uids([0])),
            Arc::new(actions),
        )
        .with_intent(service)
    }

    fn command_params() -> Value {
        json!({"format_version": INTENT_COMMAND_REQUEST_VERSION, "command": COMMAND, "cwd": "/work"})
    }

    fn request(params: Value) -> DaemonRequest {
        DaemonRequest {
            method: method::INTENT_PROPOSE_COMMAND.into(),
            params,
            trace_context: None,
            compatibility: None,
        }
    }

    fn call(dispatcher: &crate::DaemonDispatcher, params: Value) -> Value {
        serde_json::to_value(dispatcher.handle(
            RequestId::new("command-fixture").unwrap(),
            PeerCredentials::new(1000, 1000, 1),
            request(params),
        ))
        .unwrap()
    }

    #[test]
    fn command_api_routes_hook_input_to_cedar_or_clarification() {
        use asc_daemon_service::RequestDispatcher;
        let provider = Arc::new(Reply::new());
        let service = Arc::new(IntentService::new(
            provider.clone(),
            "fixture".into(),
            || 2_000_000_000,
        ));
        let dispatcher = &dispatcher(service);
        let params = command_params();
        let response = call(dispatcher, params.clone());
        assert_eq!(
            response["result"],
            json!({"outcome": "candidate", "policy": POLICY, "caveats": []})
        );
        assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            method::metadata(method::INTENT_PROPOSE_COMMAND)
                .unwrap()
                .access,
            method::AccessPolicy::LocalUser
        );
        assert!(method::resolve("intent.propose_update").is_none());
        assert!(method::resolve("intent.propose_permissions").is_none());
        assert_eq!(
            dispatcher.dispatch_timeout(&serde_json::to_vec(&request(params.clone())).unwrap()),
            Some(Duration::from_secs(35))
        );

        provider.set(&json!({"policy": POLICY, "unresolved": ["需要确认运行依赖"]}));
        assert_eq!(
            call(dispatcher, params.clone())["result"],
            json!({"outcome": "needs_clarification", "reasons": ["需要确认运行依赖"]})
        );
        provider.set(&json!({"policy": "", "unresolved": []}));
        assert_eq!(
            call(dispatcher, params)["result"],
            json!({"outcome": "candidate", "policy": "", "caveats": []})
        );
        assert!(
            serde_json::from_value::<IntentCommandResult>(
                json!({"outcome":"needs_clarification", "reasons":["missing"], "policy": POLICY})
            )
            .is_err()
        );
    }

    #[test]
    fn command_api_rejects_invalid_inputs_outputs_and_expired_results() {
        let provider = Arc::new(Reply::new());
        let service = Arc::new(IntentService::new(
            provider.clone(),
            "fixture".into(),
            || 2_000_000_000,
        ));
        let dispatcher = &dispatcher(service.clone());
        assert_invalid_inputs(dispatcher, &provider);
        assert_invalid_outputs(dispatcher, &provider);
        assert_expired_results(dispatcher, &provider, &service);
    }

    fn assert_invalid_inputs(dispatcher: &crate::DaemonDispatcher, provider: &Reply) {
        let valid = command_params();
        for (key, value) in [
            ("format_version", json!("asc.intent.command.request.v2")),
            ("command", json!(" ")),
            ("command", json!("read\0x")),
            ("command", Value::Null),
            ("cwd", json!("relative")),
            ("cwd", json!("/work/../etc")),
            ("cwd", json!("/work//data")),
            ("cwd", json!("/work/")),
            ("cwd", json!("/work\n")),
            ("base_policy", json!({})),
            ("requested_permissions", json!({})),
            ("effective", json!({})),
            ("uid", json!(0)),
            ("ttl", json!(30)),
        ] {
            let mut params = valid.clone();
            params[key] = value;
            assert_eq!(
                call(dispatcher, params)["error"]["code"],
                json!(error_code::INVALID_ARGUMENT),
                "{key}"
            );
        }
        for key in ["command", "cwd", "format_version"] {
            let mut params = valid.clone();
            params.as_object_mut().unwrap().remove(key);
            assert_eq!(
                call(dispatcher, params)["error"]["code"],
                json!(error_code::INVALID_ARGUMENT)
            );
        }
        for (key, value) in [
            ("command", "x".repeat(asc_intent::MAX_COMMAND_BYTES + 1)),
            ("cwd", format!("/{}", "x".repeat(asc_intent::MAX_CWD_BYTES))),
        ] {
            let mut params = valid.clone();
            params[key] = json!(value);
            assert_eq!(
                call(dispatcher, params)["error"]["code"],
                json!(error_code::RESOURCE_EXHAUSTED)
            );
        }
        assert_eq!(provider.calls.load(Ordering::Relaxed), 0);
        assert!(serde_json::from_str::<IntentCommandParams>(r#"{"format_version":"asc.intent.command.request.v1","command":"a","command":"b","cwd":"/work"}"#).is_err());
        let mut wire = serde_json::to_vec(&request(valid.clone())).unwrap();
        wire.resize(INTENT_REQUEST_BYTES - 1, b' ');
        assert!(asc_daemon_protocol::request_fits_budget(&wire).unwrap());
        wire.push(b' ');
        assert!(!asc_daemon_protocol::request_fits_budget(&wire).unwrap());
    }

    fn assert_invalid_outputs(dispatcher: &crate::DaemonDispatcher, provider: &Reply) {
        let valid = command_params();
        for (reply, code) in [
            (json!({"policy": POLICY}), error_code::UNAVAILABLE),
            (
                json!({"policy": POLICY, "unresolved": [""]}),
                error_code::UNAVAILABLE,
            ),
            (
                json!({"policy": POLICY, "unresolved": ["x\n"]}),
                error_code::UNAVAILABLE,
            ),
            (
                json!({"policy": POLICY, "unresolved": [], "approved": true}),
                error_code::UNAVAILABLE,
            ),
            (
                json!({"policy": "x".repeat(asc_model_client::provider::MAX_CONTENT_BYTES), "unresolved": []}),
                error_code::UNAVAILABLE,
            ),
            (
                json!({"policy": "not cedar", "unresolved": []}),
                error_code::INTERNAL,
            ),
            (
                json!({"policy": POLICY.replace("file.read", "unknown.action"), "unresolved": []}),
                error_code::INTERNAL,
            ),
        ] {
            provider.set(&reply);
            let response = call(dispatcher, valid.clone());
            assert_eq!(response["error"]["code"], json!(code));
            assert!(response.get("result").is_none());
        }
        for policy in [
            POLICY.replace("/work/report.csv", "report.csv"),
            format!("@approved(\"true\") {POLICY}"),
        ] {
            provider.set(&json!({"policy": policy, "unresolved": []}));
            let result = call(dispatcher, valid.clone())["result"].clone();
            assert_eq!(result["outcome"], "needs_clarification");
            assert!(result.get("policy").is_none());
        }
    }

    fn assert_expired_results(
        dispatcher: &crate::DaemonDispatcher,
        provider: &Reply,
        service: &IntentService,
    ) {
        let valid = command_params();
        provider.set(&json!({"policy": POLICY, "unresolved": []}));
        let input = CommandInput {
            command: COMMAND.into(),
            cwd: "/work".into(),
        };
        let before = provider.calls.load(Ordering::Relaxed);
        assert!(matches!(
            service.propose_command(
                PeerCredentials::new(1000, 1000, 1),
                &input,
                Instant::now() + Duration::from_secs(5),
                || true
            ),
            Err(IntentServiceError::DeadlineExceeded)
        ));
        assert!(matches!(
            service.propose_command(
                PeerCredentials::new(1000, 1000, 1),
                &input,
                Instant::now(),
                || false
            ),
            Err(IntentServiceError::DeadlineExceeded)
        ));
        assert_eq!(provider.calls.load(Ordering::Relaxed), before);
        assert!(matches!(
            service.propose_command(
                PeerCredentials::new(1000, 1000, 1),
                &input,
                Instant::now() + Duration::from_secs(5),
                || provider.calls.load(Ordering::Relaxed) > before
            ),
            Err(IntentServiceError::DeadlineExceeded)
        ));
        provider.late.store(true, Ordering::Relaxed);
        let response = serde_json::to_value(dispatcher.handle_with_control(
            RequestId::new("late-fixture").unwrap(),
            PeerCredentials::new(1000, 1000, 1),
            &DispatchControl::new(Instant::now() + Duration::from_millis(20)),
            request(valid.clone()),
        ))
        .unwrap();
        assert_eq!(
            response["error"]["code"],
            json!(error_code::DEADLINE_EXCEEDED)
        );
        provider.late.store(false, Ordering::Relaxed);
        assert_eq!(call(dispatcher, valid)["result"]["outcome"], "candidate");
    }

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
