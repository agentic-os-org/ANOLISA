//! Thin `SkillSec` protocol adapter; peer credentials never come from business JSON.

use asc_action_runtime::ExecutionControl;
use asc_action_types::SkillSecCommand;
use asc_daemon_core::{ActionService, PeerCredentials};
use asc_daemon_protocol::{DaemonResponse, RequestId, error_code};
use asc_daemon_service::DispatchControl;
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) struct SkillSecHandler {
    application: Arc<ActionService>,
}

impl SkillSecHandler {
    pub(super) fn new(application: Arc<ActionService>) -> Self {
        Self { application }
    }

    pub(super) fn handle(
        &self,
        request_id: RequestId,
        peer: PeerCredentials,
        control: &DispatchControl,
        mut params: Value,
    ) -> DaemonResponse {
        if let Some(value) = params
            .as_object_mut()
            .and_then(|map| map.remove("timeoutMs"))
            && !value.as_u64().is_some_and(|ms| (1..=120_000).contains(&ms))
        {
            return DaemonResponse::error(
                request_id,
                error_code::INVALID_ARGUMENT,
                "timeoutMs must be between 1 and 120000",
            );
        }
        let command: SkillSecCommand = match serde_json::from_value(params) {
            Ok(command) => command,
            Err(_) => {
                return DaemonResponse::error(
                    request_id,
                    error_code::INVALID_REQUEST,
                    "invalid SkillSec command parameters",
                );
            }
        };
        let Ok(outcome) = self.application.skill_sec(
            peer,
            &ExecutionControl {
                deadline: control.deadline(),
                cancelled: control.is_cancelled(),
            },
            command,
        ) else {
            return DaemonResponse::error(
                request_id,
                error_code::INTERNAL,
                "capability execution failed",
            );
        };
        let result = json!({"success":outcome.success,"exitCode":outcome.exit_code,"error":outcome.error,"errorType":outcome.error_type,"data":outcome.data.get("output")});
        // A committed mutation is not rolled back because its detailed response is too large.
        // Preserve a parseable diagnosis and require show/export instead of silently truncating.
        if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > 3 * 1024 * 1024) {
            return DaemonResponse::success(
                request_id,
                json!({"success":false,"exitCode":1,"error":"SkillSec result exceeds response limit; inspect show or export. The operation may have committed.","errorType":"ResponseTooLarge","data":{"status":"error","operationMayHaveCommitted":true}}),
            );
        }
        DaemonResponse::success(request_id, result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_action_runtime::{
        ActionRuntime, Finalizer, SecurityEventSink, testing::audit_finalizer,
    };
    use asc_action_types::ActionId;
    use asc_capability_code_scan::{CodeScanAuditProjector, CodeScanExecutor};
    use asc_capability_skill_sec::SkillSecService;
    use asc_capability_skill_sec::executor::{SkillSecAuditProjector, SkillSecExecutor};
    use asc_capability_skill_sec::{SkillSecConfig, scanner::ScannerRegistry};
    use asc_security_events::SecurityEvent;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Events(Mutex<Vec<SecurityEvent>>);
    impl SecurityEventSink for Events {
        fn write(&self, event: &SecurityEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    fn application(service: Arc<SkillSecService>, finalizer: Finalizer) -> Arc<ActionService> {
        Arc::new(
            ActionService::new(ActionRuntime::new(
                ActionId::CodeScan,
                CodeScanExecutor,
                CodeScanAuditProjector,
                finalizer.clone(),
            ))
            .with_skill_sec(ActionRuntime::new(
                ActionId::SkillSec,
                SkillSecExecutor::new(service),
                SkillSecAuditProjector,
                finalizer,
            )),
        )
    }

    #[test]
    fn consumer_errors_and_kernel_identity_remain_distinct_from_protocol_errors() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let service = Arc::new(
            SkillSecService::new(
                SkillSecConfig {
                    state_dir: state,
                    managed_skill_dirs: Vec::new(),
                },
                ScannerRegistry::default(),
            )
            .unwrap(),
        );
        let events = Arc::new(Events::default());
        let handler = SkillSecHandler::new(application(service, audit_finalizer(events.clone())));
        let id = || RequestId::new("fixture").unwrap();
        let peer = PeerCredentials::new(1001, 1002, 1003);
        let control = DispatchControl::new(Instant::now() + Duration::from_secs(10));
        let status = handler.handle(id(), peer, &control, json!({"command":"status"}));
        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["result"]["data"]["keys"]["initialized"], false);
        let rotation = serde_json::to_value(handler.handle(
            id(),
            peer,
            &control,
            json!({"command":"rotate-keys"}),
        ))
        .unwrap();
        assert_eq!(rotation["result"]["errorType"], "PermissionDenied");
        let invalid = serde_json::to_value(handler.handle(
            id(),
            peer,
            &control,
            json!({"command":"rotate-keys","uid":0}),
        ))
        .unwrap();
        assert!(invalid.get("error").is_some());
        let timeout = serde_json::to_value(handler.handle(
            id(),
            peer,
            &DispatchControl::new(Instant::now()),
            json!({"command":"init","baseline":false}),
        ))
        .unwrap();
        assert_eq!(timeout["result"]["errorType"], "TimeoutError");
        let events = events.0.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|e| e.uid == 1001 && e.pid == 1003));
    }

    #[test]
    fn skill_budget_does_not_change_ordinary_methods_or_embedding_defaults() {
        use asc_daemon_service::RequestDispatcher as _;
        let pap = asc_pap::PapService::new(
            Arc::new(asc_pap_repository_memory::ProcessLocalPapRepository::default()),
            Arc::new(asc_policy_engine::PolicyTemplateCompiler),
        );
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let service = Arc::new(
            SkillSecService::new(
                SkillSecConfig {
                    state_dir: directory.path().into(),
                    managed_skill_dirs: Vec::new(),
                },
                ScannerRegistry::default(),
            )
            .unwrap(),
        );
        let dispatcher = crate::DaemonDispatcher::new(
            pap,
            Arc::new(asc_daemon_core::RootManagedPrincipalPolicy::default()),
            application(service, audit_finalizer(Arc::new(Events::default()))),
        );
        for payload in [
            br#"{"method":"action.code_scan","params":{"timeoutMs":120000}}"#.as_slice(),
            br#"{"method":"policy.templates.list","params":{}}"#.as_slice(),
            b"invalid",
        ] {
            assert_eq!(dispatcher.dispatch_timeout(payload), None);
        }
        assert_eq!(
            dispatcher
                .dispatch_timeout(br#"{"method":"action.skill_sec","params":{"command":"scan"}}"#),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            dispatcher.dispatch_timeout(
                br#"{"method":"action.skill_sec","params":{"command":"status","timeoutMs":25}}"#
            ),
            Some(Duration::from_millis(25))
        );
    }
    #[test]
    fn unexpected_preparation_failure_returns_a_controlled_error_after_one_finalization() {
        use asc_capability_skill_sec::executor::SkillEnvironment;
        struct PanickingEnvironment;
        impl SkillEnvironment for PanickingEnvironment {
            fn resolve(
                &self,
                _: &asc_action_types::SkillIdentity,
                _: Instant,
            ) -> Result<asc_capability_skill_sec::SkillRoot, asc_capability_skill_sec::SkillSecError>
            {
                panic!("PRIVATE_RESOLVER_FAILURE")
            }
        }
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let service = Arc::new(
            SkillSecService::new(
                SkillSecConfig {
                    state_dir: directory.path().into(),
                    managed_skill_dirs: Vec::new(),
                },
                ScannerRegistry::default(),
            )
            .unwrap(),
        );
        let events = Arc::new(Events::default());
        let finalizer = audit_finalizer(events.clone());
        let application = Arc::new(
            ActionService::new(ActionRuntime::new(
                ActionId::CodeScan,
                CodeScanExecutor,
                CodeScanAuditProjector,
                finalizer.clone(),
            ))
            .with_skill_sec(ActionRuntime::new(
                ActionId::SkillSec,
                SkillSecExecutor::new(service).with_environment(Arc::new(PanickingEnvironment)),
                SkillSecAuditProjector,
                finalizer,
            )),
        );
        let response = SkillSecHandler::new(application).handle(
            RequestId::new("panic").unwrap(),
            PeerCredentials::new(1001, 1002, 1003),
            &DispatchControl::new(Instant::now() + Duration::from_secs(10)),
            json!({"command":"analyze", "skillDir":"/PRIVATE_SKILL_PATH"}),
        );
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["error"]["code"], error_code::INTERNAL);
        assert!(!value.to_string().contains("PRIVATE_"));
        let events = events.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].details["error_type"], "InternalExecutionError");
        assert_eq!(events[0].uid, 1001);
        assert!(
            !serde_json::to_string(&*events)
                .unwrap()
                .contains("PRIVATE_")
        );
    }
}
