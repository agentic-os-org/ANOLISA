use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cosh_gateway_contracts::{
    capability::{
        BrokeredOperation, CapabilityRequest, CapabilityScope, DenialCode, OperationDescriptor,
        WorkspaceCheckpointCreateV1,
    },
    common::{
        ActorKind, ActorRef, AuthAssurance, BoundedName, BoundedOpaque, BoundedText,
        ContractHeader, ContractSchema, Correlation, Digest, RuntimeSelector, TargetRef,
    },
    error::ErrorCategory,
    external::{ExternalRef, ExternalRefKind},
    ids::{
        ActorId, AgentSessionId, ApprovalId, CheckpointId, InputRequestId, InstallationId,
        MessageId, RequestId, RunId, RuntimeBindingId, RuntimeInstanceId, TaskId, ToolUseId,
    },
    runtime::{
        AgentRuntimeCommand, AgentRuntimeEvent, BrokeredExecutionDelivery,
        BrokeredExecutionOutcome, BrokeredRequestAcknowledgement, RuntimeEventEnvelope,
        RuntimeInputRequest, RuntimeInputResponse, TurnLimit, TurnOutcome,
    },
    task::RuntimeUpdate,
};

use super::*;

struct FakePortFactory {
    port: Option<FakePort>,
    workspace: WorkspaceRef,
}

impl AgentRuntimePortFactory for FakePortFactory {
    fn create(&mut self, _run: &ScheduledRun) -> Result<ScheduledRuntimePort, ContractError> {
        Ok(ScheduledRuntimePort::new(
            Box::new(self.port.take().expect("one scheduled Runtime")),
            self.workspace.clone(),
        ))
    }
}

struct FakePort {
    binding_id: RuntimeBindingId,
    events: VecDeque<RuntimeEventEnvelope>,
    commands: Arc<Mutex<Vec<AgentRuntimeCommand>>>,
}

impl AgentRuntimePort for FakePort {
    fn binding_id(&self) -> &RuntimeBindingId {
        &self.binding_id
    }

    fn dispatch(
        &mut self,
        command: AgentRuntimeCommand,
        _deadline: Instant,
    ) -> Result<(), AgentRuntimePortError> {
        if let AgentRuntimeCommand::Prompt { turn_id, .. } = &command {
            for envelope in &mut self.events {
                match &mut envelope.event {
                    AgentRuntimeEvent::TurnStarted { turn_id: observed }
                    | AgentRuntimeEvent::ExecutionPermissionRequested {
                        turn_id: observed, ..
                    }
                    | AgentRuntimeEvent::CoreExecutionPermissionRequested {
                        turn_id: observed,
                        ..
                    }
                    | AgentRuntimeEvent::ExecutionPermissionsAbandoned {
                        turn_id: observed, ..
                    }
                    | AgentRuntimeEvent::Completed {
                        turn_id: observed, ..
                    } => *observed = turn_id.clone(),
                    _ => {}
                }
            }
        }
        self.commands.lock().expect("command log").push(command);
        Ok(())
    }

    fn next_event(
        &mut self,
        deadline: Instant,
    ) -> Result<RuntimeEventEnvelope, AgentRuntimePortError> {
        if deadline <= Instant::now() {
            return Err(AgentRuntimePortError::Deadline {
                operation: "next_event",
            });
        }
        self.events
            .pop_front()
            .ok_or(AgentRuntimePortError::Deadline {
                operation: "next_event",
            })
    }
}

struct Fixture {
    run: ScheduledRun,
    workspace: WorkspaceRef,
    binding: RuntimeBindingRef,
    installation_id: InstallationId,
    commands: Arc<Mutex<Vec<AgentRuntimeCommand>>>,
}

impl Fixture {
    fn new() -> Self {
        let task_id = TaskId::new();
        let run_id = RunId::new();
        let binding_id = RuntimeBindingId::new();
        let installation_id = InstallationId::new();
        let workspace = WorkspaceRef {
            scope_digest: digest('w'),
            display_name: Some(BoundedText::new("test workspace").unwrap()),
        };
        let binding = RuntimeBindingRef {
            binding_id,
            task_id: task_id.clone(),
            run_id: run_id.clone(),
            agent_session_id: AgentSessionId::new(),
            runtime_instance_id: RuntimeInstanceId::new(),
            runtime_generation: 1,
            external_session: ExternalRef {
                kind: ExternalRefKind::AcpSession,
                authority: BoundedName::new("test-adapter").unwrap(),
                scope_digest: digest('s'),
                value: BoundedOpaque::new("session-hash").unwrap(),
            },
        };
        Self {
            run: ScheduledRun {
                actor: ActorRef {
                    actor_id: ActorId::new(),
                    actor_kind: ActorKind::Human,
                    issuer: BoundedName::new("local-os").unwrap(),
                    assurance: AuthAssurance::LocalOs,
                },
                task_id,
                run_id,
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("acp").unwrap(),
                    profile: Some(BoundedName::new("codex").unwrap()),
                },
                intent: BoundedText::new("inspect the workspace").unwrap(),
                target: TargetRef {
                    kind: BoundedName::new("local").unwrap(),
                    authority: BoundedName::new("test").unwrap(),
                    identifier: BoundedOpaque::new("host").unwrap(),
                },
                workspace: workspace.clone(),
                capability_profile:
                    cosh_gateway_contracts::profile::GatewayCapabilityProfile::task_only_v1()
                        .identity(),
                launch: cosh_gateway_contracts::task::TaskLaunchSpecV1::new(
                    BoundedText::new("inspect the workspace").unwrap(),
                    cosh_gateway_contracts::task::TaskRuntime::Codex,
                    workspace.clone(),
                    cosh_gateway_contracts::task::CheckpointPolicy::Off,
                    cosh_gateway_contracts::task::ApprovalPolicy::Interactive,
                ),
                lease_generation: 1,
            },
            workspace,
            binding,
            installation_id,
            commands: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn event(&self, sequence: u64, event: AgentRuntimeEvent) -> RuntimeEventEnvelope {
        let mut correlation = Correlation::new(self.installation_id.clone());
        correlation.actor_id = Some(self.run.actor.actor_id.clone());
        correlation.task_id = Some(self.run.task_id.clone());
        correlation.run_id = Some(self.run.run_id.clone());
        correlation.runtime_binding_id = Some(self.binding.binding_id.clone());
        RuntimeEventEnvelope {
            header: ContractHeader::new(
                ContractSchema::RuntimeEvent,
                MessageId::new(),
                sequence,
                correlation,
            ),
            binding_id: self.binding.binding_id.clone(),
            sequence,
            event,
        }
    }

    fn factory(
        &self,
        events: impl IntoIterator<Item = RuntimeEventEnvelope>,
    ) -> ScheduledAgentRuntimeFactory<FakePortFactory> {
        ScheduledAgentRuntimeFactory::new(FakePortFactory {
            port: Some(FakePort {
                binding_id: self.binding.binding_id.clone(),
                events: events.into_iter().collect(),
                commands: Arc::clone(&self.commands),
            }),
            workspace: self.workspace.clone(),
        })
    }

    fn session_opened(&self) -> RuntimeEventEnvelope {
        self.event(
            1,
            AgentRuntimeEvent::SessionOpened {
                binding: self.binding.clone(),
            },
        )
    }
}

#[test]
fn scheduled_runtime_opens_prompts_streams_and_closes_in_order() {
    let fixture = Fixture::new();
    let commands = Arc::clone(&fixture.commands);
    let scripted = Arc::new(Mutex::new(VecDeque::from([fixture.session_opened()])));
    let port = TurnAwarePort::new(
        fixture.binding.binding_id.clone(),
        Arc::clone(&commands),
        scripted,
        TurnOutcome::Completed,
        None,
        Some(BoundedText::new("working").unwrap()),
    );
    let mut factory = ScheduledAgentRuntimeFactory::new(SinglePortFactory {
        port: Some(Box::new(port)),
        workspace: fixture.workspace.clone(),
    });
    let mut started = factory.open(&fixture.run).unwrap();

    assert!(matches!(
        commands.lock().unwrap().as_slice(),
        [AgentRuntimeCommand::OpenSession { .. }]
    ));
    started.handle.begin().unwrap();
    assert!(matches!(
        commands.lock().unwrap().as_slice(),
        [
            AgentRuntimeCommand::OpenSession { .. },
            AgentRuntimeCommand::Prompt { .. }
        ]
    ));
    assert_eq!(started.handle.poll(), RuntimePoll::Observed { sequence: 2 });
    assert_eq!(
        started.handle.poll(),
        RuntimePoll::Update {
            sequence: 3,
            update: RuntimeUpdate::Progress {
                summary: BoundedText::new("working").unwrap(),
            },
        }
    );
    assert_eq!(started.handle.poll(), RuntimePoll::Succeeded);
    assert!(matches!(
        commands.lock().unwrap().last(),
        Some(AgentRuntimeCommand::Close { .. })
    ));
}

#[test]
fn turn_limit_refusal_and_failure_never_map_to_success() {
    for (outcome, expected_code) in [
        (
            TurnOutcome::LimitReached {
                limit: TurnLimit::Tokens,
            },
            "runtime_turn_limit_reached",
        ),
        (TurnOutcome::Refused, "runtime_turn_refused"),
        (
            TurnOutcome::Failed {
                error: ContractError::new(
                    "provider_failed",
                    ErrorCategory::RuntimeUnavailable,
                    false,
                    "The provider failed",
                )
                .unwrap(),
            },
            "provider_failed",
        ),
        (TurnOutcome::Cancelled, "runtime_turn_cancelled_unsolicited"),
    ] {
        let fixture = Fixture::new();
        let commands = Arc::clone(&fixture.commands);
        let scripted = Arc::new(Mutex::new(VecDeque::from([fixture.session_opened()])));
        let port = TurnAwarePort::new(
            fixture.binding.binding_id.clone(),
            Arc::clone(&commands),
            Arc::clone(&scripted),
            outcome,
            None,
            None,
        );
        let mut factory = ScheduledAgentRuntimeFactory::new(SinglePortFactory {
            port: Some(Box::new(port)),
            workspace: fixture.workspace.clone(),
        });
        let mut started = factory.open(&fixture.run).unwrap();
        started.handle.begin().unwrap();
        assert_eq!(started.handle.poll(), RuntimePoll::Observed { sequence: 2 });
        let RuntimePoll::Failed(error) = started.handle.poll() else {
            panic!("non-completed turn must fail the scheduled Run")
        };
        assert_eq!(error.code.as_str(), expected_code);
        assert!(matches!(
            commands.lock().unwrap().last(),
            Some(AgentRuntimeCommand::Close { .. })
        ));
    }
}

#[test]
fn provider_permission_pauses_polling_until_allow_once_is_dispatched() {
    let fixture = Fixture::new();
    let request = capability_request(&fixture);
    let commands = Arc::clone(&fixture.commands);
    let scripted = Arc::new(Mutex::new(VecDeque::from([fixture.session_opened()])));
    let port = TurnAwarePort::new(
        fixture.binding.binding_id.clone(),
        Arc::clone(&commands),
        scripted,
        TurnOutcome::Completed,
        Some(request.clone()),
        None,
    );
    let mut factory = ScheduledAgentRuntimeFactory::new(SinglePortFactory {
        port: Some(Box::new(port)),
        workspace: fixture.workspace.clone(),
    });
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();

    assert_eq!(started.handle.poll(), RuntimePoll::Observed { sequence: 2 });
    let RuntimePoll::PermissionRequested {
        permission,
        request: observed,
        summary,
    } = started.handle.poll()
    else {
        panic!("provider permission must be returned to the scheduler")
    };
    assert_eq!(*observed, request);
    assert_eq!(summary.summary.as_str(), "Run the inspected shell command");
    assert_eq!(started.handle.poll(), RuntimePoll::Pending);
    started
        .handle
        .resolve_provider_permission(
            &permission,
            RuntimePermissionDecision::ProviderNativeAllowOnce,
        )
        .unwrap();
    assert!(commands.lock().unwrap().iter().any(|command| matches!(
        command,
        AgentRuntimeCommand::ResolvePermission {
            decision: RuntimePermissionDecision::ProviderNativeAllowOnce,
            ..
        }
    )));
    assert_eq!(started.handle.poll(), RuntimePoll::Succeeded);
}

#[test]
fn core_permission_uses_only_the_runtime_native_callback_fence() {
    let fixture = Fixture::new();
    let request = capability_request(&fixture);
    let tool_use_id = ToolUseId::new();
    let events = VecDeque::from([
        fixture.session_opened(),
        fixture.event(
            2,
            AgentRuntimeEvent::TurnStarted {
                turn_id: TurnId::new(),
            },
        ),
        fixture.event(
            3,
            AgentRuntimeEvent::CoreExecutionPermissionRequested {
                turn_id: TurnId::new(),
                tool_use_id: tool_use_id.clone(),
                summary: cosh_gateway_contracts::runtime::ToolSummary {
                    name: BoundedName::new("write_file").unwrap(),
                    summary: BoundedText::new("Write one workspace file").unwrap(),
                },
                callback: cosh_gateway_contracts::runtime::CorePermissionCallbackV1 {
                    private_request_id_digest: digest('r'),
                    private_tool_use_id_digest: digest('t'),
                    callback_payload_digest: digest('c'),
                    normalized_operation_digest: request.operation_digest.clone(),
                },
                request: request.clone(),
            },
        ),
        fixture.event(
            4,
            AgentRuntimeEvent::Completed {
                turn_id: TurnId::new(),
                outcome: TurnOutcome::Completed,
            },
        ),
    ]);
    let mut factory = ScheduledAgentRuntimeFactory::new(FakePortFactory {
        port: Some(FakePort {
            binding_id: fixture.binding.binding_id.clone(),
            events,
            commands: Arc::clone(&fixture.commands),
        }),
        workspace: fixture.workspace.clone(),
    });
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();
    assert_eq!(started.handle.poll(), RuntimePoll::Observed { sequence: 2 });
    let RuntimePoll::PermissionRequested {
        permission,
        request: observed,
        ..
    } = started.handle.poll()
    else {
        panic!("Core permission must be returned to the scheduler")
    };
    assert_eq!(*observed, request);
    assert_eq!(permission.tool_use_id, Some(tool_use_id));
    assert!(permission.callback.is_none());
    assert!(permission.core_callback.is_some());
    assert_eq!(started.handle.poll(), RuntimePoll::Pending);
    started
        .handle
        .resolve_provider_permission(
            &permission,
            RuntimePermissionDecision::RuntimeNativeAllowOnce,
        )
        .unwrap();
    assert!(fixture
        .commands
        .lock()
        .unwrap()
        .iter()
        .any(|command| matches!(
            command,
            AgentRuntimeCommand::ResolvePermission {
                decision: RuntimePermissionDecision::RuntimeNativeAllowOnce,
                ..
            }
        )));
    assert_eq!(started.handle.poll(), RuntimePoll::Succeeded);
}

#[test]
fn denied_provider_permission_makes_cancelled_terminal_expected() {
    let fixture = Fixture::new();
    let request = capability_request(&fixture);
    let scripted = Arc::new(Mutex::new(VecDeque::from([fixture.session_opened()])));
    let port = TurnAwarePort::new(
        fixture.binding.binding_id.clone(),
        Arc::clone(&fixture.commands),
        scripted,
        TurnOutcome::Cancelled,
        Some(request),
        None,
    );
    let mut factory = ScheduledAgentRuntimeFactory::new(SinglePortFactory {
        port: Some(Box::new(port)),
        workspace: fixture.workspace.clone(),
    });
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();
    assert_eq!(started.handle.poll(), RuntimePoll::Observed { sequence: 2 });
    let RuntimePoll::PermissionRequested { permission, .. } = started.handle.poll() else {
        panic!("provider permission must be observed")
    };
    started
        .handle
        .resolve_provider_permission(
            &permission,
            RuntimePermissionDecision::Deny {
                code: DenialCode::ApprovalDenied,
                safe_message: BoundedText::new("Denied by actor").unwrap(),
            },
        )
        .unwrap();

    assert_eq!(
        started.handle.poll(),
        RuntimePoll::Cancelled {
            cause: RuntimeCancellationCause::ProviderPermissionDenied { permission },
        }
    );
}

#[test]
fn allowed_provider_permission_does_not_authorize_a_cancelled_terminal() {
    let fixture = Fixture::new();
    let request = capability_request(&fixture);
    let scripted = Arc::new(Mutex::new(VecDeque::from([fixture.session_opened()])));
    let port = TurnAwarePort::new(
        fixture.binding.binding_id.clone(),
        Arc::clone(&fixture.commands),
        scripted,
        TurnOutcome::Cancelled,
        Some(request),
        None,
    );
    let mut factory = ScheduledAgentRuntimeFactory::new(SinglePortFactory {
        port: Some(Box::new(port)),
        workspace: fixture.workspace.clone(),
    });
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();
    assert_eq!(started.handle.poll(), RuntimePoll::Observed { sequence: 2 });
    let RuntimePoll::PermissionRequested { permission, .. } = started.handle.poll() else {
        panic!("provider permission must be observed")
    };
    started
        .handle
        .resolve_provider_permission(
            &permission,
            RuntimePermissionDecision::ProviderNativeAllowOnce,
        )
        .unwrap();

    let RuntimePoll::Failed(error) = started.handle.poll() else {
        panic!("cancellation after allow-once must remain a provider failure")
    };
    assert_eq!(error.code.as_str(), "runtime_turn_cancelled_unsolicited");
}

#[test]
fn pending_permission_abandonment_precedes_expected_cancelled_terminal() {
    let fixture = Fixture::new();
    let request = capability_request(&fixture);
    let request_id = request.request_id.clone();
    let turn_id = TurnId::new();
    let events = [
        fixture.session_opened(),
        fixture.event(
            2,
            AgentRuntimeEvent::TurnStarted {
                turn_id: turn_id.clone(),
            },
        ),
        fixture.event(
            3,
            AgentRuntimeEvent::ExecutionPermissionRequested {
                turn_id: turn_id.clone(),
                tool_use_id: None,
                summary: cosh_gateway_contracts::runtime::ToolSummary {
                    name: BoundedName::new("shell").unwrap(),
                    summary: BoundedText::new("Read the workspace").unwrap(),
                },
                callback: provider_callback(request.operation_digest.clone()),
                request,
            },
        ),
        fixture.event(
            4,
            AgentRuntimeEvent::ExecutionPermissionsAbandoned {
                turn_id: turn_id.clone(),
                request_ids: vec![request_id],
            },
        ),
        fixture.event(
            5,
            AgentRuntimeEvent::Completed {
                turn_id,
                outcome: TurnOutcome::Cancelled,
            },
        ),
    ];
    let mut factory = fixture.factory(events);
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();
    assert_eq!(started.handle.poll(), RuntimePoll::Observed { sequence: 2 });
    let RuntimePoll::PermissionRequested { permission, .. } = started.handle.poll() else {
        panic!("provider permission must be observed")
    };
    assert_eq!(
        started.handle.poll(),
        RuntimePoll::PermissionAbandoned {
            sequence: 4,
            permission: permission.clone(),
        }
    );
    assert_eq!(
        started.handle.poll(),
        RuntimePoll::Cancelled {
            cause: RuntimeCancellationCause::ProviderPermissionAbandoned { permission },
        }
    );
}

#[test]
fn wrong_abandoned_permission_identity_fails_closed() {
    let fixture = Fixture::new();
    let request = capability_request(&fixture);
    let turn_id = TurnId::new();
    let events = [
        fixture.session_opened(),
        fixture.event(
            2,
            AgentRuntimeEvent::TurnStarted {
                turn_id: turn_id.clone(),
            },
        ),
        fixture.event(
            3,
            AgentRuntimeEvent::ExecutionPermissionRequested {
                turn_id: turn_id.clone(),
                tool_use_id: None,
                summary: cosh_gateway_contracts::runtime::ToolSummary {
                    name: BoundedName::new("shell").unwrap(),
                    summary: BoundedText::new("Read the workspace").unwrap(),
                },
                callback: provider_callback(request.operation_digest.clone()),
                request,
            },
        ),
        fixture.event(
            4,
            AgentRuntimeEvent::ExecutionPermissionsAbandoned {
                turn_id,
                request_ids: vec![RequestId::new()],
            },
        ),
    ];
    let mut factory = fixture.factory(events);
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();
    assert_eq!(started.handle.poll(), RuntimePoll::Observed { sequence: 2 });
    assert!(matches!(
        started.handle.poll(),
        RuntimePoll::PermissionRequested { .. }
    ));
    let RuntimePoll::Failed(error) = started.handle.poll() else {
        panic!("wrong abandonment identity must fail")
    };
    assert_eq!(error.code.as_str(), "runtime_event_order_invalid");
}

#[test]
fn second_permission_cannot_replace_the_pending_callback() {
    let fixture = Fixture::new();
    let first = capability_request(&fixture);
    let second = capability_request(&fixture);
    let turn_id = TurnId::new();
    let permission_event = |sequence, request: CapabilityRequest| {
        fixture.event(
            sequence,
            AgentRuntimeEvent::ExecutionPermissionRequested {
                turn_id: turn_id.clone(),
                tool_use_id: None,
                summary: cosh_gateway_contracts::runtime::ToolSummary {
                    name: BoundedName::new("shell").unwrap(),
                    summary: BoundedText::new("Read the workspace").unwrap(),
                },
                callback: provider_callback(request.operation_digest.clone()),
                request,
            },
        )
    };
    let events = [
        fixture.session_opened(),
        fixture.event(
            2,
            AgentRuntimeEvent::TurnStarted {
                turn_id: turn_id.clone(),
            },
        ),
        permission_event(3, first),
        permission_event(4, second),
    ];
    let mut factory = fixture.factory(events);
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();
    assert_eq!(started.handle.poll(), RuntimePoll::Observed { sequence: 2 });
    assert!(matches!(
        started.handle.poll(),
        RuntimePoll::PermissionRequested { .. }
    ));
    let RuntimePoll::Failed(error) = started.handle.poll() else {
        panic!("a second live callback must fail closed")
    };
    assert_eq!(error.code.as_str(), "runtime_event_order_invalid");
}

#[test]
fn mismatched_permission_target_fails_without_answering_agent() {
    let fixture = Fixture::new();
    let mut request = capability_request(&fixture);
    request.target.identifier = BoundedOpaque::new("another-host").unwrap();
    let commands = Arc::clone(&fixture.commands);
    let scripted = Arc::new(Mutex::new(VecDeque::from([fixture.session_opened()])));
    let port = TurnAwarePort::new(
        fixture.binding.binding_id.clone(),
        Arc::clone(&commands),
        scripted,
        TurnOutcome::Completed,
        Some(request),
        None,
    );
    let mut factory = ScheduledAgentRuntimeFactory::new(SinglePortFactory {
        port: Some(Box::new(port)),
        workspace: fixture.workspace.clone(),
    });
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();

    assert_eq!(started.handle.poll(), RuntimePoll::Observed { sequence: 2 });
    let RuntimePoll::Failed(error) = started.handle.poll() else {
        panic!("mismatched permission target must fail")
    };
    assert_eq!(error.code.as_str(), "runtime_event_order_invalid");
    assert!(commands
        .lock()
        .unwrap()
        .iter()
        .all(|command| !matches!(command, AgentRuntimeCommand::ResolvePermission { .. })));
}

#[test]
fn brokered_callback_is_fenced_acknowledged_and_delivered_once() {
    let fixture = Fixture::new();
    let request = capability_request(&fixture);
    let operation = BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
        checkpoint_id: CheckpointId::new(),
    });
    let commands = Arc::clone(&fixture.commands);
    let scripted = Arc::new(Mutex::new(VecDeque::from([fixture.session_opened()])));
    let port = TurnAwarePort::new(
        fixture.binding.binding_id.clone(),
        Arc::clone(&commands),
        scripted,
        TurnOutcome::Completed,
        None,
        None,
    )
    .with_brokered(request.clone(), operation.clone());
    let mut factory = ScheduledAgentRuntimeFactory::new(SinglePortFactory {
        port: Some(Box::new(port)),
        workspace: fixture.workspace.clone(),
    });
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();
    assert_eq!(started.handle.poll(), RuntimePoll::Observed { sequence: 2 });
    let RuntimePoll::BrokeredExecutionRequested {
        brokered,
        request: observed,
        operation: observed_operation,
        summary,
    } = started.handle.poll()
    else {
        panic!("brokered request must be returned to the scheduler")
    };
    assert_eq!(*observed, request);
    assert_eq!(observed_operation, operation);
    assert_eq!(brokered.binding_id, fixture.binding.binding_id);
    assert_eq!(
        brokered.runtime_generation,
        fixture.binding.runtime_generation
    );
    assert_eq!(brokered.event_sequence, 3);
    assert_eq!(brokered.request_id, request.request_id);
    assert_eq!(brokered.operation, operation);
    assert_eq!(summary.summary.as_str(), "Create a governed checkpoint");
    assert_eq!(started.handle.poll(), RuntimePoll::Pending);

    let denied = BrokeredExecutionDelivery {
        request_id: request.request_id.clone(),
        outcome: BrokeredExecutionOutcome::Denied {
            code: DenialCode::ApprovalDenied,
            safe_message: BoundedText::new("The checkpoint was denied").unwrap(),
        },
    };
    assert_eq!(
        started
            .handle
            .deliver_brokered_result(&brokered, denied.clone())
            .unwrap_err()
            .code
            .as_str(),
        "runtime_brokered_identity_invalid"
    );
    let mut stale = brokered.clone();
    stale.event_sequence += 1;
    assert_eq!(
        started
            .handle
            .acknowledge_brokered_request(
                &stale,
                BrokeredRequestAcknowledgement {
                    request_id: request.request_id.clone(),
                    approval_id: ApprovalId::new(),
                },
            )
            .unwrap_err()
            .code
            .as_str(),
        "runtime_brokered_identity_invalid"
    );
    let acknowledgement = BrokeredRequestAcknowledgement {
        request_id: request.request_id.clone(),
        approval_id: ApprovalId::new(),
    };
    started
        .handle
        .acknowledge_brokered_request(&brokered, acknowledgement.clone())
        .unwrap();
    assert_eq!(
        started
            .handle
            .acknowledge_brokered_request(&brokered, acknowledgement)
            .unwrap_err()
            .code
            .as_str(),
        "runtime_brokered_identity_invalid"
    );
    let mut wrong_delivery = denied.clone();
    wrong_delivery.request_id = RequestId::new();
    assert_eq!(
        started
            .handle
            .deliver_brokered_result(&brokered, wrong_delivery)
            .unwrap_err()
            .code
            .as_str(),
        "runtime_brokered_identity_invalid"
    );
    started
        .handle
        .deliver_brokered_result(&brokered, denied)
        .unwrap();
    assert_eq!(started.handle.poll(), RuntimePoll::Succeeded);

    let commands = commands.lock().unwrap();
    assert_eq!(
        commands
            .iter()
            .filter(|command| matches!(
                command,
                AgentRuntimeCommand::AcknowledgeBrokeredRequest { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| matches!(command, AgentRuntimeCommand::DeliverBrokeredResult { .. }))
            .count(),
        1
    );
}

#[test]
fn failed_brokered_ack_is_indeterminate_and_never_replayed() {
    let fixture = Fixture::new();
    let request = capability_request(&fixture);
    let operation = BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
        checkpoint_id: CheckpointId::new(),
    });
    let commands = Arc::clone(&fixture.commands);
    let scripted = Arc::new(Mutex::new(VecDeque::from([fixture.session_opened()])));
    let port = TurnAwarePort::new(
        fixture.binding.binding_id.clone(),
        Arc::clone(&commands),
        scripted,
        TurnOutcome::Completed,
        None,
        None,
    )
    .with_brokered(request.clone(), operation)
    .with_failed_brokered_ack();
    let mut factory = ScheduledAgentRuntimeFactory::new(SinglePortFactory {
        port: Some(Box::new(port)),
        workspace: fixture.workspace.clone(),
    });
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();
    assert_eq!(started.handle.poll(), RuntimePoll::Observed { sequence: 2 });
    let RuntimePoll::BrokeredExecutionRequested { brokered, .. } = started.handle.poll() else {
        panic!("brokered request must be returned to the scheduler")
    };
    let acknowledgement = BrokeredRequestAcknowledgement {
        request_id: request.request_id,
        approval_id: ApprovalId::new(),
    };
    assert!(started
        .handle
        .acknowledge_brokered_request(&brokered, acknowledgement.clone())
        .is_err());
    assert!(started
        .handle
        .acknowledge_brokered_request(&brokered, acknowledgement)
        .is_err());

    let commands = commands.lock().unwrap();
    assert_eq!(
        commands
            .iter()
            .filter(|command| matches!(
                command,
                AgentRuntimeCommand::AcknowledgeBrokeredRequest { .. }
            ))
            .count(),
        1
    );
    assert!(commands
        .iter()
        .any(|command| matches!(command, AgentRuntimeCommand::Close { .. })));
}

#[test]
fn cancellation_is_acknowledged_before_the_session_is_closed() {
    let fixture = Fixture::new();
    let commands = Arc::clone(&fixture.commands);
    let scripted = Arc::new(Mutex::new(VecDeque::from([fixture.session_opened()])));
    let port = TurnAwarePort::new(
        fixture.binding.binding_id.clone(),
        Arc::clone(&commands),
        scripted,
        TurnOutcome::Completed,
        None,
        None,
    );
    let mut factory = ScheduledAgentRuntimeFactory::new(SinglePortFactory {
        port: Some(Box::new(port)),
        workspace: fixture.workspace.clone(),
    });
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();

    started
        .handle
        .shutdown(CancelReason::RuntimeShutdown)
        .unwrap();
    let commands = commands.lock().unwrap();
    assert!(matches!(
        commands[2..],
        [
            AgentRuntimeCommand::Cancel { .. },
            AgentRuntimeCommand::Close { .. }
        ]
    ));
}

#[test]
fn event_before_turn_started_fails_closed() {
    let fixture = Fixture::new();
    let mut factory = fixture.factory([
        fixture.session_opened(),
        fixture.event(
            2,
            AgentRuntimeEvent::MessageChunk {
                message_id: Default::default(),
                content: ContentPart::Text {
                    text: BoundedText::new("out of order").unwrap(),
                },
            },
        ),
    ]);
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();
    let RuntimePoll::Failed(error) = started.handle.poll() else {
        panic!("out-of-order event must fail")
    };
    assert_eq!(error.category, ErrorCategory::Internal);
}

#[test]
fn input_request_pauses_polling_and_dispatches_one_exact_response() {
    let fixture = Fixture::new();
    let commands = Arc::clone(&fixture.commands);
    let scripted = Arc::new(Mutex::new(VecDeque::from([fixture.session_opened()])));
    let port = TurnAwarePort::new(
        fixture.binding.binding_id.clone(),
        Arc::clone(&commands),
        scripted,
        TurnOutcome::Completed,
        None,
        None,
    )
    .with_input();
    let mut factory = ScheduledAgentRuntimeFactory::new(SinglePortFactory {
        port: Some(Box::new(port)),
        workspace: fixture.workspace.clone(),
    });
    let mut started = factory.open(&fixture.run).unwrap();
    started.handle.begin().unwrap();
    assert!(matches!(
        started.handle.poll(),
        RuntimePoll::Observed { sequence: 2 }
    ));
    let RuntimePoll::InputRequested { sequence, request } = started.handle.poll() else {
        panic!("bounded Runtime input must reach durable coordination")
    };
    assert_eq!(sequence, 3);
    assert_eq!(started.handle.poll(), RuntimePoll::Pending);
    let response = RuntimeInputResponse::Text {
        text: BoundedText::new("main").unwrap(),
    };
    started
        .handle
        .resolve_input(&request, response.clone())
        .unwrap();
    assert!(matches!(
        commands.lock().unwrap().last(),
        Some(AgentRuntimeCommand::ResolveInput {
            request_id,
            run_id,
            turn_id,
            response: observed,
        }) if request_id == request.request_id()
            && run_id == request.run_id()
            && turn_id == request.turn_id()
            && observed == &response
    ));
    assert_eq!(started.handle.poll(), RuntimePoll::Succeeded);
}

#[test]
fn session_generation_must_match_the_scheduler_lease() {
    let mut fixture = Fixture::new();
    fixture.binding.runtime_generation = fixture.run.lease_generation + 1;
    let mut factory = fixture.factory([fixture.session_opened()]);

    let error = match factory.open(&fixture.run) {
        Ok(_) => panic!("stale Runtime generation must not start"),
        Err(error) => error,
    };
    assert_eq!(error.code.as_str(), "runtime_event_order_invalid");
}

struct SinglePortFactory {
    port: Option<Box<dyn AgentRuntimePort>>,
    workspace: WorkspaceRef,
}

impl AgentRuntimePortFactory for SinglePortFactory {
    fn create(&mut self, _run: &ScheduledRun) -> Result<ScheduledRuntimePort, ContractError> {
        Ok(ScheduledRuntimePort::new(
            self.port.take().unwrap(),
            self.workspace.clone(),
        ))
    }
}

struct TurnAwarePort {
    binding_id: RuntimeBindingId,
    commands: Arc<Mutex<Vec<AgentRuntimeCommand>>>,
    events: Arc<Mutex<VecDeque<RuntimeEventEnvelope>>>,
    template: RuntimeEventEnvelope,
    outcome: Option<TurnOutcome>,
    permission: Option<CapabilityRequest>,
    brokered: Option<(CapabilityRequest, BrokeredOperation)>,
    fail_brokered_ack: bool,
    input_requested: bool,
    progress: Option<BoundedText>,
    deferred_permission_terminal: Option<(TurnId, u64)>,
    cancelled: bool,
}

impl TurnAwarePort {
    fn new(
        binding_id: RuntimeBindingId,
        commands: Arc<Mutex<Vec<AgentRuntimeCommand>>>,
        events: Arc<Mutex<VecDeque<RuntimeEventEnvelope>>>,
        outcome: TurnOutcome,
        permission: Option<CapabilityRequest>,
        progress: Option<BoundedText>,
    ) -> Self {
        let template = events
            .lock()
            .unwrap()
            .front()
            .cloned()
            .expect("session-opened template");
        Self {
            binding_id,
            commands,
            events,
            template,
            outcome: Some(outcome),
            permission,
            brokered: None,
            fail_brokered_ack: false,
            input_requested: false,
            progress,
            deferred_permission_terminal: None,
            cancelled: false,
        }
    }

    fn with_brokered(mut self, request: CapabilityRequest, operation: BrokeredOperation) -> Self {
        self.brokered = Some((request, operation));
        self
    }

    fn with_failed_brokered_ack(mut self) -> Self {
        self.fail_brokered_ack = true;
        self
    }

    fn with_input(mut self) -> Self {
        self.input_requested = true;
        self
    }
}

impl AgentRuntimePort for TurnAwarePort {
    fn binding_id(&self) -> &RuntimeBindingId {
        &self.binding_id
    }

    fn dispatch(
        &mut self,
        command: AgentRuntimeCommand,
        _deadline: Instant,
    ) -> Result<(), AgentRuntimePortError> {
        if self.fail_brokered_ack
            && matches!(
                &command,
                AgentRuntimeCommand::AcknowledgeBrokeredRequest { .. }
            )
        {
            self.commands.lock().unwrap().push(command);
            return Err(AgentRuntimePortError::Transport);
        }
        if let AgentRuntimeCommand::Prompt { turn_id, .. } = &command {
            let mut next = self.template.clone();
            next.sequence = 2;
            next.header.occurred_at_ms = 2;
            next.event = AgentRuntimeEvent::TurnStarted {
                turn_id: turn_id.clone(),
            };
            self.events.lock().unwrap().push_back(next);
            let mut next_sequence = 3;
            if let Some(text) = self.progress.take() {
                let mut progress = self.template.clone();
                progress.sequence = next_sequence;
                progress.header.occurred_at_ms = next_sequence;
                progress.event = AgentRuntimeEvent::MessageChunk {
                    message_id: Default::default(),
                    content: ContentPart::Text { text },
                };
                self.events.lock().unwrap().push_back(progress);
                next_sequence += 1;
            }
            let permission_was_emitted = self.permission.is_some();
            if let Some(request) = self.permission.take() {
                let mut permission = self.template.clone();
                permission.sequence = next_sequence;
                permission.header.occurred_at_ms = next_sequence;
                permission.event = AgentRuntimeEvent::ExecutionPermissionRequested {
                    turn_id: turn_id.clone(),
                    tool_use_id: None,
                    callback: provider_callback(request.operation_digest.clone()),
                    request,
                    summary: cosh_gateway_contracts::runtime::ToolSummary {
                        name: BoundedName::new("shell").unwrap(),
                        summary: BoundedText::new("Run the inspected shell command").unwrap(),
                    },
                };
                self.events.lock().unwrap().push_back(permission);
                next_sequence += 1;
            }
            if let Some((request, operation)) = self.brokered.take() {
                let mut brokered = self.template.clone();
                brokered.sequence = next_sequence;
                brokered.header.occurred_at_ms = next_sequence;
                brokered.event = AgentRuntimeEvent::BrokeredExecutionRequested {
                    turn_id: turn_id.clone(),
                    tool_use_id: None,
                    request,
                    operation,
                    summary: cosh_gateway_contracts::runtime::ToolSummary {
                        name: BoundedName::new("workspace_checkpoint_create").unwrap(),
                        summary: BoundedText::new("Create a governed checkpoint").unwrap(),
                    },
                };
                self.events.lock().unwrap().push_back(brokered);
                next_sequence += 1;
            }
            if self.input_requested {
                self.input_requested = false;
                let mut input = self.template.clone();
                input.sequence = next_sequence;
                input.header.occurred_at_ms = next_sequence;
                input.event = AgentRuntimeEvent::InputRequested {
                    request: RuntimeInputRequest::new(
                        InputRequestId::new(),
                        match &command {
                            AgentRuntimeCommand::Prompt { run_id, .. } => run_id.clone(),
                            _ => unreachable!("input is emitted only while handling Prompt"),
                        },
                        turn_id.clone(),
                        None,
                        BoundedText::new("Choose safely").unwrap(),
                        Vec::new(),
                        true,
                        false,
                    )
                    .unwrap(),
                };
                self.events.lock().unwrap().push_back(input);
                next_sequence += 1;
            }
            if permission_was_emitted {
                self.deferred_permission_terminal = Some((turn_id.clone(), next_sequence));
            } else {
                self.push_completion(turn_id.clone(), next_sequence);
            }
        }
        if matches!(&command, AgentRuntimeCommand::ResolvePermission { .. }) {
            if let Some((turn_id, sequence)) = self.deferred_permission_terminal.take() {
                self.push_completion(turn_id, sequence);
            }
        }
        let close_after_cancel =
            self.cancelled && matches!(&command, AgentRuntimeCommand::Close { .. });
        if matches!(&command, AgentRuntimeCommand::Cancel { .. }) {
            self.cancelled = true;
        }
        self.commands.lock().unwrap().push(command);
        if close_after_cancel {
            Err(AgentRuntimePortError::Terminal)
        } else {
            Ok(())
        }
    }

    fn next_event(
        &mut self,
        deadline: Instant,
    ) -> Result<RuntimeEventEnvelope, AgentRuntimePortError> {
        if deadline <= Instant::now() {
            return Err(AgentRuntimePortError::Deadline {
                operation: "next_event",
            });
        }
        self.events
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(AgentRuntimePortError::Deadline {
                operation: "next_event",
            })
    }
}

impl TurnAwarePort {
    fn push_completion(&mut self, turn_id: TurnId, sequence: u64) {
        let mut completed = self.template.clone();
        completed.sequence = sequence;
        completed.header.occurred_at_ms = sequence;
        completed.event = AgentRuntimeEvent::Completed {
            turn_id,
            outcome: self.outcome.take().expect("one terminal outcome"),
        };
        self.events.lock().unwrap().push_back(completed);
    }
}

fn provider_callback(
    operation_digest: Digest,
) -> cosh_gateway_contracts::runtime::ProviderPermissionCallbackV2 {
    cosh_gateway_contracts::runtime::ProviderPermissionCallbackV2 {
        provider_session_digest: digest('s'),
        provider_request_id_digest: digest('r'),
        provider_tool_call_id_digest: digest('t'),
        ordered_option_set_digest: digest('o'),
        callback_payload_digest: digest('c'),
        normalized_operation_digest: operation_digest,
    }
}

fn capability_request(fixture: &Fixture) -> CapabilityRequest {
    let actor_id = fixture.run.actor.actor_id.clone();
    CapabilityRequest {
        request_id: RequestId::new(),
        task_id: fixture.run.task_id.clone(),
        run_id: fixture.run.run_id.clone(),
        actor: ActorRef {
            actor_id,
            actor_kind: ActorKind::Human,
            issuer: BoundedName::new("local-os").unwrap(),
            assurance: AuthAssurance::LocalOs,
        },
        target: fixture.run.target.clone(),
        operation: OperationDescriptor {
            namespace: BoundedName::new("process").unwrap(),
            name: BoundedName::new("spawn").unwrap(),
            arguments_digest: digest('a'),
        },
        operation_digest: digest('o'),
        requested_scope: CapabilityScope {
            resource: BoundedName::new("process").unwrap(),
            access: BoundedName::new("execute").unwrap(),
        },
        input_digest: digest('i'),
        expires_at_ms: u64::MAX,
    }
}

fn digest(_character: char) -> Digest {
    Digest::parse("a".repeat(64)).unwrap()
}
