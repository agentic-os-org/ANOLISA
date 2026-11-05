use super::*;
use crate::daemon::actor_ref_for_uid;
use crate::runtime::{
    AgentRuntimePortFactory, CoshCoreBridge, CoshCoreBridgeConfig, CoshCoreBridgeIdentity,
    CoshCoreBrokeredContext, RuntimeLaunchSpec, ScheduledAgentRuntimeFactory, ScheduledRuntimePort,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct MandatoryCheckpointCore {
    installation: InstallationId,
    workspace: PathBuf,
    mandatory: bool,
}

impl AgentRuntimePortFactory for MandatoryCheckpointCore {
    fn create(&mut self, run: &ScheduledRun) -> Result<ScheduledRuntimePort, ContractError> {
        let identity = CoshCoreBridgeIdentity {
            installation_id: self.installation.clone(),
            actor_id: Some(run.actor.actor_id.clone()),
            task_id: run.task_id.clone(),
            run_id: run.run_id.clone(),
            agent_session_id: AgentSessionId::new(),
            binding_id: RuntimeBindingId::new(),
            runtime_instance_id: RuntimeInstanceId::new(),
            runtime_generation: run.lease_generation,
            provider_authority: BoundedName::new("cosh-core").unwrap(),
            provider_scope_digest: test_digest(),
        };
        let initialize = serde_json::json!({"type":"control_response","response":{"subtype":"success","request_id":format!("init-{}",identity.runtime_instance_id),"response":{"subtype":"initialize","protocol_version":4,"execution_profile":"gateway_brokered_checkpoint_v1","capability_profile":{"profile_id":"workspace-checkpoint-v1","manifest_digest":"6b3e7093e7b8656d4a7cf21faa85b9eed761ef415d002623cfc442f3ef3c8ae1"},"runtime_tools":["ask_user_question","workspace_checkpoint_create"],"capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":false,"can_handle_shell_evidence_tool":false,"can_handle_approval_receipt":true,"can_handle_hosted_checkpoint_create":true,"can_handle_brokered_ask_user":true}}}});
        let request = serde_json::json!({"type":"control_request","request_id":"private-checkpoint","request":{"subtype":"can_use_tool","tool_name":"workspace_checkpoint_create","input":{},"tool_use_id":"checkpoint-call","hook_requires_approval":self.mandatory}});
        let script = format!(
            r#"
IFS= read -r line
printf '%s\n' '{}'
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"provider-session"}}'
IFS= read -r line
printf '%s\n' '{{"type":"stream_event","event":{{"type":"message_start"}}}}'
printf '%s\n' '{{"type":"stream_event","event":{{"type":"content_block_start","index":0,"content_block":{{"type":"tool_use","id":"checkpoint-call","name":"workspace_checkpoint_create"}}}}}}'
printf '%s\n' '{{"type":"stream_event","event":{{"type":"content_block_stop","index":0}}}}'
printf '%s\n' '{{"type":"stream_event","event":{{"type":"message_stop"}}}}'
printf '%s\n' '{}'
IFS= read -r line
IFS= read -r line
printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"session_id":"provider-session"}}'
"#,
            initialize, request
        );
        let mut launch = RuntimeLaunchSpec::new("/bin/sh", &self.workspace);
        launch.arguments = vec!["-c".into(), script.into()];
        let mut config = CoshCoreBridgeConfig::new(launch, run.workspace.clone(), identity)
            .gateway_brokered_checkpoint(CoshCoreBrokeredContext {
                actor: run.actor.clone(),
                target: run.target.clone(),
            });
        config.prompt_timeout = Duration::from_secs(5);
        config.shutdown_grace = Duration::from_millis(50);
        let bridge = CoshCoreBridge::launch(config)
            .map_err(|error| test_contract_error(format!("{error:?}")))?;
        Ok(ScheduledRuntimePort::new(
            Box::new(bridge),
            run.workspace.clone(),
        ))
    }
}

#[test]
fn mandatory_checkpoint_reapproval_overrides_allow_all() {
    for mandatory in [true, false] {
        let root = TempDir::new().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let database = root.path().join("gateway.db");
        let installation = InstallationId::new();
        let profile = GatewayCapabilityProfile::workspace_checkpoint_v1();
        let mut coordinator = TaskCoordinator::open_for_capability_profile(
            &database,
            Some(installation.clone()),
            profile,
        )
        .unwrap();
        let actor = actor_ref_for_uid(&installation, 1000).unwrap();
        let workspace = WorkspaceRef {
            scope_digest: sha256_digest(b"cosh.gateway.test.workspace.v1"),
            display_name: None,
        };
        coordinator
            .commit_submission(
                &actor,
                IdempotencyKey::new("mandatory-checkpoint").unwrap(),
                TaskLaunchSpecV1::new(
                    BoundedText::new("checkpoint after uncertain result").unwrap(),
                    TaskRuntime::Core,
                    workspace,
                    CheckpointPolicy::Off,
                    ApprovalPolicy::AllowAll,
                ),
                profile.governed_target(),
                RuntimeSelector {
                    runtime: BoundedName::new("core").unwrap(),
                    profile: Some(BoundedName::new("gateway-checkpoint-v1").unwrap()),
                },
                profile,
            )
            .unwrap();
        drop(coordinator);
        let target_calls = Arc::new(Mutex::new(0));
        let resolve_calls = Arc::new(Mutex::new(0));
        let mut scheduler = TaskScheduler::open_for_capability_profile(
            &database,
            Some(installation.clone()),
            BoundedOpaque::new("mandatory-checkpoint-worker").unwrap(),
            profile,
            ScheduledAgentRuntimeFactory::new(MandatoryCheckpointCore {
                installation,
                workspace: root.path().to_path_buf(),
                mandatory,
            }),
        )
        .unwrap()
        .with_brokered_execution_driver(Box::new(ConclusiveDriver {
            target_calls: Arc::clone(&target_calls),
            resolve_calls: Arc::clone(&resolve_calls),
            mode: ConclusiveDriverMode::CompletionUnknownAfterCommit,
        }));
        assert!(matches!(
            scheduler.tick(now_ms().unwrap()).unwrap(),
            SchedulerTick::Started(_)
        ));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            scheduler.tick(now_ms().unwrap()).unwrap();
            if *resolve_calls.lock().unwrap() > 0
                || scheduler
                    .active
                    .as_ref()
                    .and_then(|active| active.pending_brokered.as_ref())
                    .is_some()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "checkpoint request was not observed"
            );
        }
        let calls_before_user_decision = *target_calls.lock().unwrap();
        let resolutions_before_user_decision = *resolve_calls.lock().unwrap();
        if mandatory && resolutions_before_user_decision == 0 {
            let approval_id = scheduler
                .active
                .as_ref()
                .unwrap()
                .pending_brokered
                .as_ref()
                .unwrap()
                .approval
                .approval_id
                .clone();
            scheduler
                .resolve_approval(
                    &actor.actor_id,
                    IdempotencyKey::new("explicit-checkpoint-approval").unwrap(),
                    &approval_id,
                    ApprovalDecision::Approve,
                    now_ms().unwrap(),
                )
                .unwrap();
            assert_eq!(*target_calls.lock().unwrap(), 1);
        }
        scheduler.shutdown(now_ms().unwrap()).unwrap();
        assert_eq!(
            calls_before_user_decision,
            usize::from(!mandatory),
            "mandatory checkpoint executed under AllowAll before a fresh decision"
        );
        assert_eq!(resolutions_before_user_decision, usize::from(!mandatory));
    }
}
