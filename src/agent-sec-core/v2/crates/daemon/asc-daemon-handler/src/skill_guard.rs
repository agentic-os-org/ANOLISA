//! Thin `SkillGuard` protocol adapter; peer credentials never come from business JSON.

use asc_action_runtime::{ActionRuntime, ExecutionControl, Finalizer};
use asc_action_types::{ActionAttribution, ActionId, CallerIdentity, Correlation};
use asc_capability_skill_guard::command::GuardCommand;
use asc_capability_skill_guard::executor::{
    SkillGuardAuditProjector, SkillGuardExecutor, SkillGuardRequest,
};
use asc_capability_skill_guard::{GuardError, SkillGuardService, SkillRoot};
use asc_daemon_core::PeerCredentials;
use asc_daemon_protocol::{DaemonResponse, RequestId, error_code};
use asc_daemon_service::DispatchControl;
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) struct SkillGuardHandler {
    service: Arc<SkillGuardService>,
    runtime: ActionRuntime<SkillGuardExecutor, SkillGuardAuditProjector>,
}

impl SkillGuardHandler {
    pub(super) fn new(service: Arc<SkillGuardService>, finalizer: Finalizer) -> Self {
        Self {
            service: service.clone(),
            runtime: ActionRuntime::new(
                ActionId::SkillGuard,
                SkillGuardExecutor::new(service),
                SkillGuardAuditProjector,
                finalizer,
            ),
        }
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
        let command: GuardCommand = match serde_json::from_value(params) {
            Ok(command) => command,
            Err(_) => {
                return DaemonResponse::error(
                    request_id,
                    error_code::INVALID_REQUEST,
                    "invalid SkillGuard command parameters",
                );
            }
        };
        let prepared = match &command {
            GuardCommand::RotateKeys {} => self.service.rotation_skills(control.deadline()),
            GuardCommand::Init {
                force_keys: true,
                baseline,
                ..
            } => self
                .service
                .rotation_skills(control.deadline())
                .and_then(|mut skills| {
                    if *baseline {
                        skills.extend(self.service.managed_skills()?);
                    }
                    Ok(skills)
                }),
            _ => self.service.managed_skills(),
        }
        .and_then(|managed| command.identities(&managed))
        .and_then(|identities| {
            identities
                .iter()
                .map(|identity| SkillRoot::direct(identity.path()))
                .collect::<Result<Vec<_>, GuardError>>()
        });
        let (roots, preparation_error) = match prepared {
            Ok(roots) => (roots, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let outcome = self.runtime.invoke(
            &ExecutionControl {
                deadline: control.deadline(),
                cancelled: control.is_cancelled(),
            },
            &ActionAttribution {
                caller: CallerIdentity {
                    uid: peer.uid(),
                    gid: peer.gid(),
                    pid: peer.pid(),
                },
                correlation: Correlation::default(),
            },
            &SkillGuardRequest {
                command,
                roots,
                caller_uid: peer.uid(),
                preparation_error,
            },
        );
        let result = json!({"success":outcome.success,"exitCode":outcome.exit_code,"error":outcome.error,"errorType":outcome.error_type,"data":outcome.data.get("output")});
        // A committed mutation is not rolled back because its detailed response is too large.
        // Preserve a parseable diagnosis and require show/export instead of silently truncating.
        if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > 3 * 1024 * 1024) {
            return DaemonResponse::success(
                request_id,
                json!({"success":false,"exitCode":1,"error":"SkillGuard result exceeds response limit; inspect show or export. The operation may have committed.","errorType":"ResponseTooLarge","data":{"status":"error","operationMayHaveCommitted":true}}),
            );
        }
        DaemonResponse::success(request_id, result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_action_runtime::SecurityEventSink;
    use asc_capability_skill_guard::{GuardConfig, scanner::ScannerRegistry};
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

    #[test]
    fn consumer_errors_and_kernel_identity_remain_distinct_from_protocol_errors() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let service = Arc::new(
            SkillGuardService::new(
                GuardConfig {
                    state_dir: state,
                    managed_skill_dirs: Vec::new(),
                },
                ScannerRegistry::default(),
            )
            .unwrap(),
        );
        let events = Arc::new(Events::default());
        let handler = SkillGuardHandler::new(service, Finalizer::new(events.clone()));
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
        let application = asc_pap::PapService::new(
            Arc::new(asc_pap_repository_memory::ProcessLocalPapRepository::default()),
            Arc::new(asc_policy_engine::PolicyTemplateCompiler),
        );
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let service = Arc::new(
            SkillGuardService::new(
                GuardConfig {
                    state_dir: directory.path().into(),
                    managed_skill_dirs: Vec::new(),
                },
                ScannerRegistry::default(),
            )
            .unwrap(),
        );
        let dispatcher = crate::DaemonDispatcher::new(
            application,
            Arc::new(asc_daemon_core::RootManagedPrincipalPolicy::default()),
        )
        .with_skill_guard(service, Finalizer::new(Arc::new(Events::default())));
        for payload in [
            br#"{"method":"action.code_scan","params":{"timeoutMs":120000}}"#.as_slice(),
            br#"{"method":"policy.templates.list","params":{}}"#.as_slice(),
            b"invalid",
        ] {
            assert_eq!(dispatcher.dispatch_timeout(payload), None);
        }
        assert_eq!(
            dispatcher.dispatch_timeout(
                br#"{"method":"action.skill_guard","params":{"command":"scan"}}"#
            ),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            dispatcher.dispatch_timeout(
                br#"{"method":"action.skill_guard","params":{"command":"status","timeoutMs":25}}"#
            ),
            Some(Duration::from_millis(25))
        );
    }
}
