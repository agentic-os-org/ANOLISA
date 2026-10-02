//! Fake-Core coverage for public Runtime mapping and settlement.

use std::time::{Duration, Instant};

use cosh_gateway_contracts::{
    common::{
        ActorKind, ActorRef, AuthAssurance, BoundedName, BoundedOpaque, BoundedText, ContentPart,
        Digest, TargetRef, WorkspaceRef,
    },
    external::ExternalRefKind,
    ids::{
        ActorId, AgentSessionId, ApprovalId, CheckpointId, ExecutionId, InstallationId, RequestId,
        RunId, RuntimeBindingId, RuntimeInstanceId, RuntimeMessageId, TaskId, ToolUseId, TurnId,
    },
    profile::GatewayCapabilityProfile,
    runtime::{
        AgentRuntimeCommand, AgentRuntimeEvent, BrokeredExecutionDelivery,
        BrokeredExecutionOutcome, BrokeredOperationResult, RuntimeInputResponse,
        RuntimeInputSelections, RuntimePermissionDecision, TurnOutcome,
        WorkspaceCheckpointCreateV1Outcome, WorkspaceCheckpointCreateV1Result,
    },
    task::CancelReason,
};

use super::*;

fn workspace_ref() -> WorkspaceRef {
    WorkspaceRef {
        scope_digest: Digest::parse("0".repeat(64)).unwrap(),
        display_name: Some(BoundedText::new("test workspace").unwrap()),
    }
}

#[cfg(unix)]
fn bridge(script: &str, workspace: &tempfile::TempDir) -> (CoshCoreBridge, CoshCoreBridgeIdentity) {
    let identity = CoshCoreBridgeIdentity {
        installation_id: InstallationId::new(),
        actor_id: None,
        task_id: TaskId::new(),
        run_id: RunId::new(),
        agent_session_id: AgentSessionId::new(),
        binding_id: RuntimeBindingId::new(),
        runtime_instance_id: RuntimeInstanceId::new(),
        runtime_generation: 7,
        provider_authority: BoundedName::new("cosh-core").unwrap(),
        provider_scope_digest: Digest::parse("1".repeat(64)).unwrap(),
    };
    let initialize_request_id = format!("init-{}", identity.runtime_instance_id);
    let script = script.replace("__INIT_REQUEST_ID__", &initialize_request_id);
    let mut launch = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    launch.arguments = vec!["-c".into(), script.into()];
    let mut config = CoshCoreBridgeConfig::new(launch, workspace_ref(), identity.clone());
    config.prompt_timeout = Duration::from_secs(2);
    config.shutdown_grace = Duration::from_millis(50);
    (CoshCoreBridge::launch(config).unwrap(), identity)
}

#[cfg(unix)]
fn brokered_bridge(
    script: &str,
    workspace: &tempfile::TempDir,
) -> (CoshCoreBridge, CoshCoreBridgeIdentity) {
    let actor = ActorRef {
        actor_id: ActorId::new(),
        actor_kind: ActorKind::Human,
        issuer: BoundedName::new("local-os").unwrap(),
        assurance: AuthAssurance::LocalOs,
    };
    let identity = CoshCoreBridgeIdentity {
        installation_id: InstallationId::new(),
        actor_id: Some(actor.actor_id.clone()),
        task_id: TaskId::new(),
        run_id: RunId::new(),
        agent_session_id: AgentSessionId::new(),
        binding_id: RuntimeBindingId::new(),
        runtime_instance_id: RuntimeInstanceId::new(),
        runtime_generation: 11,
        provider_authority: BoundedName::new("cosh-core").unwrap(),
        provider_scope_digest: Digest::parse("1".repeat(64)).unwrap(),
    };
    let initialize_request_id = format!("init-{}", identity.runtime_instance_id);
    let script = script.replace("__INIT_REQUEST_ID__", &initialize_request_id);
    let mut launch = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    launch.arguments = vec!["-c".into(), script.into()];
    let mut config = CoshCoreBridgeConfig::new(launch, workspace_ref(), identity.clone())
        .gateway_brokered(CoshCoreBrokeredContext {
            actor,
            target: TargetRef {
                kind: BoundedName::new("local").unwrap(),
                authority: BoundedName::new("cosh").unwrap(),
                identifier: BoundedOpaque::new("primary").unwrap(),
            },
        });
    config.prompt_timeout = Duration::from_secs(2);
    config.shutdown_grace = Duration::from_millis(50);
    (CoshCoreBridge::launch(config).unwrap(), identity)
}

#[cfg(unix)]
fn checkpoint_brokered_bridge(
    script: &str,
    workspace: &tempfile::TempDir,
) -> (CoshCoreBridge, CoshCoreBridgeIdentity, TargetRef) {
    let actor = ActorRef {
        actor_id: ActorId::new(),
        actor_kind: ActorKind::Human,
        issuer: BoundedName::new("local-os").unwrap(),
        assurance: AuthAssurance::LocalOs,
    };
    let target = TargetRef {
        kind: BoundedName::new("local").unwrap(),
        authority: BoundedName::new("cosh").unwrap(),
        identifier: BoundedOpaque::new("primary").unwrap(),
    };
    let identity = CoshCoreBridgeIdentity {
        installation_id: InstallationId::new(),
        actor_id: Some(actor.actor_id.clone()),
        task_id: TaskId::new(),
        run_id: RunId::new(),
        agent_session_id: AgentSessionId::new(),
        binding_id: RuntimeBindingId::new(),
        runtime_instance_id: RuntimeInstanceId::new(),
        runtime_generation: 12,
        provider_authority: BoundedName::new("cosh-core").unwrap(),
        provider_scope_digest: Digest::parse("1".repeat(64)).unwrap(),
    };
    let initialize_request_id = format!("init-{}", identity.runtime_instance_id);
    let script = script.replace("__INIT_REQUEST_ID__", &initialize_request_id);
    let mut launch = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    launch.arguments = vec!["-c".into(), script.into()];
    let mut config = CoshCoreBridgeConfig::new(launch, workspace_ref(), identity.clone())
        .gateway_brokered_checkpoint(CoshCoreBrokeredContext {
            actor,
            target: target.clone(),
        });
    config.prompt_timeout = Duration::from_secs(2);
    config.shutdown_grace = Duration::from_millis(50);
    (CoshCoreBridge::launch(config).unwrap(), identity, target)
}

#[cfg(unix)]
fn workspace_write_brokered_bridge(
    script: &str,
    workspace: &tempfile::TempDir,
) -> (CoshCoreBridge, CoshCoreBridgeIdentity, TargetRef) {
    let actor = ActorRef {
        actor_id: ActorId::new(),
        actor_kind: ActorKind::Human,
        issuer: BoundedName::new("local-os").unwrap(),
        assurance: AuthAssurance::LocalOs,
    };
    let target = GatewayCapabilityProfile::workspace_write_v1().governed_target();
    let identity = CoshCoreBridgeIdentity {
        installation_id: InstallationId::new(),
        actor_id: Some(actor.actor_id.clone()),
        task_id: TaskId::new(),
        run_id: RunId::new(),
        agent_session_id: AgentSessionId::new(),
        binding_id: RuntimeBindingId::new(),
        runtime_instance_id: RuntimeInstanceId::new(),
        runtime_generation: 13,
        provider_authority: BoundedName::new("cosh-core").unwrap(),
        provider_scope_digest: Digest::parse("1".repeat(64)).unwrap(),
    };
    let initialize_request_id = format!("init-{}", identity.runtime_instance_id);
    let script = script.replace("__INIT_REQUEST_ID__", &initialize_request_id);
    let mut launch = RuntimeLaunchSpec::new("/bin/sh", workspace.path());
    launch.arguments = vec!["-c".into(), script.into()];
    let mut config = CoshCoreBridgeConfig::new(launch, workspace_ref(), identity.clone())
        .gateway_brokered_workspace_write(CoshCoreBrokeredContext {
            actor,
            target: target.clone(),
        });
    config.prompt_timeout = Duration::from_secs(2);
    config.shutdown_grace = Duration::from_millis(50);
    (CoshCoreBridge::launch(config).unwrap(), identity, target)
}

fn open(bridge: &mut CoshCoreBridge, identity: &CoshCoreBridgeIdentity) {
    bridge
        .dispatch(
            AgentRuntimeCommand::OpenSession {
                task_id: identity.task_id.clone(),
                run_id: identity.run_id.clone(),
                workspace: workspace_ref(),
            },
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
}

#[cfg(unix)]
#[test]
fn core_bridge_maps_identity_stream_and_terminal_once() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1)
            printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"__INIT_REQUEST_ID__","response":{"subtype":"initialize","protocol_version":1,"capabilities":{}}}}'
            printf '%s\n' '{"type":"system","subtype":"init","session_id":"provider-session","model":"test","tools":[]}'
            ;;
        2)
            printf '%s\n' '{"type":"stream_event","event":{"type":"message_start"}}'
            printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}}'
            printf '%s\n' '{"type":"stream_event","event":{"type":"message_stop"}}'
            printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"private result must not leak","session_id":"provider-session"}'
            ;;
    esac
done
"#;
    let (mut bridge, identity) = bridge(script, &workspace);
    open(&mut bridge, &identity);

    let opened = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(opened.sequence, 1);
    assert_eq!(opened.binding_id, identity.binding_id);
    assert_eq!(
        opened.header.correlation.task_id,
        Some(identity.task_id.clone())
    );
    assert_eq!(
        opened.header.correlation.run_id,
        Some(identity.run_id.clone())
    );
    let AgentRuntimeEvent::SessionOpened { binding } = opened.event else {
        panic!("expected session binding")
    };
    assert_eq!(binding.agent_session_id, identity.agent_session_id);
    assert_eq!(binding.runtime_instance_id, identity.runtime_instance_id);
    assert_eq!(binding.runtime_generation, 7);
    assert_eq!(
        binding.external_session.kind,
        ExternalRefKind::ProviderSession
    );
    assert_eq!(binding.external_session.value.as_str(), "provider-session");

    let turn_id = TurnId::new();
    bridge
        .dispatch(
            AgentRuntimeCommand::Prompt {
                run_id: identity.run_id.clone(),
                turn_id: turn_id.clone(),
                input: vec![ContentPart::Text {
                    text: BoundedText::new("diagnose").unwrap(),
                }],
            },
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    let started = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(started.sequence, 2);
    assert!(matches!(
        started.event,
        AgentRuntimeEvent::TurnStarted { turn_id: observed } if observed == turn_id
    ));
    let chunk = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(chunk.sequence, 3);
    assert!(matches!(
        chunk.event,
        AgentRuntimeEvent::MessageChunk {
            content: ContentPart::Text { ref text },
            ..
        } if text.as_str() == "hello"
    ));
    let terminal = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(terminal.sequence, 4);
    assert!(matches!(
        terminal.event,
        AgentRuntimeEvent::Completed {
            turn_id: ref observed,
            outcome: TurnOutcome::Completed
        } if observed == &turn_id
    ));
    assert_eq!(
        bridge.next_event(Instant::now() + Duration::from_millis(20)),
        Err(AgentRuntimePortError::Terminal)
    );
    assert!(!format!("{terminal:?}").contains("private result must not leak"));
}

#[cfg(unix)]
#[test]
fn cancellation_reaps_core_and_emits_one_cancelled_terminal() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
IFS= read -r line
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"__INIT_REQUEST_ID__","response":{"subtype":"initialize","protocol_version":1,"capabilities":{}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"provider-session"}'
IFS= read -r line
while :; do sleep 1; done
"#;
    let (mut bridge, identity) = bridge(script, &workspace);
    open(&mut bridge, &identity);
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let turn_id = TurnId::new();
    bridge
        .dispatch(
            AgentRuntimeCommand::Prompt {
                run_id: identity.run_id.clone(),
                turn_id: turn_id.clone(),
                input: vec![ContentPart::Text {
                    text: BoundedText::new("wait").unwrap(),
                }],
            },
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();

    let started = Instant::now();
    bridge
        .dispatch(
            AgentRuntimeCommand::Cancel {
                run_id: identity.run_id,
                turn_id: turn_id.clone(),
                cause: CancelReason::UserRequested,
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    let terminal = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        terminal.event,
        AgentRuntimeEvent::Completed {
            turn_id: observed,
            outcome: TurnOutcome::Cancelled
        } if observed == turn_id
    ));
    assert_eq!(
        bridge.next_event(Instant::now() + Duration::from_millis(20)),
        Err(AgentRuntimePortError::Terminal)
    );
}

#[cfg(unix)]
#[test]
fn open_deadline_fails_closed_without_provider_payload() {
    let workspace = tempfile::tempdir().unwrap();
    let (mut bridge, identity) = bridge("read -r line; sleep 60", &workspace);
    let result = bridge.dispatch(
        AgentRuntimeCommand::OpenSession {
            task_id: identity.task_id,
            run_id: identity.run_id,
            workspace: workspace_ref(),
        },
        Instant::now() + Duration::from_millis(40),
    );
    assert!(matches!(
        result,
        Err(AgentRuntimePortError::Deadline {
            operation: "open_session"
        })
    ));
    let terminal = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::TransportFailed { error } = terminal.event else {
        panic!("expected transport failure")
    };
    assert_eq!(error.code.as_str(), "core_session_open_failed");
    assert_eq!(
        error.safe_message.as_str(),
        "The Agent runtime transport failed"
    );
}

#[cfg(unix)]
#[test]
fn cross_run_commands_are_rejected_before_private_io() {
    let workspace = tempfile::tempdir().unwrap();
    let (mut bridge, identity) = bridge("sleep 60", &workspace);
    let result = bridge.dispatch(
        AgentRuntimeCommand::OpenSession {
            task_id: identity.task_id,
            run_id: RunId::new(),
            workspace: workspace_ref(),
        },
        Instant::now() + Duration::from_secs(1),
    );
    assert_eq!(result, Err(AgentRuntimePortError::IdentityMismatch));
}

#[cfg(unix)]
#[test]
fn session_open_must_be_delivered_before_prompt_and_idle_cancel() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
IFS= read -r line
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"__INIT_REQUEST_ID__","response":{"subtype":"initialize","protocol_version":1,"capabilities":{}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"provider-session"}'
IFS= read -r line
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"session_id":"provider-session"}'
"#;
    let (mut bridge, identity) = bridge(script, &workspace);
    open(&mut bridge, &identity);

    let prompt = || AgentRuntimeCommand::Prompt {
        run_id: identity.run_id.clone(),
        turn_id: TurnId::new(),
        input: vec![ContentPart::Text {
            text: BoundedText::new("continue").unwrap(),
        }],
    };
    assert_eq!(
        bridge.dispatch(prompt(), Instant::now() + Duration::from_secs(1)),
        Err(AgentRuntimePortError::InvalidState {
            operation: "prompt",
            state: "session-opened-pending",
        })
    );

    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        bridge.dispatch(
            AgentRuntimeCommand::Cancel {
                run_id: identity.run_id.clone(),
                turn_id: TurnId::new(),
                cause: CancelReason::UserRequested,
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::InvalidState {
            operation: "cancel",
            state: "session-open",
        })
    );

    bridge
        .dispatch(prompt(), Instant::now() + Duration::from_secs(1))
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let terminal = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        terminal.event,
        AgentRuntimeEvent::Completed {
            outcome: TurnOutcome::Completed,
            ..
        }
    ));
}

#[test]
fn aggregate_prompt_size_is_bounded() {
    let input = vec![
        ContentPart::Text {
            text: BoundedText::new("abc").unwrap(),
        },
        ContentPart::Text {
            text: BoundedText::new("def").unwrap(),
        },
    ];
    assert_eq!(prompt_text(input.clone(), 7).unwrap(), "abc\ndef");
    assert_eq!(prompt_text(input, 6), Err(AgentRuntimePortError::Protocol));
}

#[cfg(unix)]
#[test]
fn tool_identity_retention_is_bounded() {
    let workspace = tempfile::tempdir().unwrap();
    let (mut bridge, _) = bridge("sleep 60", &workspace);
    bridge.current_message = Some(RuntimeMessageId::new());
    for index in 0..MAX_TOOL_USES_PER_TURN {
        bridge.tool_ids.insert(
            format!("tool-{index}"),
            ObservedToolUse {
                tool_use_id: ToolUseId::new(),
                name: BoundedName::new("shell").unwrap(),
            },
        );
    }

    let result = bridge.map_stream(CoshCoreStreamEvent::ContentBlockStart {
        index: 0,
        content_block: CoshCoreContentBlockInfo::ToolUse {
            id: "one-too-many".to_owned(),
            name: "shell".to_owned(),
        },
    });
    assert_eq!(result, Err(AgentRuntimePortError::Protocol));
}

#[cfg(unix)]
#[test]
fn workspace_write_profile_maps_one_core_fenced_permission_and_exact_allow() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
IFS= read -r line
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"__INIT_REQUEST_ID__","response":{"subtype":"initialize","protocol_version":5,"execution_profile":"gateway_brokered_workspace_write_v1","capability_profile":{"profile_id":"workspace-write-v1","manifest_digest":"30574302eeba3adbb5ea143a8a869331d58a15bd24b9532d0f52613136bb2b2a"},"runtime_tools":["ask_user_question","write_file"],"capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":false,"can_handle_shell_evidence_tool":false,"can_handle_approval_receipt":true,"can_handle_brokered_ask_user":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"provider-session"}'
IFS= read -r line
printf '%s\n' '{"type":"stream_event","event":{"type":"message_start"}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"write-call","name":"write_file"}}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_stop","index":0}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"message_stop"}}'
printf '%s\n' '{"type":"control_request","request_id":"private-write-1","request":{"subtype":"can_use_tool","tool_name":"write_file","input":{"path":"1.txt","content":"one"},"tool_use_id":"write-call"}}'
IFS= read -r line
test "$line" = '{"type":"control_response","response":{"subtype":"success","request_id":"private-write-1","response":{"behavior":"allow"}}}' || exit 51
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"session_id":"provider-session"}'
"#;
    let (mut bridge, identity, expected_target) =
        workspace_write_brokered_bridge(script, &workspace);
    open(&mut bridge, &identity);
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let turn_id = TurnId::new();
    bridge
        .dispatch(
            AgentRuntimeCommand::Prompt {
                run_id: identity.run_id.clone(),
                turn_id: turn_id.clone(),
                input: vec![ContentPart::Text {
                    text: BoundedText::new("write one file").unwrap(),
                }],
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let tool = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ToolInvocationUpdated { snapshot } = tool.event else {
        panic!("expected write tool observation")
    };
    assert_eq!(
        snapshot.authority,
        ExecutionAuthority::ProviderNativeObserved
    );

    let permission = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::CoreExecutionPermissionRequested {
        turn_id: observed_turn,
        tool_use_id,
        request,
        callback,
        ..
    } = permission.event
    else {
        panic!("expected Core-specific permission event")
    };
    assert_eq!(observed_turn, turn_id);
    assert_eq!(tool_use_id, snapshot.tool_use_id);
    assert_eq!(request.target, expected_target);
    assert_eq!(request.operation.namespace.as_str(), "workspace");
    assert_eq!(request.operation.name.as_str(), "write_file");
    assert_eq!(request.requested_scope.resource.as_str(), "workspace");
    assert_eq!(request.requested_scope.access.as_str(), "write");
    assert_eq!(
        callback.normalized_operation_digest,
        request.operation_digest
    );
    assert_eq!(
        callback.private_request_id_digest,
        sha256_digest(b"private-write-1")
    );
    assert_eq!(
        callback.private_tool_use_id_digest,
        sha256_digest(b"write-call")
    );

    assert_eq!(
        bridge.dispatch(
            AgentRuntimeCommand::ResolvePermission {
                request_id: RequestId::new(),
                decision: RuntimePermissionDecision::RuntimeNativeAllowOnce,
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::IdentityMismatch)
    );
    assert_eq!(
        bridge.dispatch(
            AgentRuntimeCommand::ResolvePermission {
                request_id: request.request_id.clone(),
                decision: RuntimePermissionDecision::ProviderNativeAllowOnce,
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::Unsupported {
            operation: "provider-native permission decision"
        })
    );
    bridge
        .dispatch(
            AgentRuntimeCommand::ResolvePermission {
                request_id: request.request_id,
                decision: RuntimePermissionDecision::RuntimeNativeAllowOnce,
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    let completed = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        completed.event,
        AgentRuntimeEvent::Completed {
            turn_id: observed,
            outcome: TurnOutcome::Completed
        } if observed == turn_id
    ));
}

#[cfg(unix)]
#[test]
fn checkpoint_profile_brokers_one_typed_correlated_result() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
IFS= read -r line
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"__INIT_REQUEST_ID__","response":{"subtype":"initialize","protocol_version":4,"execution_profile":"gateway_brokered_checkpoint_v1","capability_profile":{"profile_id":"workspace-checkpoint-v1","manifest_digest":"6b3e7093e7b8656d4a7cf21faa85b9eed761ef415d002623cfc442f3ef3c8ae1"},"runtime_tools":["ask_user_question","workspace_checkpoint_create"],"capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":false,"can_handle_shell_evidence_tool":false,"can_handle_approval_receipt":true,"can_handle_hosted_checkpoint_create":true,"can_handle_brokered_ask_user":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"provider-session"}'
IFS= read -r line
printf '%s\n' '{"type":"stream_event","event":{"type":"message_start"}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"checkpoint-call","name":"workspace_checkpoint_create"}}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_stop","index":0}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"message_stop"}}'
printf '%s\n' '{"type":"control_request","request_id":"private-checkpoint-1","request":{"subtype":"can_use_tool","tool_name":"workspace_checkpoint_create","input":{},"tool_use_id":"checkpoint-call","hook_requires_approval":true}}'
IFS= read -r line
test "$line" = '{"type":"approval_receipt","request_id":"private-checkpoint-1"}' || exit 41
IFS= read -r line
case "$line" in
    *'"request_id":"private-checkpoint-1"'*'"behavior":"host_executed_checkpoint_create"'*'"checkpointResult":{"checkpoint_id":"ckp_'*'"status":"created","snapshot_id":"snapshot-1"'*) ;;
    *) exit 42 ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"session_id":"provider-session"}'
"#;
    let (mut bridge, identity, expected_target) = checkpoint_brokered_bridge(script, &workspace);
    open(&mut bridge, &identity);
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let turn_id = TurnId::new();
    bridge
        .dispatch(
            AgentRuntimeCommand::Prompt {
                run_id: identity.run_id.clone(),
                turn_id: turn_id.clone(),
                input: vec![ContentPart::Text {
                    text: BoundedText::new("checkpoint now").unwrap(),
                }],
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let tool = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ToolInvocationUpdated { snapshot } = tool.event else {
        panic!("expected checkpoint tool observation")
    };
    assert_eq!(snapshot.authority, ExecutionAuthority::CoshBrokered);
    assert_eq!(snapshot.turn_id, turn_id);

    let requested = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::BrokeredExecutionRequested {
        turn_id: requested_turn,
        tool_use_id,
        request,
        operation,
        ..
    } = requested.event
    else {
        panic!("expected typed checkpoint request")
    };
    assert_eq!(requested_turn, turn_id);
    assert_eq!(tool_use_id, Some(snapshot.tool_use_id));
    assert_eq!(request.task_id, identity.task_id);
    assert_eq!(request.run_id, identity.run_id);
    assert_eq!(request.actor.actor_id, identity.actor_id.unwrap());
    assert_eq!(request.target, expected_target);
    assert_eq!(request.requested_scope.resource.as_str(), "workspace");
    assert_eq!(request.requested_scope.access.as_str(), "checkpoint_create");
    assert_eq!(request.operation.namespace.as_str(), "workspace");
    assert_eq!(request.operation.name.as_str(), "checkpoint_create");
    assert!(request.expires_at_ms > now_ms());
    let BrokeredOperation::WorkspaceCheckpointCreateV1(operation_input) = &operation;
    assert_eq!(
        request.operation.arguments_digest,
        digest_json(operation_input).unwrap()
    );
    assert_eq!(
        request.operation_digest,
        digest_json(&(&request.operation, &operation)).unwrap()
    );
    assert_eq!(
        request.input_digest,
        digest_json(&(
            "workspace_checkpoint_create",
            serde_json::json!({}),
            Option::<String>::None,
            "checkpoint-call",
            Option::<String>::None,
            true,
        ))
        .unwrap()
    );
    assert_eq!(
        operation,
        bridge.pending_brokered.as_ref().unwrap().operation
    );
    let matching = BrokeredExecutionDelivery {
        request_id: request.request_id.clone(),
        outcome: BrokeredExecutionOutcome::Succeeded {
            execution_id: ExecutionId::new(),
            result: BrokeredOperationResult::WorkspaceCheckpointCreateV1(
                WorkspaceCheckpointCreateV1Result {
                    checkpoint_id: operation_input.checkpoint_id.clone(),
                    outcome: WorkspaceCheckpointCreateV1Outcome::Created {
                        snapshot_id: BoundedOpaque::new("snapshot-1").unwrap(),
                    },
                },
            ),
        },
    };
    assert_eq!(
        bridge.dispatch(
            AgentRuntimeCommand::DeliverBrokeredResult {
                delivery: matching.clone()
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::IdentityMismatch)
    );

    assert_eq!(
        bridge.dispatch(
            AgentRuntimeCommand::AcknowledgeBrokeredRequest {
                acknowledgement: cosh_gateway_contracts::runtime::BrokeredRequestAcknowledgement {
                    request_id: RequestId::new(),
                    approval_id: ApprovalId::new(),
                },
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::IdentityMismatch)
    );
    bridge
        .dispatch(
            AgentRuntimeCommand::AcknowledgeBrokeredRequest {
                acknowledgement: cosh_gateway_contracts::runtime::BrokeredRequestAcknowledgement {
                    request_id: request.request_id.clone(),
                    approval_id: ApprovalId::new(),
                },
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(
        bridge.dispatch(
            AgentRuntimeCommand::AcknowledgeBrokeredRequest {
                acknowledgement: cosh_gateway_contracts::runtime::BrokeredRequestAcknowledgement {
                    request_id: request.request_id.clone(),
                    approval_id: ApprovalId::new(),
                },
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::IdentityMismatch)
    );

    let mut wrong_request = matching.clone();
    wrong_request.request_id = RequestId::new();
    assert_eq!(
        bridge.dispatch(
            AgentRuntimeCommand::DeliverBrokeredResult {
                delivery: wrong_request
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::IdentityMismatch)
    );
    let mismatched = BrokeredExecutionDelivery {
        request_id: request.request_id.clone(),
        outcome: BrokeredExecutionOutcome::Succeeded {
            execution_id: ExecutionId::new(),
            result: BrokeredOperationResult::WorkspaceCheckpointCreateV1(
                WorkspaceCheckpointCreateV1Result {
                    checkpoint_id: CheckpointId::new(),
                    outcome: WorkspaceCheckpointCreateV1Outcome::Created {
                        snapshot_id: BoundedOpaque::new("snapshot-1").unwrap(),
                    },
                },
            ),
        },
    };
    assert_eq!(
        bridge.dispatch(
            AgentRuntimeCommand::DeliverBrokeredResult {
                delivery: mismatched
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::IdentityMismatch)
    );

    bridge
        .dispatch(
            AgentRuntimeCommand::DeliverBrokeredResult { delivery: matching },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    let terminal = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        terminal.event,
        AgentRuntimeEvent::Completed {
            turn_id: observed,
            outcome: TurnOutcome::Completed,
        } if observed == turn_id
    ));
}

#[cfg(unix)]
#[test]
fn checkpoint_profile_rejects_nonempty_provider_input() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
IFS= read -r line
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"__INIT_REQUEST_ID__","response":{"subtype":"initialize","protocol_version":4,"execution_profile":"gateway_brokered_checkpoint_v1","capability_profile":{"profile_id":"workspace-checkpoint-v1","manifest_digest":"6b3e7093e7b8656d4a7cf21faa85b9eed761ef415d002623cfc442f3ef3c8ae1"},"runtime_tools":["ask_user_question","workspace_checkpoint_create"],"capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":false,"can_handle_shell_evidence_tool":false,"can_handle_approval_receipt":true,"can_handle_hosted_checkpoint_create":true,"can_handle_brokered_ask_user":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"provider-session"}'
IFS= read -r line
printf '%s\n' '{"type":"stream_event","event":{"type":"message_start"}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"checkpoint-call","name":"workspace_checkpoint_create"}}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_stop","index":0}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"message_stop"}}'
printf '%s\n' '{"type":"control_request","request_id":"private-checkpoint-1","request":{"subtype":"can_use_tool","tool_name":"workspace_checkpoint_create","input":{"workspace":"provider-controlled"},"tool_use_id":"checkpoint-call","hook_requires_approval":true}}'
"#;
    let (mut bridge, identity, _) = checkpoint_brokered_bridge(script, &workspace);
    open(&mut bridge, &identity);
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    bridge
        .dispatch(
            AgentRuntimeCommand::Prompt {
                run_id: identity.run_id,
                turn_id: TurnId::new(),
                input: vec![ContentPart::Text {
                    text: BoundedText::new("checkpoint provider path").unwrap(),
                }],
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let failed = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        failed.event,
        AgentRuntimeEvent::TransportFailed { .. }
    ));
    assert!(bridge.pending_brokered.is_none());
}

#[cfg(unix)]
#[test]
fn checkpoint_profile_rejects_tool_name_substitution_for_a_prior_call() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
IFS= read -r line
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"__INIT_REQUEST_ID__","response":{"subtype":"initialize","protocol_version":4,"execution_profile":"gateway_brokered_checkpoint_v1","capability_profile":{"profile_id":"workspace-checkpoint-v1","manifest_digest":"6b3e7093e7b8656d4a7cf21faa85b9eed761ef415d002623cfc442f3ef3c8ae1"},"runtime_tools":["ask_user_question","workspace_checkpoint_create"],"capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":false,"can_handle_shell_evidence_tool":false,"can_handle_approval_receipt":true,"can_handle_hosted_checkpoint_create":true,"can_handle_brokered_ask_user":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"provider-session"}'
IFS= read -r line
printf '%s\n' '{"type":"stream_event","event":{"type":"message_start"}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"rebound-call","name":"ask_user_question"}}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_stop","index":0}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"message_stop"}}'
printf '%s\n' '{"type":"control_request","request_id":"private-checkpoint-1","request":{"subtype":"can_use_tool","tool_name":"workspace_checkpoint_create","input":{},"tool_use_id":"rebound-call","hook_requires_approval":true}}'
"#;
    let (mut bridge, identity, _) = checkpoint_brokered_bridge(script, &workspace);
    open(&mut bridge, &identity);
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    bridge
        .dispatch(
            AgentRuntimeCommand::Prompt {
                run_id: identity.run_id,
                turn_id: TurnId::new(),
                input: vec![ContentPart::Text {
                    text: BoundedText::new("rebind a tool call").unwrap(),
                }],
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let tool = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        tool.event,
        AgentRuntimeEvent::ToolInvocationUpdated { snapshot }
            if snapshot.authority == ExecutionAuthority::ProviderNativeObserved
    ));
    let failed = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        failed.event,
        AgentRuntimeEvent::TransportFailed { .. }
    ));
    assert!(bridge.pending_brokered.is_none());
}

#[cfg(unix)]
#[test]
fn brokered_profile_rejects_checkpoint_request_before_capability() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
IFS= read -r line
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"__INIT_REQUEST_ID__","response":{"subtype":"initialize","protocol_version":3,"execution_profile":"gateway_brokered_v1","capability_profile":{"profile_id":"task-only-v1","manifest_digest":"2b95e0f3e28df8eb2b7930f2dec3650ffe399f971671c971865e4663c382c94a"},"runtime_tools":["ask_user_question"],"capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":false,"can_handle_approval_receipt":true,"can_handle_hosted_checkpoint_create":false,"can_handle_brokered_ask_user":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"provider-session"}'
IFS= read -r line
printf '%s\n' '{"type":"stream_event","event":{"type":"message_start"}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"provider-tool-1","name":"workspace_checkpoint_create"}}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_stop","index":0}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"message_stop"}}'
printf '%s\n' '{"type":"control_request","request_id":"private-req-1","request":{"subtype":"can_use_tool","tool_name":"workspace_checkpoint_create","input":{},"tool_use_id":"provider-tool-1","hook_requires_approval":true}}'
"#;
    let (mut bridge, identity) = brokered_bridge(script, &workspace);
    open(&mut bridge, &identity);
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let turn_id = TurnId::new();
    bridge
        .dispatch(
            AgentRuntimeCommand::Prompt {
                run_id: identity.run_id.clone(),
                turn_id,
                input: vec![ContentPart::Text {
                    text: BoundedText::new("checkpoint now").unwrap(),
                }],
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let tool = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        tool.event,
        AgentRuntimeEvent::ToolInvocationUpdated { ref snapshot }
            if snapshot.authority == ExecutionAuthority::ProviderNativeObserved
    ));
    let failed = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        failed.event,
        AgentRuntimeEvent::TransportFailed { .. }
    ));
}

#[cfg(unix)]
#[test]
fn brokered_ask_user_is_exact_single_use_and_side_effect_free() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
IFS= read -r line
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"__INIT_REQUEST_ID__","response":{"subtype":"initialize","protocol_version":3,"execution_profile":"gateway_brokered_v1","capability_profile":{"profile_id":"task-only-v1","manifest_digest":"2b95e0f3e28df8eb2b7930f2dec3650ffe399f971671c971865e4663c382c94a"},"runtime_tools":["ask_user_question"],"capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":false,"can_handle_approval_receipt":true,"can_handle_hosted_checkpoint_create":false,"can_handle_brokered_ask_user":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"provider-session"}'
IFS= read -r line
printf '%s\n' '{"type":"stream_event","event":{"type":"message_start"}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"question-call","name":"ask_user_question"}}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_stop","index":0}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"message_stop"}}'
printf '%s\n' '{"type":"control_request","request_id":"private-question-1","request":{"subtype":"ask_user","tool_use_id":"question-call","question":"Choose a branch","options":[{"label":"main","description":"Use the default branch"},{"label":"release"}],"allow_free_text":false,"multi_select":false}}'
IFS= read -r line
case "$line" in *'"request_id":"private-question-1"'*'"behavior":"answer"'*'"answer":"main"'*) ;; *) exit 31 ;; esac
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"session_id":"provider-session"}'
"#;
    let (mut bridge, identity) = brokered_bridge(script, &workspace);
    open(&mut bridge, &identity);
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let turn_id = TurnId::new();
    bridge
        .dispatch(
            AgentRuntimeCommand::Prompt {
                run_id: identity.run_id.clone(),
                turn_id: turn_id.clone(),
                input: vec![ContentPart::Text {
                    text: BoundedText::new("ask safely").unwrap(),
                }],
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let tool = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        tool.event,
        AgentRuntimeEvent::ToolInvocationUpdated { snapshot }
            if snapshot.authority == ExecutionAuthority::ProviderNativeObserved
    ));
    let requested = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::InputRequested { request } = requested.event else {
        panic!("expected bounded input request")
    };
    assert_eq!(request.run_id(), &identity.run_id);
    assert_eq!(request.turn_id(), &turn_id);
    assert_eq!(request.question().as_str(), "Choose a branch");
    assert_eq!(request.options().len(), 2);
    assert!(!request.allows_free_text());

    let resolve = |request_id, run_id, turn_id, selection| AgentRuntimeCommand::ResolveInput {
        request_id,
        run_id,
        turn_id,
        response: RuntimeInputResponse::Options {
            selections: RuntimeInputSelections::new(vec![selection]).unwrap(),
        },
    };
    assert_eq!(
        bridge.dispatch(
            resolve(
                request.request_id().clone(),
                RunId::new(),
                turn_id.clone(),
                0,
            ),
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::IdentityMismatch)
    );
    assert_eq!(
        bridge.dispatch(
            resolve(
                request.request_id().clone(),
                identity.run_id.clone(),
                TurnId::new(),
                0,
            ),
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::IdentityMismatch)
    );
    assert_eq!(
        bridge.dispatch(
            resolve(
                request.request_id().clone(),
                identity.run_id.clone(),
                turn_id.clone(),
                9,
            ),
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::IdentityMismatch)
    );
    let exact = resolve(
        request.request_id().clone(),
        identity.run_id,
        turn_id.clone(),
        0,
    );
    bridge
        .dispatch(exact.clone(), Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        bridge.dispatch(exact, Instant::now() + Duration::from_secs(1)),
        Err(AgentRuntimePortError::IdentityMismatch)
    );
    let terminal = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        terminal.event,
        AgentRuntimeEvent::Completed {
            turn_id: observed,
            outcome: TurnOutcome::Completed,
        } if observed == turn_id
    ));
}

#[cfg(unix)]
#[test]
fn brokered_ask_user_resolution_after_cancel_fails_closed() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
IFS= read -r line
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"__INIT_REQUEST_ID__","response":{"subtype":"initialize","protocol_version":3,"execution_profile":"gateway_brokered_v1","capability_profile":{"profile_id":"task-only-v1","manifest_digest":"2b95e0f3e28df8eb2b7930f2dec3650ffe399f971671c971865e4663c382c94a"},"runtime_tools":["ask_user_question"],"capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":false,"can_handle_approval_receipt":true,"can_handle_hosted_checkpoint_create":false,"can_handle_brokered_ask_user":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"provider-session"}'
IFS= read -r line
printf '%s\n' '{"type":"stream_event","event":{"type":"message_start"}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"question-call","name":"ask_user_question"}}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_stop","index":0}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"message_stop"}}'
printf '%s\n' '{"type":"control_request","request_id":"private-question-1","request":{"subtype":"ask_user","tool_use_id":"question-call","question":"Continue?","options":[],"allow_free_text":true,"multi_select":false}}'
sleep 60
"#;
    let (mut bridge, identity) = brokered_bridge(script, &workspace);
    open(&mut bridge, &identity);
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let turn_id = TurnId::new();
    bridge
        .dispatch(
            AgentRuntimeCommand::Prompt {
                run_id: identity.run_id.clone(),
                turn_id: turn_id.clone(),
                input: vec![ContentPart::Text {
                    text: BoundedText::new("ask then cancel").unwrap(),
                }],
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let requested = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::InputRequested { request } = requested.event else {
        panic!("expected input request")
    };
    bridge
        .dispatch(
            AgentRuntimeCommand::Cancel {
                run_id: identity.run_id.clone(),
                turn_id: turn_id.clone(),
                cause: CancelReason::UserRequested,
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    assert!(matches!(
        bridge.dispatch(
            AgentRuntimeCommand::ResolveInput {
                request_id: request.request_id().clone(),
                run_id: identity.run_id,
                turn_id,
                response: RuntimeInputResponse::Text {
                    text: BoundedText::new("late secret").unwrap(),
                },
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::InvalidState { .. })
    ));
}

#[cfg(unix)]
#[test]
fn brokered_profile_rejects_generic_permission_and_unknown_intent() {
    let workspace = tempfile::tempdir().unwrap();
    let script = r#"
IFS= read -r line
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"__INIT_REQUEST_ID__","response":{"subtype":"initialize","protocol_version":3,"execution_profile":"gateway_brokered_v1","capability_profile":{"profile_id":"task-only-v1","manifest_digest":"2b95e0f3e28df8eb2b7930f2dec3650ffe399f971671c971865e4663c382c94a"},"runtime_tools":["ask_user_question"],"capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":false,"can_handle_approval_receipt":true,"can_handle_hosted_checkpoint_create":false,"can_handle_brokered_ask_user":true}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"provider-session"}'
IFS= read -r line
printf '%s\n' '{"type":"stream_event","event":{"type":"message_start"}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"provider-tool-1","name":"shell"}}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_stop","index":0}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"message_stop"}}'
printf '%s\n' '{"type":"control_request","request_id":"private-req-1","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"touch /tmp/forbidden"},"tool_use_id":"provider-tool-1"}}'
"#;
    let (mut bridge, identity) = brokered_bridge(script, &workspace);
    open(&mut bridge, &identity);
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let turn_id = TurnId::new();
    bridge
        .dispatch(
            AgentRuntimeCommand::Prompt {
                run_id: identity.run_id,
                turn_id,
                input: vec![ContentPart::Text {
                    text: BoundedText::new("do not execute shell").unwrap(),
                }],
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let failed = bridge
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        failed.event,
        AgentRuntimeEvent::TransportFailed { .. }
    ));
    assert_eq!(
        bridge.dispatch(
            AgentRuntimeCommand::ResolvePermission {
                request_id: RequestId::new(),
                decision: RuntimePermissionDecision::ProviderNativeAllowOnce,
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::Unsupported {
            operation: "resolve_permission"
        })
    );
}
