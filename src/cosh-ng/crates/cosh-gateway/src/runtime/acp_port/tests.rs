//! Fake-ACP coverage for identity, permission, and terminal mapping.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cosh_gateway_contracts::{
    capability::{CapabilityRequest, CapabilityScope, OperationDescriptor},
    common::{
        ActorKind, AuthAssurance, BoundedName, BoundedOpaque, BoundedText, ContentPart, Digest,
        TargetRef, WorkspaceRef,
    },
    ids::{
        ActorId, AgentSessionId, ApprovalId, InputRequestId, InstallationId, RequestId, RunId,
        RuntimeBindingId, RuntimeInstanceId, TaskId, TurnId,
    },
    runtime::{
        AgentRuntimeCommand, AgentRuntimeEvent, BrokeredRequestAcknowledgement, ExecutionAuthority,
        RuntimeInputResponse, RuntimePermissionDecision, ToolInvocationStatus, TurnLimit,
        TurnOutcome,
    },
};
use serde_json::json;

use super::*;
use crate::runtime::{
    AcpSessionObservation, AcpSessionTerminal, AcpV1AdapterProfile, AcpV1ClientConfig, AcpV1Codec,
    AcpV1PermissionOption, RuntimeLaunchSpec,
};

#[derive(Default)]
struct FakeState {
    events: VecDeque<AcpSessionEvent>,
    prompts: Vec<String>,
    answers: Vec<(AcpV1RequestId, AcpV1PermissionDecision)>,
    cancelled: bool,
    shutdown: bool,
    disconnected: bool,
}

struct FakeBackend(Arc<Mutex<FakeState>>);

impl AcpSessionBackend for FakeBackend {
    fn initialize(&self) -> Result<(), AcpSessionDriverError> {
        Ok(())
    }
    fn open_session(&self) -> Result<(), AcpSessionDriverError> {
        Ok(())
    }
    fn prompt(&self, text: String) -> Result<(), AcpSessionDriverError> {
        self.0.lock().unwrap().prompts.push(text);
        Ok(())
    }
    fn answer_permission(
        &self,
        request_id: AcpV1RequestId,
        decision: AcpV1PermissionDecision,
    ) -> Result<(), AcpSessionDriverError> {
        self.0.lock().unwrap().answers.push((request_id, decision));
        Ok(())
    }
    fn receive_timeout(
        &self,
        _timeout: Duration,
    ) -> Result<AcpSessionEvent, std::sync::mpsc::RecvTimeoutError> {
        let mut state = self.0.lock().unwrap();
        match state.events.pop_front() {
            Some(event) => Ok(event),
            None if state.disconnected => Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
            None => Err(std::sync::mpsc::RecvTimeoutError::Timeout),
        }
    }
    fn cancel(&self) -> Result<(), AcpSessionDriverError> {
        let mut state = self.0.lock().unwrap();
        state.cancelled = true;
        state
            .events
            .push_back(AcpSessionEvent::Terminal(AcpSessionTerminal {
                kind: AcpSessionTerminalKind::Cancelled,
                detail: None,
                process: None,
            }));
        Ok(())
    }
    fn shutdown(&self) -> Result<(), AcpSessionDriverError> {
        self.0.lock().unwrap().shutdown = true;
        Ok(())
    }
}

struct Normalizer {
    request_id: RequestId,
    mismatch: bool,
}

struct SnapshotDigestNormalizer {
    request_id: RequestId,
}

impl AcpPermissionNormalizer for SnapshotDigestNormalizer {
    fn normalize(
        &mut self,
        request: &AcpV1PermissionRequest,
        context: &AcpPermissionContext,
    ) -> Result<CapabilityRequest, AgentRuntimePortError> {
        use sha2::{Digest as _, Sha256};

        let input =
            serde_json::to_vec(&request.tool_call).map_err(|_| AgentRuntimePortError::Protocol)?;
        let input_digest = Digest::parse(format!("{:x}", Sha256::digest(&input)))
            .map_err(|_| AgentRuntimePortError::Protocol)?;
        let operation_digest = Digest::parse(format!(
            "{:x}",
            Sha256::digest(
                [
                    b"test.snapshot-operation.v1".as_slice(),
                    input_digest.as_str().as_bytes(),
                ]
                .concat()
            )
        ))
        .map_err(|_| AgentRuntimePortError::Protocol)?;
        Ok(CapabilityRequest {
            request_id: self.request_id.clone(),
            task_id: context.task_id.clone(),
            run_id: context.run_id.clone(),
            actor: context.actor.clone(),
            target: TargetRef {
                kind: BoundedName::new("local").unwrap(),
                authority: BoundedName::new("cosh").unwrap(),
                identifier: BoundedOpaque::new("workspace").unwrap(),
            },
            operation: OperationDescriptor {
                namespace: BoundedName::new("process").unwrap(),
                name: BoundedName::new("spawn").unwrap(),
                arguments_digest: input_digest.clone(),
            },
            operation_digest,
            requested_scope: CapabilityScope {
                resource: BoundedName::new("process").unwrap(),
                access: BoundedName::new("execute").unwrap(),
            },
            input_digest,
            expires_at_ms: u64::MAX,
        })
    }
}

impl AcpPermissionNormalizer for Normalizer {
    fn normalize(
        &mut self,
        _request: &AcpV1PermissionRequest,
        context: &AcpPermissionContext,
    ) -> Result<CapabilityRequest, AgentRuntimePortError> {
        Ok(CapabilityRequest {
            request_id: self.request_id.clone(),
            task_id: if self.mismatch {
                TaskId::new()
            } else {
                context.task_id.clone()
            },
            run_id: context.run_id.clone(),
            actor: context.actor.clone(),
            target: TargetRef {
                kind: BoundedName::new("local").unwrap(),
                authority: BoundedName::new("cosh").unwrap(),
                identifier: BoundedOpaque::new("workspace").unwrap(),
            },
            operation: OperationDescriptor {
                namespace: BoundedName::new("process").unwrap(),
                name: BoundedName::new("spawn").unwrap(),
                arguments_digest: digest('2'),
            },
            operation_digest: digest('3'),
            requested_scope: CapabilityScope {
                resource: BoundedName::new("process").unwrap(),
                access: BoundedName::new("execute").unwrap(),
            },
            input_digest: digest('4'),
            expires_at_ms: u64::MAX,
        })
    }
}

fn digest(character: char) -> Digest {
    Digest::parse(character.to_string().repeat(64)).unwrap()
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        scope_digest: digest('0'),
        display_name: Some(BoundedText::new("workspace").unwrap()),
    }
}

fn observed(observation: AcpV1Observation) -> AcpSessionEvent {
    // Driver-local ordering is intentionally independent from RuntimeEventEnvelope ordering.
    AcpSessionEvent::Observation(AcpSessionObservation::new(99, observation))
}

fn observed_tool_call(session_id: &str, tool_call_id: &str, title: &str) -> AcpSessionEvent {
    observed(AcpV1Observation::SessionUpdate {
        session_id: session_id.into(),
        update: json!({
            "sessionUpdate": "tool_call",
            "toolCallId": tool_call_id,
            "title": title,
            "kind": "execute",
            "status": "in_progress"
        }),
    })
}

fn codex_162_permission_fixture(tool_update: Option<serde_json::Value>) -> Vec<AcpSessionEvent> {
    codex_162_permission_fixture_with_carrier(
        tool_update,
        json!({
            "toolCallId": "codex-tool-7",
            "status": "pending"
        }),
    )
}

fn codex_162_permission_fixture_with_carrier(
    tool_update: Option<serde_json::Value>,
    tool_call: serde_json::Value,
) -> Vec<AcpSessionEvent> {
    codex_162_permission_fixture_with_options(
        tool_update,
        tool_call,
        json!([
            {
                "optionId": "allow_permissions_session",
                "name": "Allow for Session",
                "kind": "allow_always"
            },
            {
                "optionId": "allow_permissions_turn",
                "name": "Allow Once",
                "kind": "allow_once"
            },
            {
                "optionId": "reject_permissions",
                "name": "Reject",
                "kind": "reject_once"
            }
        ]),
    )
}

fn codex_162_permission_fixture_with_options(
    tool_update: Option<serde_json::Value>,
    tool_call: serde_json::Value,
    options: serde_json::Value,
) -> Vec<AcpSessionEvent> {
    let mut codec = AcpV1Codec::new(
        AcpV1ClientConfig::new("test", "1", 64 * 1024)
            .adapter_profile(AcpV1AdapterProfile::Codex162),
    )
    .unwrap();
    codec.initialize_frame().unwrap();
    let initialized = codec
        .decode_frame(
            json!({
                "jsonrpc": "2.0",
                "id": "cosh-acp-1",
                "result": {
                    "protocolVersion": 1,
                    "agentCapabilities": {},
                    "agentInfo": {
                        "name": "@agentclientprotocol/codex-acp",
                        "title": "Codex",
                        "version": "1.6.2"
                    },
                    "_meta": {
                        "jetbrains": {
                            "air": {
                                "version": 1,
                                "capabilities": [
                                    "sessionFailure",
                                    "agentFileChangeReport"
                                ]
                            }
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    codec
        .new_session_frame(Path::new("/workspace").to_path_buf(), Vec::new())
        .unwrap();
    let opened = codec
        .decode_frame(
            json!({
                "jsonrpc": "2.0",
                "id": "cosh-acp-2",
                "result": {"sessionId": "codex-session"}
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    codec.prompt_frame("inspect").unwrap();

    let mut events = vec![observed(initialized), observed(opened)];
    if let Some(update) = tool_update {
        let update = codec
            .decode_frame(
                json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {"sessionId": "codex-session", "update": update}
                })
                .to_string()
                .as_bytes(),
            )
            .unwrap();
        events.push(observed(update));
    }
    let permission = codec
        .decode_frame(
            json!({
                "jsonrpc": "2.0",
                "id": "codex-permission-7",
                "method": "session/request_permission",
                "params": {
                    "sessionId": "codex-session",
                    "toolCall": tool_call,
                    "options": options,
                    "_meta": {"codex": {"permission": "command"}}
                }
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    events.push(observed(permission));
    events
}

fn test_port(
    events: Vec<AcpSessionEvent>,
    normalizer: impl AcpPermissionNormalizer + 'static,
) -> (
    AcpAgentRuntime,
    Arc<Mutex<FakeState>>,
    AcpAgentRuntimeIdentity,
) {
    test_port_with_profile(events, normalizer, AcpV1AdapterProfile::Generic)
}

fn test_port_with_profile(
    events: Vec<AcpSessionEvent>,
    normalizer: impl AcpPermissionNormalizer + 'static,
    adapter_profile: AcpV1AdapterProfile,
) -> (
    AcpAgentRuntime,
    Arc<Mutex<FakeState>>,
    AcpAgentRuntimeIdentity,
) {
    let actor = ActorRef {
        actor_id: ActorId::new(),
        actor_kind: ActorKind::Human,
        issuer: BoundedName::new("local-os").unwrap(),
        assurance: AuthAssurance::LocalOs,
    };
    let identity = AcpAgentRuntimeIdentity {
        installation_id: InstallationId::new(),
        actor,
        task_id: TaskId::new(),
        run_id: RunId::new(),
        agent_session_id: AgentSessionId::new(),
        binding_id: RuntimeBindingId::new(),
        runtime_instance_id: RuntimeInstanceId::new(),
        runtime_generation: 9,
        adapter_authority: BoundedName::new("codex-acp").unwrap(),
        connection_scope_digest: digest('1'),
    };
    let mut launch = RuntimeLaunchSpec::new("/bin/false", Path::new("/"));
    launch.stdout_line_limit = 64 * 1024;
    let session = AcpSessionDriverConfig::new(
        launch,
        AcpV1ClientConfig::new("test", "1", 64 * 1024).adapter_profile(adapter_profile),
        "/workspace",
    );
    let config = AcpAgentRuntimeConfig {
        session,
        workspace: workspace(),
        identity: identity.clone(),
    };
    let state = Arc::new(Mutex::new(FakeState {
        events: events.into(),
        ..FakeState::default()
    }));
    let port = AcpAgentRuntime::with_backend(
        config,
        Box::new(normalizer),
        Box::new(FakeBackend(state.clone())),
    );
    (port, state, identity)
}

fn open(port: &mut AcpAgentRuntime, identity: &AcpAgentRuntimeIdentity) {
    port.dispatch(
        AgentRuntimeCommand::OpenSession {
            task_id: identity.task_id.clone(),
            run_id: identity.run_id.clone(),
            workspace: workspace(),
        },
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
}

fn prompt(port: &mut AcpAgentRuntime, identity: &AcpAgentRuntimeIdentity) -> TurnId {
    let turn_id = TurnId::new();
    port.dispatch(
        AgentRuntimeCommand::Prompt {
            run_id: identity.run_id.clone(),
            turn_id: turn_id.clone(),
            input: vec![ContentPart::Text {
                text: BoundedText::new("inspect").unwrap(),
            }],
        },
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    let started = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        started.event,
        AgentRuntimeEvent::TurnStarted { turn_id: observed } if observed == turn_id
    ));
    turn_id
}

#[test]
fn maps_bounded_text_and_exactly_one_terminal_without_provider_ids() {
    let events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "provider-secret-session".into(),
        }),
        observed(AcpV1Observation::SessionUpdate {
            session_id: "provider-secret-session".into(),
            update: json!({"sessionUpdate":"agent_message_chunk","messageId":"provider-message","content":{"type":"text","text":"hello"}}),
        }),
        observed(AcpV1Observation::PromptFinished {
            session_id: "provider-secret-session".into(),
            stop_reason: AcpV1StopReason::EndTurn,
        }),
    ];
    let (mut port, state, identity) = test_port(
        events,
        Normalizer {
            request_id: RequestId::new(),
            mismatch: false,
        },
    );
    open(&mut port, &identity);
    let opened = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(opened.sequence, 1);
    let turn_id = prompt(&mut port, &identity);
    let chunk = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(
        matches!(chunk.event, AgentRuntimeEvent::MessageChunk { content: ContentPart::Text { ref text }, .. } if text.as_str() == "hello")
    );
    let encoded = serde_json::to_string(&chunk).unwrap();
    assert!(!encoded.contains("provider-secret-session"));
    assert!(!encoded.contains("provider-message"));
    let terminal = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        terminal.event,
        AgentRuntimeEvent::Completed {
            turn_id: observed,
            outcome: TurnOutcome::Completed
        } if observed == turn_id
    ));
    assert!(!state.lock().unwrap().shutdown);
    assert_eq!(
        port.next_event(Instant::now() + Duration::from_millis(1)),
        Err(AgentRuntimePortError::InvalidState {
            operation: "next_event",
            state: "session-open"
        })
    );
}

#[test]
fn limit_result_keeps_session_open_for_another_turn() {
    let events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "session".into(),
        }),
        observed(AcpV1Observation::PromptFinished {
            session_id: "session".into(),
            stop_reason: AcpV1StopReason::MaxTokens,
        }),
    ];
    let (mut port, state, identity) = test_port(
        events,
        Normalizer {
            request_id: RequestId::new(),
            mismatch: false,
        },
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let first_turn = prompt(&mut port, &identity);
    let limited = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        limited.event,
        AgentRuntimeEvent::Completed {
            turn_id: observed,
            outcome: TurnOutcome::LimitReached {
                limit: TurnLimit::Tokens
            }
        } if observed == first_turn
    ));
    assert!(!state.lock().unwrap().shutdown);

    state
        .lock()
        .unwrap()
        .events
        .push_back(observed(AcpV1Observation::PromptFinished {
            session_id: "session".into(),
            stop_reason: AcpV1StopReason::MaxTurnRequests,
        }));
    let second_turn = prompt(&mut port, &identity);
    let completed = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        completed.event,
        AgentRuntimeEvent::Completed {
            turn_id: observed,
            outcome: TurnOutcome::LimitReached {
                limit: TurnLimit::Requests
            }
        } if observed == second_turn
    ));
}

#[test]
fn tool_updates_emit_stable_bounded_snapshots() {
    let events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "session".into(),
        }),
        observed(AcpV1Observation::SessionUpdate {
            session_id: "session".into(),
            update: json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "tool-1",
                "title": "Run tests",
                "kind": "execute"
            }),
        }),
        observed(AcpV1Observation::SessionUpdate {
            session_id: "session".into(),
            update: json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tool-1",
                "status": "completed",
                "rawOutput": {"exitCode": 0}
            }),
        }),
    ];
    let (mut port, _, identity) = test_port(
        events,
        Normalizer {
            request_id: RequestId::new(),
            mismatch: false,
        },
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let turn_id = prompt(&mut port, &identity);

    let created = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ToolInvocationUpdated { snapshot: created } = created.event else {
        panic!("expected initial tool snapshot");
    };
    assert_eq!(created.turn_id, turn_id);
    assert_eq!(created.revision, 1);
    assert_eq!(created.status, ToolInvocationStatus::Pending);
    assert_eq!(
        created.authority,
        ExecutionAuthority::ProviderNativeObserved
    );

    let updated = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ToolInvocationUpdated { snapshot: updated } = updated.event else {
        panic!("expected updated tool snapshot");
    };
    assert_eq!(updated.tool_use_id, created.tool_use_id);
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.status, ToolInvocationStatus::Completed);
}

#[test]
fn correlates_provider_native_approval_only_to_offered_allow_once() {
    let request_id = RequestId::new();
    let permission = AcpV1PermissionRequest {
        request_id: AcpV1RequestId::String("acp-request".into()),
        session_id: "session".into(),
        tool_call: json!({"toolCallId":"provider-tool","title":"run"}),
        callback_payload_digest: digest('5'),
        options: vec![
            AcpV1PermissionOption {
                option_id: "allow".into(),
                name: "Allow once".into(),
                kind: AcpV1PermissionOptionKind::AllowOnce,
            },
            AcpV1PermissionOption {
                option_id: "always".into(),
                name: "Always".into(),
                kind: AcpV1PermissionOptionKind::AllowAlways,
            },
        ],
    };
    let events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "session".into(),
        }),
        observed_tool_call("session", "provider-tool", "run"),
        observed(AcpV1Observation::PermissionRequested(permission)),
        observed(AcpV1Observation::SessionUpdate {
            session_id: "session".into(),
            update: json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "provider-tool",
                "status": "completed"
            }),
        }),
    ];
    let (mut port, state, identity) = test_port(
        events,
        Normalizer {
            request_id: request_id.clone(),
            mismatch: false,
        },
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let event = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        event.event,
        AgentRuntimeEvent::ExecutionPermissionRequested { ref request, .. }
            if request.request_id == request_id
    ));
    assert_eq!(
        port.dispatch(
            AgentRuntimeCommand::ResolvePermission {
                request_id: request_id.clone(),
                decision: RuntimePermissionDecision::RuntimeNativeAllowOnce,
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::Unsupported {
            operation: "runtime-native permission decision"
        })
    );
    assert!(state.lock().unwrap().answers.is_empty());
    port.dispatch(
        AgentRuntimeCommand::ResolvePermission {
            request_id,
            decision: RuntimePermissionDecision::ProviderNativeAllowOnce,
        },
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    let completed = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        completed.event,
        AgentRuntimeEvent::ToolInvocationUpdated { snapshot }
            if snapshot.status == ToolInvocationStatus::Completed
    ));
    assert_eq!(
        state.lock().unwrap().answers,
        vec![(
            AcpV1RequestId::String("acp-request".into()),
            AcpV1PermissionDecision::Selected {
                option_id: "allow".into()
            }
        )]
    );
}

#[test]
fn codex_permission_carrier_does_not_regress_an_in_progress_tool() {
    let request_id = RequestId::new();
    let events = codex_162_permission_fixture(Some(json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "codex-tool-7",
        "title": "Run cargo test",
        "kind": "execute",
        "status": "in_progress",
        "rawInput": {"command": "cargo test"}
    })));
    let (mut port, _, identity) = test_port(
        events,
        Normalizer {
            request_id: request_id.clone(),
            mismatch: false,
        },
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);
    let observed_tool = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ToolInvocationUpdated { snapshot } = observed_tool.event else {
        panic!("expected tool projection");
    };
    assert_eq!(snapshot.status, ToolInvocationStatus::InProgress);

    let permission = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let serialized = serde_json::to_string(&permission).unwrap();
    assert!(!serialized.contains("codex-session"));
    assert!(!serialized.contains("codex-permission-7"));
    assert!(!serialized.contains("codex-tool-7"));
    let AgentRuntimeEvent::ExecutionPermissionRequested {
        tool_use_id,
        summary,
        request,
        callback,
        ..
    } = permission.event
    else {
        panic!("expected permission callback");
    };
    assert_eq!(request.request_id, request_id);
    assert_eq!(tool_use_id, Some(snapshot.tool_use_id));
    assert_eq!(summary.summary.as_str(), "Run cargo test");
    assert_eq!(
        callback.normalized_operation_digest,
        request.operation_digest
    );
    let serialized = serde_json::to_string(&callback).unwrap();
    assert!(!serialized.contains("codex-session"));
    assert!(!serialized.contains("codex-permission-7"));
    assert!(!serialized.contains("codex-tool-7"));
    assert!(!serialized.contains("cargo test"));
}

#[test]
fn sparse_permission_without_a_tool_projection_fails_closed() {
    for carrier in [
        json!({"toolCallId": "codex-tool-7", "status": "pending"}),
        json!({
            "toolCallId": "codex-tool-7",
            "kind": "execute",
            "status": "pending",
            "rawInput": {"command": "cargo test", "cwd": "workspace"}
        }),
        json!({
            "toolCallId": "codex-tool-7",
            "kind": "execute",
            "status": "pending",
            "rawInput": {"command": "cargo test", "cwd": "/workspace", "shell": "bash"}
        }),
        json!({
            "toolCallId": "codex-tool-7",
            "kind": "execute",
            "status": "pending",
            "rawInput": {"command": "x".repeat(MAX_TEXT_BYTES), "cwd": "/workspace"}
        }),
        json!({
            "toolCallId": "codex-tool-7",
            "kind": "edit",
            "status": "pending"
        }),
    ] {
        let events = codex_162_permission_fixture_with_carrier(None, carrier);
        let (mut port, state, identity) = test_port_with_profile(
            events,
            Normalizer {
                request_id: RequestId::new(),
                mismatch: false,
            },
            AcpV1AdapterProfile::Codex162,
        );
        open(&mut port, &identity);
        port.next_event(Instant::now() + Duration::from_secs(1))
            .unwrap();
        prompt(&mut port, &identity);
        let permission = port
            .next_event(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            permission.event,
            AgentRuntimeEvent::TransportFailed { ref error }
                if error.code.as_str() == "acp_protocol_failed"
        ));
        assert!(state.lock().unwrap().cancelled);
    }
}

#[test]
fn codex_command_permission_derives_a_visible_title() {
    use sha2::Digest as _;

    let request_id = RequestId::new();
    let command = "sed -n '1,240p' full-access-proof.txt";
    let carrier = json!({
        "toolCallId": "codex-command-8",
        "kind": "execute",
        "status": "pending",
        "rawInput": {"command": command, "cwd": "/workspace"}
    });
    let events = codex_162_permission_fixture_with_options(
        None,
        carrier.clone(),
        json!([
            {"optionId": "allow_once", "name": "Allow Once", "kind": "allow_once"},
            {"optionId": "allow_always", "name": "Allow for Session", "kind": "allow_always"},
            {"optionId": "reject_once", "name": "Reject", "kind": "reject_once"}
        ]),
    );
    let (mut port, state, identity) = test_port_with_profile(
        events,
        SnapshotDigestNormalizer {
            request_id: request_id.clone(),
        },
        AcpV1AdapterProfile::Codex162,
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);

    let observed = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ToolInvocationUpdated { snapshot } = observed.event else {
        panic!("expected promoted command snapshot before permission");
    };
    assert_eq!(snapshot.summary.name.as_str(), "execute");
    assert!(snapshot.summary.summary.as_str().contains("/workspace"));
    assert!(snapshot.summary.summary.as_str().contains(command));

    let permission = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ExecutionPermissionRequested {
        tool_use_id,
        summary,
        request,
        callback,
        ..
    } = permission.event
    else {
        panic!("expected command permission request");
    };
    assert_eq!(tool_use_id, Some(snapshot.tool_use_id));
    assert!(summary.summary.as_str().contains("/workspace"));
    assert!(summary.summary.as_str().contains(command));
    let mut expected_carrier = carrier;
    expected_carrier["title"] = json!(format!("Run cwd=\"/workspace\", command=\"{command}\""));
    let canonical: agent_client_protocol::schema::v1::ToolCall =
        serde_json::from_value(expected_carrier).unwrap();
    assert_eq!(
        request.input_digest,
        Digest::parse(format!(
            "{:x}",
            sha2::Sha256::digest(serde_json::to_vec(&canonical).unwrap())
        ))
        .unwrap()
    );
    assert_eq!(
        callback.normalized_operation_digest,
        request.operation_digest
    );

    port.dispatch(
        AgentRuntimeCommand::ResolvePermission {
            request_id,
            decision: RuntimePermissionDecision::ProviderNativeAllowOnce,
        },
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        state.lock().unwrap().answers,
        vec![(
            AcpV1RequestId::String("codex-permission-7".into()),
            AcpV1PermissionDecision::Selected {
                option_id: "allow_once".into()
            }
        )]
    );
}

#[test]
fn codex_command_permission_refines_a_prior_read_projection() {
    use sha2::Digest as _;

    let request_id = RequestId::new();
    let command = "\"sed -n '1,240p' /workspace/full-access-proof.txt\"";
    let carrier = json!({
        "toolCallId": "exec-live-1",
        "kind": "execute",
        "status": "pending",
        "rawInput": {"command": command, "cwd": "/workspace"}
    });
    let events = codex_162_permission_fixture_with_options(
        Some(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "exec-live-1",
            "status": "in_progress",
            "kind": "read",
            "title": "Read file '/workspace/full-access-proof.txt'",
            "locations": [{"path": "/workspace/full-access-proof.txt"}]
        })),
        carrier.clone(),
        json!([
            {
                "optionId": "allow_once",
                "name": "Allow Once",
                "kind": "allow_once",
                "_meta": {"codex": {"decision": "accept"}}
            },
            {
                "optionId": "allow_always",
                "name": "Allow for Session",
                "kind": "allow_always",
                "_meta": {"codex": {"decision": "acceptForSession"}}
            },
            {
                "optionId": "accept_execpolicy_amendment",
                "name": "Allow Commands Starting With sed",
                "kind": "allow_always",
                "_meta": {
                    "permission": {"version": 1, "changes": []},
                    "codex": {
                        "decision": "acceptWithExecpolicyAmendment",
                        "execpolicyAmendment": ["sed"]
                    }
                }
            },
            {
                "optionId": "reject_once",
                "name": "Reject",
                "kind": "reject_once",
                "_meta": {"codex": {"decision": "decline"}}
            }
        ]),
    );
    let (mut port, state, identity) = test_port_with_profile(
        events,
        SnapshotDigestNormalizer {
            request_id: request_id.clone(),
        },
        AcpV1AdapterProfile::Codex162,
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);

    let initial = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ToolInvocationUpdated { snapshot: initial } = initial.event else {
        panic!("expected initial read projection");
    };
    assert_eq!(initial.summary.name.as_str(), "read");

    let refined = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ToolInvocationUpdated { snapshot: refined } = refined.event else {
        panic!("expected command permission refinement");
    };
    assert_eq!(refined.tool_use_id, initial.tool_use_id);
    assert_eq!(refined.revision, initial.revision + 1);
    assert_eq!(refined.summary.name.as_str(), "execute");
    let expected_title = format!(
        "Run cwd=\"/workspace\", command={}",
        serde_json::to_string(command).unwrap()
    );
    assert_eq!(refined.summary.summary.as_str(), expected_title);

    let permission = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ExecutionPermissionRequested {
        tool_use_id,
        summary,
        request,
        callback,
        ..
    } = permission.event
    else {
        panic!("expected refined permission request");
    };
    assert_eq!(tool_use_id, Some(initial.tool_use_id));
    assert_eq!(summary, refined.summary);
    let mut canonical = carrier;
    canonical["title"] = json!(expected_title);
    let canonical: agent_client_protocol::schema::v1::ToolCall =
        serde_json::from_value(canonical).unwrap();
    assert_eq!(
        request.input_digest,
        Digest::parse(format!(
            "{:x}",
            sha2::Sha256::digest(serde_json::to_vec(&canonical).unwrap())
        ))
        .unwrap()
    );
    assert_eq!(
        callback.normalized_operation_digest,
        request.operation_digest
    );

    port.dispatch(
        AgentRuntimeCommand::ResolvePermission {
            request_id,
            decision: RuntimePermissionDecision::ProviderNativeAllowOnce,
        },
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        state.lock().unwrap().answers,
        vec![(
            AcpV1RequestId::String("codex-permission-7".into()),
            AcpV1PermissionDecision::Selected {
                option_id: "allow_once".into()
            }
        )]
    );
}

#[test]
fn titleless_command_without_cwd_uses_a_command_only_title() {
    let carrier = json!({
        "toolCallId": "codex-command-no-cwd",
        "kind": "execute",
        "status": "pending",
        "rawInput": {"command": "pwd"}
    });
    let canonical = canonicalize_self_contained_permission_carrier(&carrier).unwrap();
    assert_eq!(canonical["title"], "Run command=\"pwd\"");
    assert_eq!(canonical["rawInput"], carrier["rawInput"]);
}

#[test]
fn command_titles_distinguish_newline_from_literal_backslash_n() {
    let title = |command: &str| {
        let carrier = json!({
            "toolCallId": "codex-command-collision",
            "kind": "execute",
            "status": "pending",
            "rawInput": {"command": command}
        });
        canonicalize_self_contained_permission_carrier(&carrier).unwrap()["title"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let newline = title("printf a\nprintf b");
    let literal = title("printf a\\nprintf b");
    assert_ne!(newline, literal);
    assert!(newline.contains("\\n"));
    assert!(literal.contains("\\\\n"));
}

#[test]
fn command_titles_distinguish_cwd_command_delimiter_shifts() {
    let title = |cwd: &str, command: &str| {
        let carrier = json!({
            "toolCallId": "codex-command-collision",
            "kind": "execute",
            "status": "pending",
            "rawInput": {"command": command, "cwd": cwd}
        });
        canonicalize_self_contained_permission_carrier(&carrier).unwrap()["title"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_ne!(title("/tmp: echo", "x"), title("/tmp", "echo: x"));
}

#[test]
fn codex_command_permission_escapes_controls_without_rewriting_input() {
    use sha2::Digest as _;

    let command = "printf first\nprintf\tsecond";
    let visible_title = "Run cwd=\"/workspace\", command=\"printf first\\nprintf\\tsecond\"";
    let carrier = json!({
        "toolCallId": "codex-command-controls",
        "kind": "execute",
        "status": "pending",
        "rawInput": {"command": command, "cwd": "/workspace"}
    });
    let events = codex_162_permission_fixture_with_options(
        None,
        carrier.clone(),
        json!([
            {"optionId": "allow_once", "name": "Allow Once", "kind": "allow_once"},
            {"optionId": "reject_once", "name": "Reject", "kind": "reject_once"}
        ]),
    );
    let (mut port, _, identity) = test_port_with_profile(
        events,
        SnapshotDigestNormalizer {
            request_id: RequestId::new(),
        },
        AcpV1AdapterProfile::Codex162,
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);

    let observed = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ToolInvocationUpdated { snapshot } = observed.event else {
        panic!("expected promoted command snapshot");
    };
    assert_eq!(snapshot.summary.summary.as_str(), visible_title);
    let permission = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ExecutionPermissionRequested { request, .. } = permission.event else {
        panic!("expected command permission");
    };

    let mut expected_carrier = carrier;
    expected_carrier["title"] = json!(visible_title);
    assert_eq!(expected_carrier["rawInput"]["command"], command);
    let canonical: agent_client_protocol::schema::v1::ToolCall =
        serde_json::from_value(expected_carrier).unwrap();
    assert_eq!(
        request.input_digest,
        Digest::parse(format!(
            "{:x}",
            sha2::Sha256::digest(serde_json::to_vec(&canonical).unwrap())
        ))
        .unwrap()
    );
}

#[test]
fn self_contained_permission_is_rejected_outside_codex_162() {
    let carrier = json!({
        "toolCallId": "codex-tool-7",
        "kind": "other",
        "status": "pending",
        "title": "Read the requested file",
        "rawInput": {"permissions": {"fileSystem": {"read": ["/workspace/proof.txt"]}}}
    });
    let events = codex_162_permission_fixture_with_carrier(None, carrier);
    let (mut port, state, identity) = test_port(
        events,
        Normalizer {
            request_id: RequestId::new(),
            mismatch: false,
        },
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);
    let failed = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        failed.event,
        AgentRuntimeEvent::TransportFailed { ref error }
            if error.code.as_str() == "acp_protocol_failed"
    ));
    assert!(state.lock().unwrap().cancelled);
}

#[test]
fn self_contained_permission_cannot_replace_a_buffered_tool() {
    let carrier = json!({
        "toolCallId": "codex-tool-7",
        "kind": "execute",
        "status": "pending",
        "rawInput": {"command": "sed -n '1,240p' proof.txt", "cwd": "/workspace"}
    });
    let mut events = codex_162_permission_fixture_with_carrier(None, carrier);
    events.insert(
        2,
        observed(AcpV1Observation::SessionUpdate {
            session_id: "codex-session".into(),
            update: json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "codex-tool-7",
                "rawInput": {"command": "rm /workspace/proof.txt"}
            }),
        }),
    );
    let (mut port, state, identity) = test_port_with_profile(
        events,
        Normalizer {
            request_id: RequestId::new(),
            mismatch: false,
        },
        AcpV1AdapterProfile::Codex162,
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);
    let failed = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        failed.event,
        AgentRuntimeEvent::TransportFailed { ref error }
            if error.code.as_str() == "acp_protocol_failed"
    ));
    assert!(state.lock().unwrap().cancelled);
}

#[test]
fn codex_permissions_request_is_self_contained_without_a_tool_update() {
    use sha2::Digest as _;

    let request_id = RequestId::new();
    let carrier = json!({
        "toolCallId": "codex-tool-7",
        "kind": "other",
        "status": "pending",
        "title": "Read the requested file",
        "rawInput": {
            "itemId": "codex-tool-7",
            "reason": "Read the requested file",
            "permissions": {
                "fileSystem": {
                    "read": ["/workspace/full-access-proof.txt"]
                }
            }
        },
        "content": [{
            "type": "content",
            "content": {
                "type": "text",
                "text": "Read the requested file\n\nFile System Read Access: /workspace/full-access-proof.txt"
            }
        }]
    });
    let events = codex_162_permission_fixture_with_carrier(None, carrier.clone());
    let (mut port, state, identity) = test_port_with_profile(
        events,
        SnapshotDigestNormalizer {
            request_id: request_id.clone(),
        },
        AcpV1AdapterProfile::Codex162,
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);

    let observed = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ToolInvocationUpdated { snapshot } = observed.event else {
        panic!("expected promoted tool snapshot before permission");
    };
    assert_eq!(snapshot.summary.name.as_str(), "agent_tool");
    assert_eq!(snapshot.summary.summary.as_str(), "Read the requested file");

    let permission = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::ExecutionPermissionRequested {
        tool_use_id,
        summary,
        request,
        callback,
        ..
    } = permission.event
    else {
        panic!("expected permission request");
    };
    assert_eq!(tool_use_id, Some(snapshot.tool_use_id));
    assert_eq!(summary.name.as_str(), "agent_tool");
    assert_eq!(summary.summary.as_str(), "Read the requested file");
    assert_eq!(request.request_id, request_id);
    let canonical: agent_client_protocol::schema::v1::ToolCall =
        serde_json::from_value(carrier).unwrap();
    assert_eq!(
        request.input_digest,
        Digest::parse(format!(
            "{:x}",
            sha2::Sha256::digest(serde_json::to_vec(&canonical).unwrap())
        ))
        .unwrap()
    );
    assert_eq!(
        callback.normalized_operation_digest,
        request.operation_digest
    );
    port.dispatch(
        AgentRuntimeCommand::ResolvePermission {
            request_id,
            decision: RuntimePermissionDecision::ProviderNativeAllowOnce,
        },
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        state.lock().unwrap().answers,
        vec![(
            AcpV1RequestId::String("codex-permission-7".into()),
            AcpV1PermissionDecision::Selected {
                option_id: "allow_permissions_turn".into()
            }
        )]
    );
    assert!(!state.lock().unwrap().cancelled);
}

#[test]
fn self_contained_permission_blocks_following_tool_mutation() {
    let carrier = json!({
        "toolCallId": "codex-tool-7",
        "kind": "other",
        "status": "pending",
        "title": "Read the requested file",
        "rawInput": {
            "permissions": {"fileSystem": {"read": ["/workspace/proof.txt"]}}
        }
    });
    let mut events = codex_162_permission_fixture_with_carrier(None, carrier);
    events.push(observed(AcpV1Observation::SessionUpdate {
        session_id: "codex-session".into(),
        update: json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "codex-tool-7",
            "title": "Delete the requested file",
            "rawInput": {"command": "rm /workspace/proof.txt"}
        }),
    }));
    let (mut port, state, identity) = test_port_with_profile(
        events,
        Normalizer {
            request_id: RequestId::new(),
            mismatch: false,
        },
        AcpV1AdapterProfile::Codex162,
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);
    let observed = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        observed.event,
        AgentRuntimeEvent::ToolInvocationUpdated { .. }
    ));
    let permission = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        permission.event,
        AgentRuntimeEvent::ExecutionPermissionRequested { .. }
    ));

    let failed = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        failed.event,
        AgentRuntimeEvent::TransportFailed { ref error }
            if error.code.as_str() == "acp_protocol_failed"
    ));
    assert!(state.lock().unwrap().cancelled);
}

#[test]
fn permission_carrier_cannot_override_the_approved_snapshot() {
    for conflicting_tool_call in [
        json!({
            "toolCallId": "tool",
            "status": "pending",
            "title": "Delete everything"
        }),
        json!({
            "toolCallId": "tool",
            "status": "pending",
            "rawInput": {"command": "rm -rf output"}
        }),
    ] {
        let permission = AcpV1PermissionRequest {
            request_id: AcpV1RequestId::Number(9),
            session_id: "session".into(),
            tool_call: conflicting_tool_call,
            callback_payload_digest: digest('5'),
            options: vec![AcpV1PermissionOption {
                option_id: "reject-once".into(),
                name: "Reject once".into(),
                kind: AcpV1PermissionOptionKind::RejectOnce,
            }],
        };
        let events = vec![
            observed(AcpV1Observation::Initialized {
                agent_info: None,
                capabilities: Default::default(),
            }),
            observed(AcpV1Observation::SessionOpened {
                session_id: "session".into(),
            }),
            observed(AcpV1Observation::SessionUpdate {
                session_id: "session".into(),
                update: json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tool",
                    "title": "Run tests",
                    "kind": "execute",
                    "status": "in_progress",
                    "rawInput": {"command": "cargo test"}
                }),
            }),
            observed(AcpV1Observation::PermissionRequested(permission)),
        ];
        let (mut port, state, identity) = test_port(
            events,
            Normalizer {
                request_id: RequestId::new(),
                mismatch: false,
            },
        );
        open(&mut port, &identity);
        port.next_event(Instant::now() + Duration::from_secs(1))
            .unwrap();
        prompt(&mut port, &identity);
        port.next_event(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let failed = port
            .next_event(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            failed.event,
            AgentRuntimeEvent::TransportFailed { ref error }
                if error.code.as_str() == "acp_protocol_failed"
        ));
        assert!(state.lock().unwrap().cancelled);
    }
}

#[test]
fn tool_updates_cannot_mutate_a_snapshot_while_permission_is_pending() {
    let request_id = RequestId::new();
    let permission = AcpV1PermissionRequest {
        request_id: AcpV1RequestId::Number(9),
        session_id: "session".into(),
        tool_call: json!({"toolCallId": "tool", "status": "pending"}),
        callback_payload_digest: digest('5'),
        options: vec![AcpV1PermissionOption {
            option_id: "allow-once".into(),
            name: "Allow once".into(),
            kind: AcpV1PermissionOptionKind::AllowOnce,
        }],
    };
    let events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "session".into(),
        }),
        observed(AcpV1Observation::SessionUpdate {
            session_id: "session".into(),
            update: json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "tool",
                "title": "Run tests",
                "kind": "execute",
                "status": "in_progress",
                "rawInput": {"command": "cargo test"}
            }),
        }),
        observed(AcpV1Observation::PermissionRequested(permission)),
        observed(AcpV1Observation::SessionUpdate {
            session_id: "session".into(),
            update: json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "tool",
                "title": "Delete output",
                "rawInput": {"command": "rm -rf output"}
            }),
        }),
    ];
    let (mut port, state, identity) = test_port(
        events,
        Normalizer {
            request_id: request_id.clone(),
            mismatch: false,
        },
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let permission = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        permission.event,
        AgentRuntimeEvent::ExecutionPermissionRequested { ref request, .. }
            if request.request_id == request_id
    ));

    let failed = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        failed.event,
        AgentRuntimeEvent::TransportFailed { ref error }
            if error.code.as_str() == "acp_protocol_failed"
    ));
    assert!(state.lock().unwrap().cancelled);
}

#[test]
fn durable_operation_identity_is_derived_from_the_displayed_snapshot() {
    let run = |title: &str, command: &str| {
        let events = codex_162_permission_fixture(Some(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "codex-tool-7",
            "title": title,
            "kind": "execute",
            "status": "in_progress",
            "rawInput": {"command": command}
        })));
        let (mut port, _, identity) = test_port(
            events,
            SnapshotDigestNormalizer {
                request_id: RequestId::new(),
            },
        );
        open(&mut port, &identity);
        port.next_event(Instant::now() + Duration::from_secs(1))
            .unwrap();
        prompt(&mut port, &identity);
        port.next_event(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let permission = port
            .next_event(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let AgentRuntimeEvent::ExecutionPermissionRequested {
            summary,
            request,
            callback,
            ..
        } = permission.event
        else {
            panic!("expected permission callback");
        };
        (summary, request, callback)
    };

    let (first_summary, first, first_callback) = run("Run tests", "cargo test");
    let (second_summary, second, second_callback) = run("Delete output", "rm output");
    assert_eq!(first_summary.summary.as_str(), "Run tests");
    assert_eq!(second_summary.summary.as_str(), "Delete output");
    assert_ne!(first.input_digest, second.input_digest);
    assert_ne!(first.operation_digest, second.operation_digest);
    assert_eq!(
        first.operation_digest,
        first_callback.normalized_operation_digest
    );
    assert_eq!(
        second.operation_digest,
        second_callback.normalized_operation_digest
    );
    // Both ports received the exact same sparse provider callback; only the
    // previously observed, user-visible snapshot differed.
    assert_eq!(
        first_callback.callback_payload_digest,
        second_callback.callback_payload_digest
    );
}

#[test]
fn callback_binding_preserves_request_type_and_option_order() {
    let (_, _, identity) = test_port(
        Vec::new(),
        Normalizer {
            request_id: RequestId::new(),
            mismatch: false,
        },
    );
    let context = AcpPermissionContext {
        actor: identity.actor,
        task_id: identity.task_id,
        run_id: identity.run_id,
    };
    let mut request = AcpV1PermissionRequest {
        request_id: AcpV1RequestId::Number(1),
        session_id: "session".into(),
        tool_call: json!({"toolCallId":"tool","status":"pending"}),
        callback_payload_digest: digest('5'),
        options: vec![
            AcpV1PermissionOption {
                option_id: "allow".into(),
                name: "Allow once".into(),
                kind: AcpV1PermissionOptionKind::AllowOnce,
            },
            AcpV1PermissionOption {
                option_id: "reject".into(),
                name: "Reject once".into(),
                kind: AcpV1PermissionOptionKind::RejectOnce,
            },
        ],
    };
    let mut normalizer = Normalizer {
        request_id: RequestId::new(),
        mismatch: false,
    };
    let normalized = normalizer.normalize(&request, &context).unwrap();
    let numeric = provider_permission_callback(&request, &normalized).unwrap();

    request.request_id = AcpV1RequestId::String("1".into());
    let string = provider_permission_callback(&request, &normalized).unwrap();
    assert_ne!(
        numeric.provider_request_id_digest,
        string.provider_request_id_digest
    );

    request.options.reverse();
    let reversed = provider_permission_callback(&request, &normalized).unwrap();
    assert_ne!(
        string.ordered_option_set_digest,
        reversed.ordered_option_set_digest
    );
}

#[test]
fn cancelled_permission_callbacks_are_correlated_before_turn_completion() {
    let request_id = RequestId::new();
    let permission = AcpV1PermissionRequest {
        request_id: AcpV1RequestId::Number(9),
        session_id: "session".into(),
        tool_call: json!({"toolCallId":"tool-9","status":"pending"}),
        callback_payload_digest: digest('5'),
        options: vec![AcpV1PermissionOption {
            option_id: "reject-once".into(),
            name: "Reject once".into(),
            kind: AcpV1PermissionOptionKind::RejectOnce,
        }],
    };
    let events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "session".into(),
        }),
        observed_tool_call("session", "tool-9", "run"),
        observed(AcpV1Observation::PermissionRequested(permission)),
        observed(AcpV1Observation::PromptCancelledWithPendingPermissions {
            session_id: "session".into(),
            request_ids: vec![AcpV1RequestId::Number(9)],
        }),
    ];
    let (mut port, _, identity) = test_port(
        events,
        Normalizer {
            request_id: request_id.clone(),
            mismatch: false,
        },
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let turn_id = prompt(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();

    let abandoned = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        abandoned.event,
        AgentRuntimeEvent::ExecutionPermissionsAbandoned {
            turn_id: observed_turn,
            request_ids,
        } if observed_turn == turn_id && request_ids == vec![request_id]
    ));
    let cancelled = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(cancelled.sequence, abandoned.sequence + 1);
    assert!(matches!(
        cancelled.event,
        AgentRuntimeEvent::Completed {
            turn_id: observed_turn,
            outcome: TurnOutcome::Cancelled,
        } if observed_turn == turn_id
    ));
}

#[test]
fn rejects_normalizer_identity_substitution_and_settles_transport() {
    let permission = AcpV1PermissionRequest {
        request_id: AcpV1RequestId::Number(7),
        session_id: "session".into(),
        tool_call: json!({"toolCallId":"tool","title":"run"}),
        callback_payload_digest: digest('5'),
        options: vec![AcpV1PermissionOption {
            option_id: "reject".into(),
            name: "Reject once".into(),
            kind: AcpV1PermissionOptionKind::RejectOnce,
        }],
    };
    let events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "session".into(),
        }),
        observed_tool_call("session", "tool", "run"),
        observed(AcpV1Observation::PermissionRequested(permission)),
    ];
    let (mut port, state, identity) = test_port(
        events,
        Normalizer {
            request_id: RequestId::new(),
            mismatch: true,
        },
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let event = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        event.event,
        AgentRuntimeEvent::TransportFailed { .. }
    ));
    assert!(state.lock().unwrap().cancelled);
}

#[test]
fn brokered_takeover_is_rejected_without_answering_provider() {
    let request_id = RequestId::new();
    let permission = AcpV1PermissionRequest {
        request_id: AcpV1RequestId::String("permission".into()),
        session_id: "session".into(),
        tool_call: json!({"toolCallId":"tool","title":"run"}),
        callback_payload_digest: digest('5'),
        options: vec![AcpV1PermissionOption {
            option_id: "always".into(),
            name: "Always".into(),
            kind: AcpV1PermissionOptionKind::AllowAlways,
        }],
    };
    let events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "session".into(),
        }),
        observed_tool_call("session", "tool", "run"),
        observed(AcpV1Observation::PermissionRequested(permission)),
    ];
    let (mut port, state, identity) = test_port(
        events,
        Normalizer {
            request_id: request_id.clone(),
            mismatch: false,
        },
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();

    let result = port.dispatch(
        AgentRuntimeCommand::AcknowledgeBrokeredRequest {
            acknowledgement: BrokeredRequestAcknowledgement {
                request_id,
                approval_id: ApprovalId::new(),
            },
        },
        Instant::now() + Duration::from_secs(1),
    );
    assert_eq!(
        result,
        Err(AgentRuntimePortError::Unsupported {
            operation: "COSH-brokered execution over ACP"
        })
    );
    assert!(state.lock().unwrap().answers.is_empty());
}

#[test]
fn cancellation_waits_for_terminal_before_public_completion() {
    let events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "session".into(),
        }),
    ];
    let (mut port, state, identity) = test_port(
        events,
        Normalizer {
            request_id: RequestId::new(),
            mismatch: false,
        },
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let turn_id = prompt(&mut port, &identity);

    port.dispatch(
        AgentRuntimeCommand::Cancel {
            run_id: identity.run_id,
            turn_id: turn_id.clone(),
            cause: cosh_gateway_contracts::task::CancelReason::UserRequested,
        },
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    assert!(state.lock().unwrap().cancelled);
    let terminal = port
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
        port.next_event(Instant::now() + Duration::from_millis(1)),
        Err(AgentRuntimePortError::Terminal)
    );
}

#[test]
fn disconnected_backend_emits_one_failed_terminal_never_success() {
    let events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "session".into(),
        }),
    ];
    let (mut port, state, identity) = test_port(
        events,
        Normalizer {
            request_id: RequestId::new(),
            mismatch: false,
        },
    );
    open(&mut port, &identity);
    port.next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    prompt(&mut port, &identity);
    state.lock().unwrap().disconnected = true;

    let terminal = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        terminal.event,
        AgentRuntimeEvent::TransportFailed { ref error }
            if error.code.as_str() == "acp_transport_failed"
    ));
    assert_eq!(
        port.next_event(Instant::now() + Duration::from_millis(1)),
        Err(AgentRuntimePortError::Terminal)
    );
}

#[test]
fn completion_and_cancel_races_settle_once_and_late_permission_cannot_answer() {
    let completed_events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "session".into(),
        }),
        observed(AcpV1Observation::PromptFinished {
            session_id: "session".into(),
            stop_reason: AcpV1StopReason::EndTurn,
        }),
    ];
    let (mut completed, _, completed_identity) = test_port(
        completed_events,
        Normalizer {
            request_id: RequestId::new(),
            mismatch: false,
        },
    );
    open(&mut completed, &completed_identity);
    completed
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let completed_turn = prompt(&mut completed, &completed_identity);
    let terminal = completed
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        terminal.event,
        AgentRuntimeEvent::Completed {
            turn_id,
            outcome: TurnOutcome::Completed
        } if turn_id == completed_turn
    ));
    assert!(matches!(
        completed.dispatch(
            AgentRuntimeCommand::Cancel {
                run_id: completed_identity.run_id,
                turn_id: completed_turn,
                cause: cosh_gateway_contracts::task::CancelReason::UserRequested,
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::InvalidState { .. })
    ));

    let request_id = RequestId::new();
    let permission = AcpV1PermissionRequest {
        request_id: AcpV1RequestId::Number(91),
        session_id: "session".into(),
        tool_call: json!({"toolCallId":"tool","title":"run"}),
        callback_payload_digest: digest('5'),
        options: vec![AcpV1PermissionOption {
            option_id: "allow".into(),
            name: "Allow once".into(),
            kind: AcpV1PermissionOptionKind::AllowOnce,
        }],
    };
    let cancelled_events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "session".into(),
        }),
        observed(AcpV1Observation::PermissionRequested(permission)),
    ];
    let (mut cancelled, state, cancelled_identity) = test_port(
        cancelled_events,
        Normalizer {
            request_id: request_id.clone(),
            mismatch: false,
        },
    );
    open(&mut cancelled, &cancelled_identity);
    cancelled
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let cancelled_turn = prompt(&mut cancelled, &cancelled_identity);
    cancelled
        .dispatch(
            AgentRuntimeCommand::Cancel {
                run_id: cancelled_identity.run_id,
                turn_id: cancelled_turn.clone(),
                cause: cosh_gateway_contracts::task::CancelReason::UserRequested,
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
    assert!(matches!(
        cancelled.dispatch(
            AgentRuntimeCommand::ResolvePermission {
                request_id,
                decision: RuntimePermissionDecision::ProviderNativeAllowOnce,
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::InvalidState { .. })
    ));
    assert!(state.lock().unwrap().answers.is_empty());
    let terminal = cancelled
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        terminal.event,
        AgentRuntimeEvent::Completed {
            turn_id,
            outcome: TurnOutcome::Cancelled
        } if turn_id == cancelled_turn
    ));
    assert_eq!(
        cancelled.next_event(Instant::now() + Duration::from_millis(1)),
        Err(AgentRuntimePortError::Terminal)
    );
}

#[test]
fn unsupported_resume_rich_content_and_second_session_do_not_reach_backend() {
    let events = vec![
        observed(AcpV1Observation::Initialized {
            agent_info: None,
            capabilities: Default::default(),
        }),
        observed(AcpV1Observation::SessionOpened {
            session_id: "session".into(),
        }),
    ];
    let (mut port, state, identity) = test_port(
        events,
        Normalizer {
            request_id: RequestId::new(),
            mismatch: false,
        },
    );
    open(&mut port, &identity);
    let opened = port
        .next_event(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let AgentRuntimeEvent::SessionOpened { binding } = opened.event else {
        panic!("expected opened session")
    };

    assert_eq!(
        port.dispatch(
            AgentRuntimeCommand::ResumeSession {
                task_id: identity.task_id.clone(),
                run_id: identity.run_id.clone(),
                binding,
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::Unsupported {
            operation: "resume_session"
        })
    );
    assert!(matches!(
        port.dispatch(
            AgentRuntimeCommand::OpenSession {
                task_id: identity.task_id.clone(),
                run_id: identity.run_id.clone(),
                workspace: workspace(),
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::InvalidState { .. })
    ));
    assert_eq!(
        port.dispatch(
            AgentRuntimeCommand::ResolveInput {
                request_id: InputRequestId::new(),
                run_id: identity.run_id.clone(),
                turn_id: TurnId::new(),
                response: RuntimeInputResponse::Text {
                    text: BoundedText::new("must stay local").unwrap(),
                },
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::Unsupported {
            operation: "resolve_input"
        })
    );
    assert_eq!(
        port.dispatch(
            AgentRuntimeCommand::Prompt {
                run_id: identity.run_id,
                turn_id: TurnId::new(),
                input: vec![ContentPart::ResourceLink {
                    uri: BoundedOpaque::new("file:///forbidden").unwrap(),
                    label: None,
                }],
            },
            Instant::now() + Duration::from_secs(1),
        ),
        Err(AgentRuntimePortError::Unsupported {
            operation: "resource prompt"
        })
    );
    assert!(state.lock().unwrap().prompts.is_empty());
}
