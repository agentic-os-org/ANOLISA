use cosh_gateway_contracts::{
    capability::{
        ApprovalDecision, ApprovalRequest, BrokeredOperation, CapabilityRequest, CapabilityScope,
        OperationDescriptor, RuntimeExecutionFence, WorkspaceCheckpointCreateV1,
    },
    common::{
        ActorKind, ActorRef, AuthAssurance, BoundedName, BoundedOpaque, BoundedText,
        ContractHeader, ContractSchema, Correlation, Digest, IdempotencyKey, RuntimeBindingRef,
        RuntimeSelector, TargetRef,
    },
    external::{ExternalRef, ExternalRefKind},
    ids::{
        ActorId, AgentSessionId, ApprovalId, CheckpointId, ExecutionId, InstallationId, MessageId,
        PermitId, RequestId, RunId, RuntimeBindingId, RuntimeInstanceId, TaskId, ToolUseId, TurnId,
    },
    runtime::RuntimePermissionRef,
    task::{TaskEvent, TaskEventEnvelope},
};

use super::*;
use crate::storage::{
    ApprovalState, CommitOutcome, LeaseClaim, LeaseCommand, LedgerCommand, LedgerOutcome,
    PermitState, TaskCommit,
};

fn digest(byte: char) -> Digest {
    Digest::parse(byte.to_string().repeat(64)).unwrap()
}

fn target() -> TargetRef {
    TargetRef {
        kind: BoundedName::new("local").unwrap(),
        authority: BoundedName::new("test").unwrap(),
        identifier: BoundedOpaque::new("host").unwrap(),
    }
}

fn command(actor_id: &ActorId, key: &str, byte: char, now_ms: u64) -> LedgerCommand {
    LedgerCommand {
        actor_id: actor_id.clone(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        command_digest: digest(byte),
        committed_at_ms: now_ms,
    }
}

fn create_started_task(store: &mut SqliteTaskStore, actor_id: &ActorId, run_id: &RunId) -> TaskId {
    let task_id = TaskId::new();
    let mut correlation = Correlation::new(InstallationId::new());
    correlation.actor_id = Some(actor_id.clone());
    correlation.task_id = Some(task_id.clone());
    let event = |revision, event| TaskEventEnvelope {
        header: ContractHeader::new(
            ContractSchema::TaskEvent,
            MessageId::new(),
            1,
            correlation.clone(),
        ),
        task_id: task_id.clone(),
        revision,
        event,
    };
    let result = store
        .commit_task(&TaskCommit {
            actor_id: actor_id.clone(),
            idempotency_key: IdempotencyKey::new("create-task").unwrap(),
            command_digest: digest('0'),
            expected_revision: Some(0),
            events: vec![
                event(
                    1,
                    TaskEvent::TaskSubmitted {
                        intent_digest: digest('1'),
                        target: target(),
                    },
                ),
                event(
                    2,
                    TaskEvent::TaskQueued {
                        run_id: run_id.clone(),
                        runtime: RuntimeSelector {
                            runtime: BoundedName::new("acp").unwrap(),
                            profile: None,
                        },
                    },
                ),
                event(
                    3,
                    TaskEvent::RunStarted {
                        run_id: run_id.clone(),
                    },
                ),
            ],
            outbox: Vec::new(),
            committed_at_ms: 1,
        })
        .unwrap();
    assert!(matches!(result, CommitOutcome::Applied(_)));
    task_id
}

fn request(actor_id: ActorId, task_id: TaskId, run_id: RunId) -> CapabilityRequest {
    CapabilityRequest {
        request_id: RequestId::new(),
        task_id,
        run_id,
        actor: ActorRef {
            actor_id,
            actor_kind: ActorKind::Human,
            issuer: BoundedName::new("local-os").unwrap(),
            assurance: AuthAssurance::LocalOs,
        },
        target: target(),
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
        expires_at_ms: 100,
    }
}

fn approval(request: &CapabilityRequest) -> ApprovalRequest {
    ApprovalRequest {
        approval_id: ApprovalId::new(),
        request_id: request.request_id.clone(),
        task_id: request.task_id.clone(),
        run_id: request.run_id.clone(),
        summary: BoundedText::new("run one command").unwrap(),
        expires_at_ms: 90,
    }
}

fn provider_permission(request: &CapabilityRequest) -> RuntimePermissionRef {
    RuntimePermissionRef {
        binding_id: RuntimeBindingId::new(),
        runtime_generation: 1,
        event_sequence: 1,
        run_id: request.run_id.clone(),
        turn_id: TurnId::new(),
        tool_use_id: Some(ToolUseId::new()),
        request_id: request.request_id.clone(),
        callback: Some(
            cosh_gateway_contracts::runtime::ProviderPermissionCallbackV2 {
                provider_session_digest: digest('1'),
                provider_request_id_digest: digest('2'),
                provider_tool_call_id_digest: digest('3'),
                ordered_option_set_digest: digest('4'),
                callback_payload_digest: digest('5'),
                normalized_operation_digest: request.operation_digest.clone(),
            },
        ),
        core_callback: None,
    }
}

fn brokered_binding(
    store: &mut SqliteTaskStore,
    actor_id: &ActorId,
    request: &CapabilityRequest,
) -> (BrokeredOperation, Digest, RuntimeExecutionFence) {
    let LedgerOutcome::Applied(lease) = store
        .acquire_run_lease(&LeaseCommand {
            command: command(actor_id, "brokered-lease", 'd', 5),
            task_id: request.task_id.clone(),
            run_id: request.run_id.clone(),
            lease_owner: BoundedOpaque::new("brokered-test").unwrap(),
            expires_at_ms: 100,
        })
        .unwrap()
    else {
        panic!("lease must apply")
    };
    let claim = LeaseClaim {
        task_id: lease.task_id,
        run_id: lease.run_id,
        lease_owner: lease.lease_owner,
        generation: lease.generation,
        revision: lease.revision,
    };
    let binding = RuntimeBindingRef {
        binding_id: RuntimeBindingId::new(),
        task_id: request.task_id.clone(),
        run_id: request.run_id.clone(),
        agent_session_id: AgentSessionId::new(),
        runtime_instance_id: RuntimeInstanceId::new(),
        runtime_generation: claim.generation,
        external_session: ExternalRef {
            kind: ExternalRefKind::AcpSession,
            authority: BoundedName::new("test").unwrap(),
            scope_digest: digest('1'),
            value: BoundedOpaque::new("brokered").unwrap(),
        },
    };
    store
        .bind_runtime(
            &command(actor_id, "brokered-bind", 'e', 6),
            &binding,
            &claim,
        )
        .unwrap();
    (
        BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
            checkpoint_id: CheckpointId::new(),
        }),
        digest('f'),
        RuntimeExecutionFence {
            binding_id: binding.binding_id,
            runtime_generation: binding.runtime_generation,
            lease_generation: claim.generation,
            lease_revision: claim.revision,
        },
    )
}

#[test]
fn provider_native_pending_records_observation_binding_without_permit() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_started_task(&mut store, &actor_id, &run_id);
    let request = request(actor_id.clone(), task_id, run_id);
    let approval = approval(&request);
    let permission = provider_permission(&request);
    let LedgerOutcome::Applied(lease) = store
        .acquire_run_lease(&LeaseCommand {
            command: command(&actor_id, "provider-lease", 'a', 5),
            task_id: request.task_id.clone(),
            run_id: request.run_id.clone(),
            lease_owner: BoundedOpaque::new("provider-test").unwrap(),
            expires_at_ms: 100,
        })
        .unwrap()
    else {
        panic!("lease must be newly acquired")
    };
    let lease = LeaseClaim {
        task_id: lease.task_id,
        run_id: lease.run_id,
        lease_owner: lease.lease_owner,
        generation: lease.generation,
        revision: lease.revision,
    };
    let binding = RuntimeBindingRef {
        binding_id: permission.binding_id.clone(),
        task_id: request.task_id.clone(),
        run_id: request.run_id.clone(),
        agent_session_id: AgentSessionId::new(),
        runtime_instance_id: RuntimeInstanceId::new(),
        runtime_generation: lease.generation,
        external_session: ExternalRef {
            kind: ExternalRefKind::AcpSession,
            authority: BoundedName::new("test").unwrap(),
            scope_digest: digest('c'),
            value: BoundedOpaque::new("provider-session").unwrap(),
        },
    };
    store
        .bind_runtime(
            &command(&actor_id, "provider-binding", 'b', 6),
            &binding,
            &lease,
        )
        .unwrap();
    let pending = DurableApprovalCoordinator::new(&mut store)
        .record_provider_pending(
            &command(&actor_id, "provider-pending", 'c', 10),
            &request,
            &approval,
            &permission,
            &binding,
            &lease,
        )
        .unwrap();
    assert_eq!(pending.permission, Some(permission));
    assert_eq!(pending.state, ApprovalState::Pending);
}

#[test]
fn approval_creates_no_authority_until_explicit_allow() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_started_task(&mut store, &actor_id, &run_id);
    let request = request(actor_id.clone(), task_id, run_id);
    let approval = approval(&request);
    let (operation, target_identity_digest, runtime_fence) =
        brokered_binding(&mut store, &actor_id, &request);
    let provider_binding = BoundedOpaque::new("checkpoint-binding-v2").unwrap();
    let mut coordinator = DurableApprovalCoordinator::new(&mut store);
    let pending = coordinator
        .record_pending(
            &command(&actor_id, "record", '5', 10),
            &request,
            &approval,
            BrokeredApprovalBinding {
                operation: &operation,
                target_identity_digest: &target_identity_digest,
                runtime_fence: &runtime_fence,
                provider_binding: Some(&provider_binding),
            },
        )
        .unwrap();
    assert_eq!(pending.state, ApprovalState::Pending);
    let stored = coordinator
        .store
        .load_brokered_request(&request.request_id)
        .unwrap();
    assert_eq!(stored.operation, operation);
    assert_eq!(stored.provider_binding, Some(provider_binding));
    let delivery_kind = BoundedName::new("brokered_approval_request").unwrap();
    let delivery = coordinator
        .store
        .claim_outbox(
            &delivery_kind,
            &BoundedOpaque::new("approval-dispatcher").unwrap(),
            11,
            50,
        )
        .unwrap()
        .expect("approval and Outbox intent commit together");
    assert_eq!(delivery.task_id, request.task_id);

    let result = coordinator
        .resolve_once(
            &request,
            &approval,
            DurableApprovalResolution {
                resolution_command: &command(&actor_id, "allow", '6', 20),
                permit_command: &command(&actor_id, "permit", '7', 21),
                expected_revision: pending.revision,
                decision: ApprovalDecision::Approve,
                policy_revision: 1,
                policy_valid_until_ms: 80,
                permit_id: PermitId::new(),
                execution_id: ExecutionId::new(),
            },
        )
        .unwrap();
    let DurableApprovalOutcome::Permit(permit) = result else {
        panic!("allow once must issue a permit")
    };
    assert_eq!(permit.state, PermitState::Issued);
    assert_eq!(permit.permit.approval_id, Some(approval.approval_id));
    assert!(permit.permit.single_use);
}

#[test]
fn denial_is_durable_and_never_issues_a_permit() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_started_task(&mut store, &actor_id, &run_id);
    let request = request(actor_id.clone(), task_id, run_id);
    let approval = approval(&request);
    let (operation, target_identity_digest, runtime_fence) =
        brokered_binding(&mut store, &actor_id, &request);
    let mut coordinator = DurableApprovalCoordinator::new(&mut store);
    coordinator
        .record_pending(
            &command(&actor_id, "record", '8', 10),
            &request,
            &approval,
            BrokeredApprovalBinding {
                operation: &operation,
                target_identity_digest: &target_identity_digest,
                runtime_fence: &runtime_fence,
                provider_binding: None,
            },
        )
        .unwrap();
    let result = coordinator
        .resolve_once(
            &request,
            &approval,
            DurableApprovalResolution {
                resolution_command: &command(&actor_id, "deny", '9', 20),
                permit_command: &command(&actor_id, "unused", 'a', 21),
                expected_revision: 1,
                decision: ApprovalDecision::Deny,
                policy_revision: 1,
                policy_valid_until_ms: 80,
                permit_id: PermitId::new(),
                execution_id: ExecutionId::new(),
            },
        )
        .unwrap();
    let DurableApprovalOutcome::NotPermitted(record) = result else {
        panic!("denial must not create a permit")
    };
    assert_eq!(record.state, ApprovalState::Denied);
}

#[test]
fn changed_approval_binding_fails_before_storage() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_started_task(&mut store, &actor_id, &run_id);
    let request = request(actor_id.clone(), task_id, run_id);
    let (operation, target_identity_digest, runtime_fence) =
        brokered_binding(&mut store, &actor_id, &request);
    let mut approval = approval(&request);
    approval.request_id = RequestId::new();
    let result = DurableApprovalCoordinator::new(&mut store).record_pending(
        &command(&actor_id, "record", 'b', 10),
        &request,
        &approval,
        BrokeredApprovalBinding {
            operation: &operation,
            target_identity_digest: &target_identity_digest,
            runtime_fence: &runtime_fence,
            provider_binding: None,
        },
    );
    assert!(matches!(result, Err(DurableApprovalError::BindingMismatch)));
}
