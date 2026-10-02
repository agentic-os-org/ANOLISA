use cosh_gateway_contracts::capability::{
    ApprovalRequest, BrokeredOperation, CapabilityRequest, CapabilityScope, OperationDescriptor,
    WorkspaceCheckpointCreateV1,
};
use cosh_gateway_contracts::common::{
    ActorKind, ActorRef, AuthAssurance, BoundedName, BoundedText, ContractHeader, ContractSchema,
    Correlation, RuntimeSelector,
};
use cosh_gateway_contracts::external::{ExternalRef, ExternalRefKind};
use cosh_gateway_contracts::ids::{
    AgentSessionId, CheckpointId, DeliveryId, InputRequestId, InstallationId, MessageId, ToolUseId,
    TurnId,
};
use cosh_gateway_contracts::runtime::{
    BrokeredExecutionDelivery, BrokeredExecutionOutcome, BrokeredExecutionRef,
    BrokeredOperationResult, RuntimeInputRequest, RuntimeInputResponse, RuntimePermissionRef,
    WorkspaceCheckpointCreateV1Outcome, WorkspaceCheckpointCreateV1Result,
};
use cosh_gateway_contracts::task::{TaskEvent, TaskEventEnvelope};

use super::*;
use crate::storage::{CommitOutcome, OutboxIntent, TaskCommit};

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

fn create_task(store: &mut SqliteTaskStore, actor_id: &ActorId, run_id: &RunId) -> TaskId {
    let task_id = TaskId::new();
    let mut correlation = Correlation::new(InstallationId::new());
    correlation.actor_id = Some(actor_id.clone());
    correlation.task_id = Some(task_id.clone());
    let envelope = |revision, event| TaskEventEnvelope {
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
    let events = vec![
        envelope(
            1,
            TaskEvent::TaskSubmitted {
                intent_digest: digest('0'),
                target: target(),
            },
        ),
        envelope(
            2,
            TaskEvent::TaskQueued {
                run_id: run_id.clone(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("acp").unwrap(),
                    profile: Some(BoundedName::new("test").unwrap()),
                },
            },
        ),
        envelope(
            3,
            TaskEvent::RunStarted {
                run_id: run_id.clone(),
            },
        ),
    ];
    let outcome = store
        .commit_task(&TaskCommit {
            actor_id: actor_id.clone(),
            idempotency_key: IdempotencyKey::new(format!("task-{}", task_id.as_str())).unwrap(),
            command_digest: digest('1'),
            expected_revision: Some(0),
            events,
            outbox: Vec::new(),
            committed_at_ms: 1,
        })
        .unwrap();
    assert!(matches!(outcome, CommitOutcome::Applied(_)));
    task_id
}

fn append_task_event(
    store: &mut SqliteTaskStore,
    actor_id: &ActorId,
    task_id: &TaskId,
    revision: u64,
    key: &str,
    now_ms: u64,
    event: TaskEvent,
) {
    let mut correlation = Correlation::new(InstallationId::new());
    correlation.actor_id = Some(actor_id.clone());
    correlation.task_id = Some(task_id.clone());
    let envelope = TaskEventEnvelope {
        header: ContractHeader::new(ContractSchema::TaskEvent, MessageId::new(), 1, correlation),
        task_id: task_id.clone(),
        revision,
        event,
    };
    store
        .commit_task(&TaskCommit {
            actor_id: actor_id.clone(),
            idempotency_key: IdempotencyKey::new(key).unwrap(),
            command_digest: digest('f'),
            expected_revision: Some(revision - 1),
            events: vec![envelope],
            outbox: Vec::new(),
            committed_at_ms: now_ms,
        })
        .unwrap();
}

fn runtime_input_request(run_id: &RunId) -> RuntimeInputRequest {
    RuntimeInputRequest::new(
        InputRequestId::new(),
        run_id.clone(),
        TurnId::new(),
        None,
        BoundedText::new("Which safe option?").unwrap(),
        Vec::new(),
        true,
        false,
    )
    .unwrap()
}

fn acquire_lease(
    store: &mut SqliteTaskStore,
    actor_id: &ActorId,
    task_id: &TaskId,
    run_id: &RunId,
    key: &str,
    now_ms: u64,
    expires_at_ms: u64,
) -> LeaseClaim {
    let lease = LeaseCommand {
        command: command(actor_id, key, 'a', now_ms),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        lease_owner: BoundedOpaque::new(format!("owner-{key}")).unwrap(),
        expires_at_ms,
    };
    let LedgerOutcome::Applied(record) = store.acquire_run_lease(&lease).unwrap() else {
        panic!("lease must apply")
    };
    LeaseClaim {
        task_id: record.task_id,
        run_id: record.run_id,
        lease_owner: record.lease_owner,
        generation: record.generation,
        revision: record.revision,
    }
}

fn approval(actor_id: &ActorId, task_id: &TaskId, run_id: &RunId) -> ApprovalRecord {
    ApprovalRecord {
        approval_id: ApprovalId::new(),
        request_id: RequestId::new(),
        actor_id: actor_id.clone(),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        target: target(),
        target_identity_digest: None,
        runtime_fence: None,
        operation_digest: digest('2'),
        input_digest: digest('3'),
        permission: None,
        state: ApprovalState::Pending,
        revision: 1,
        expires_at_ms: 100,
        decided_by_actor_id: None,
        created_at_ms: 10,
        updated_at_ms: 10,
    }
}

fn approved_fixture(
    store: &mut SqliteTaskStore,
) -> (ActorId, TaskId, RunId, ApprovalRecord, LeaseClaim) {
    let (actor_id, task_id, run_id, approval, lease, _brokered) = pending_brokered_fixture(store);
    let resolved = store
        .resolve_approval(
            &command(&actor_id, "resolve-approval", '5', 20),
            &approval.approval_id,
            1,
            ApprovalResolution::Decide(ApprovalDecision::Approve),
        )
        .unwrap();
    let LedgerOutcome::Applied(approval) = resolved else {
        panic!("approval must be applied")
    };
    (actor_id, task_id, run_id, approval, lease)
}

fn pending_brokered_fixture(
    store: &mut SqliteTaskStore,
) -> (
    ActorId,
    TaskId,
    RunId,
    ApprovalRecord,
    LeaseClaim,
    BrokeredExecutionRef,
) {
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(store, &actor_id, &run_id);
    let lease = acquire_lease(store, &actor_id, &task_id, &run_id, "fixture", 2, 200);
    let binding = runtime_binding(&task_id, &run_id, lease.generation);
    store
        .bind_runtime(
            &command(&actor_id, "fixture-bind", '3', 3),
            &binding,
            &lease,
        )
        .unwrap();
    store
        .record_runtime_sequence(
            &binding.binding_id,
            &binding.runtime_instance_id,
            binding.runtime_generation,
            1,
            4,
            &lease,
        )
        .unwrap();
    let mut approval = approval(&actor_id, &task_id, &run_id);
    approval.target_identity_digest = Some(digest('8'));
    approval.runtime_fence = Some(RuntimeExecutionFence {
        binding_id: binding.binding_id.clone(),
        runtime_generation: binding.runtime_generation,
        lease_generation: lease.generation,
        lease_revision: lease.revision,
    });
    approval.created_at_ms = 13;
    approval.updated_at_ms = 13;
    let request = CapabilityRequest {
        request_id: approval.request_id.clone(),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        actor: ActorRef {
            actor_id: actor_id.clone(),
            actor_kind: ActorKind::Human,
            issuer: BoundedName::new("local-os").unwrap(),
            assurance: AuthAssurance::LocalOs,
        },
        target: approval.target.clone(),
        operation: OperationDescriptor {
            namespace: BoundedName::new("workspace").unwrap(),
            name: BoundedName::new("checkpoint_create").unwrap(),
            arguments_digest: approval.operation_digest.clone(),
        },
        operation_digest: approval.operation_digest.clone(),
        requested_scope: CapabilityScope {
            resource: BoundedName::new("workspace").unwrap(),
            access: BoundedName::new("checkpoint").unwrap(),
        },
        input_digest: approval.input_digest.clone(),
        expires_at_ms: approval.expires_at_ms,
    };
    let approval_request = ApprovalRequest {
        approval_id: approval.approval_id.clone(),
        request_id: approval.request_id.clone(),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        summary: BoundedText::new("create workspace checkpoint").unwrap(),
        expires_at_ms: approval.expires_at_ms,
    };
    let operation = BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
        checkpoint_id: CheckpointId::new(),
    });
    store
        .create_brokered_approval(
            &command(&actor_id, "create-approval", '4', 13),
            &request,
            &approval_request,
            &operation,
            None,
            &approval,
        )
        .unwrap();
    let brokered = BrokeredExecutionRef {
        binding_id: binding.binding_id,
        runtime_generation: binding.runtime_generation,
        event_sequence: 1,
        run_id: run_id.clone(),
        turn_id: TurnId::new(),
        tool_use_id: Some(ToolUseId::new()),
        request_id: approval.request_id.clone(),
        operation,
    };
    (actor_id, task_id, run_id, approval, lease, brokered)
}

fn permit(approval: &ApprovalRecord) -> ExecutionPermit {
    ExecutionPermit {
        permit_id: PermitId::new(),
        request_id: approval.request_id.clone(),
        actor_id: approval.actor_id.clone(),
        approval_id: Some(approval.approval_id.clone()),
        task_id: approval.task_id.clone(),
        run_id: approval.run_id.clone(),
        execution_id: ExecutionId::new(),
        target: approval.target.clone(),
        target_identity_digest: approval.target_identity_digest.clone().unwrap(),
        runtime_fence: approval.runtime_fence.clone().unwrap(),
        operation_digest: approval.operation_digest.clone(),
        input_digest: approval.input_digest.clone(),
        policy_revision: 7,
        valid_until_ms: 90,
        single_use: true,
    }
}

fn claim(permit: &ExecutionPermit, lease: &LeaseClaim) -> ExecutionClaim {
    ExecutionClaim {
        permit_id: permit.permit_id.clone(),
        execution_id: permit.execution_id.clone(),
        task_id: permit.task_id.clone(),
        run_id: permit.run_id.clone(),
        target: permit.target.clone(),
        target_identity_digest: permit.target_identity_digest.clone(),
        runtime_fence: permit.runtime_fence.clone(),
        operation_digest: permit.operation_digest.clone(),
        input_digest: permit.input_digest.clone(),
        policy_revision: permit.policy_revision,
        lease: lease.clone(),
    }
}

fn claim_and_start(
    store: &mut SqliteTaskStore,
    actor_id: &ActorId,
    permit: &ExecutionPermit,
    lease: &LeaseClaim,
    key: &str,
    now_ms: u64,
) -> ExecutionRecord {
    let LedgerOutcome::Applied(claimed) = store
        .claim_execution(
            &command(actor_id, &format!("{key}-claim"), 'c', now_ms),
            &claim(permit, lease),
        )
        .unwrap()
    else {
        panic!("claim must apply")
    };
    let LedgerOutcome::Applied(started) = store
        .start_claimed_execution(
            &command(actor_id, &format!("{key}-start"), 'd', now_ms + 1),
            &permit.execution_id,
            claimed.revision,
            &SecurityAuditProof {
                proof_digest: digest('e'),
                persisted_at_ms: now_ms,
            },
        )
        .unwrap()
    else {
        panic!("start must apply")
    };
    started
}

fn brokered_reference(store: &SqliteTaskStore, approval: &ApprovalRecord) -> BrokeredExecutionRef {
    let request = store.load_brokered_request(&approval.request_id).unwrap();
    BrokeredExecutionRef {
        binding_id: request.runtime_fence.binding_id,
        runtime_generation: request.runtime_fence.runtime_generation,
        event_sequence: 1,
        run_id: approval.run_id.clone(),
        turn_id: TurnId::new(),
        tool_use_id: Some(ToolUseId::new()),
        request_id: approval.request_id.clone(),
        operation: request.operation,
    }
}

fn successful_result(store: &SqliteTaskStore, request_id: &RequestId) -> BrokeredOperationResult {
    let request = store.load_brokered_request(request_id).unwrap();
    let BrokeredOperation::WorkspaceCheckpointCreateV1(operation) = request.operation;
    BrokeredOperationResult::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1Result {
        checkpoint_id: operation.checkpoint_id,
        outcome: WorkspaceCheckpointCreateV1Outcome::Skipped {
            reason: BoundedText::new("test result").unwrap(),
        },
    })
}

fn denied_delivery(request_id: &RequestId) -> BrokeredExecutionDelivery {
    BrokeredExecutionDelivery {
        request_id: request_id.clone(),
        outcome: BrokeredExecutionOutcome::Denied {
            code: DenialCode::ApprovalDenied,
            safe_message: BoundedText::new("The checkpoint request was denied").unwrap(),
        },
    }
}

fn uncertain_delivery(permit: &ExecutionPermit) -> BrokeredExecutionDelivery {
    BrokeredExecutionDelivery {
        request_id: permit.request_id.clone(),
        outcome: BrokeredExecutionOutcome::Uncertain {
            execution_id: permit.execution_id.clone(),
            error: ContractError::new(
                "checkpoint_execution_uncertain",
                ErrorCategory::Internal,
                false,
                "Checkpoint outcome is indeterminate and requires reconciliation",
            )
            .unwrap(),
        },
    }
}

#[test]
fn approval_resolution_is_actor_revision_deadline_and_idempotency_bound() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let approval = approval(&actor_id, &task_id, &run_id);
    let create = command(&actor_id, "create", '6', 10);

    assert!(matches!(
        store.create_approval(&create, &approval).unwrap(),
        LedgerOutcome::Applied(_)
    ));
    assert!(matches!(
        store.create_approval(&create, &approval).unwrap(),
        LedgerOutcome::Replayed(_)
    ));
    let attacker = ActorId::new();
    assert!(matches!(
        store.resolve_approval(
            &command(&attacker, "attack", '7', 20),
            &approval.approval_id,
            1,
            ApprovalResolution::Decide(ApprovalDecision::Approve)
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    let expired = store
        .resolve_approval(
            &command(&actor_id, "late", '8', 100),
            &approval.approval_id,
            1,
            ApprovalResolution::Decide(ApprovalDecision::Approve),
        )
        .unwrap();
    let LedgerOutcome::Applied(expired) = expired else {
        panic!("deadline transition must be applied")
    };
    assert_eq!(expired.state, ApprovalState::Expired);
    assert!(expired.decided_by_actor_id.is_none());
}

#[test]
fn permit_consumption_and_execution_start_are_atomic_and_exactly_bound() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(&command(&actor_id, "issue", '9', 30), &permit)
        .unwrap();

    let exact = claim(&permit, &lease);
    let mut substitutions = Vec::new();
    let mut task = exact.clone();
    task.task_id = TaskId::new();
    substitutions.push(task);
    let mut run = exact.clone();
    run.run_id = RunId::new();
    substitutions.push(run);
    let mut changed_target = exact.clone();
    changed_target.target = TargetRef {
        kind: BoundedName::new("local").unwrap(),
        authority: BoundedName::new("test").unwrap(),
        identifier: BoundedOpaque::new("other-host").unwrap(),
    };
    substitutions.push(changed_target);
    let mut target_identity = exact.clone();
    target_identity.target_identity_digest = digest('9');
    substitutions.push(target_identity);
    let mut runtime_fence = exact.clone();
    runtime_fence.runtime_fence.runtime_generation += 1;
    substitutions.push(runtime_fence);
    let mut operation = exact.clone();
    operation.operation_digest = digest('a');
    substitutions.push(operation);
    let mut input = exact.clone();
    input.input_digest = digest('b');
    substitutions.push(input);
    let mut execution = exact.clone();
    execution.execution_id = ExecutionId::new();
    substitutions.push(execution);
    for (index, substituted) in substitutions.iter().enumerate() {
        let result = store.claim_execution(
            &command(
                &actor_id,
                &format!("substitute-{index}"),
                char::from_digit(u32::try_from(index + 1).unwrap(), 10).unwrap(),
                40,
            ),
            substituted,
        );
        assert!(result.is_err(), "substitution {index} must fail closed");
    }
    let attacker = ActorId::new();
    assert!(store
        .claim_execution(&command(&attacker, "substitute-actor", '7', 40), &exact,)
        .is_err());
    let permit_state: String = store
        .connection()
        .query_row(
            "SELECT state FROM permits WHERE permit_id=?1",
            params![permit.permit_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let execution_state: String = store
        .connection()
        .query_row(
            "SELECT state FROM executions WHERE execution_id=?1",
            params![permit.execution_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        (permit_state.as_str(), execution_state.as_str()),
        ("issued", "planned")
    );

    let started = store
        .claim_execution(&command(&actor_id, "consume", 'b', 40), &exact)
        .unwrap();
    let LedgerOutcome::Applied(started) = started else {
        panic!("consumption must apply")
    };
    assert_eq!(started.state, ExecutionState::Planned);
    assert_eq!(started.broker_state, Some(BrokerExecutionState::Claimed));
    assert_eq!(started.revision, 2);
    assert!(matches!(
        store.claim_execution(
            &command(&actor_id, "reuse", 'c', 41),
            &claim(&permit, &lease)
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
}

#[test]
fn completion_is_revisioned_and_persists_one_evidence_receipt() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(&command(&actor_id, "issue", 'd', 30), &permit)
        .unwrap();
    let started = claim_and_start(&mut store, &actor_id, &permit, &lease, "complete", 40);
    let typed_result = successful_result(&store, &permit.request_id);
    let completion = ExecutionCompletion {
        execution_id: permit.execution_id.clone(),
        expected_revision: started.revision,
        succeeded: true,
        receipt_digest: digest('f'),
        safe_detail: Some(BoundedText::new("completed").unwrap()),
        typed_result: Some(typed_result.clone()),
    };
    let completed = store
        .complete_execution(&command(&actor_id, "complete", 'f', 50), &completion)
        .unwrap();
    let LedgerOutcome::Applied(completed) = completed else {
        panic!("must apply")
    };
    assert_eq!(completed.state, ExecutionState::Succeeded);
    assert_eq!(
        completed.typed_result_state,
        TypedExecutionResultState::Available
    );
    let durable_result = store
        .load_brokered_execution_result(&permit.execution_id)
        .unwrap();
    assert_eq!(durable_result.result, typed_result);
    assert_eq!(durable_result.request_id, permit.request_id);
    assert_eq!(durable_result.execution_id, permit.execution_id);
    assert_eq!(
        durable_result.target_identity_digest,
        permit.target_identity_digest
    );
    assert_eq!(durable_result.runtime_fence, permit.runtime_fence);
    let receipt_count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM execution_receipts WHERE execution_id=?1",
            params![permit.execution_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipt_count, 1);
    let event_count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM task_events
             WHERE task_id=?1 AND event_type='execution_result_recorded'",
            params![permit.task_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count, 1);
    assert!(matches!(
        store
            .complete_execution(&command(&actor_id, "complete", 'f', 50), &completion)
            .unwrap(),
        LedgerOutcome::Replayed(_)
    ));
    assert_eq!(
        store
            .load_brokered_execution_result(&permit.execution_id)
            .unwrap(),
        durable_result
    );
}

#[test]
fn mismatched_or_missing_success_result_rolls_back_completion() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(&command(&actor_id, "typed-issue", '1', 30), &permit)
        .unwrap();
    let started = claim_and_start(&mut store, &actor_id, &permit, &lease, "typed", 40);
    let missing = ExecutionCompletion {
        execution_id: permit.execution_id.clone(),
        expected_revision: started.revision,
        succeeded: true,
        receipt_digest: digest('2'),
        safe_detail: None,
        typed_result: None,
    };
    assert!(matches!(
        store.complete_execution(&command(&actor_id, "typed-missing", '2', 50), &missing),
        Err(StoreError::LedgerConflict { .. })
    ));
    let mismatched =
        BrokeredOperationResult::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1Result {
            checkpoint_id: CheckpointId::new(),
            outcome: WorkspaceCheckpointCreateV1Outcome::Skipped {
                reason: BoundedText::new("substituted").unwrap(),
            },
        });
    let substituted = ExecutionCompletion {
        typed_result: Some(mismatched),
        ..missing
    };
    assert!(matches!(
        store.complete_execution(
            &command(&actor_id, "typed-substituted", '3', 50),
            &substituted
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    assert_eq!(
        store
            .load_execution_record(&permit.execution_id)
            .unwrap()
            .state,
        ExecutionState::Started
    );
    let payload_count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM brokered_execution_results WHERE execution_id=?1",
            params![permit.execution_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(payload_count, 0);
}

#[test]
fn typed_result_insert_failure_rolls_back_execution_receipt_and_task_event() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(&command(&actor_id, "atomic-result-issue", 'a', 30), &permit)
        .unwrap();
    let started = claim_and_start(&mut store, &actor_id, &permit, &lease, "atomic-result", 40);
    store
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_brokered_result
             BEFORE INSERT ON brokered_execution_results
             BEGIN SELECT RAISE(ABORT, 'injected result durability failure'); END;",
        )
        .unwrap();
    let result = successful_result(&store, &permit.request_id);
    assert!(store
        .complete_execution(
            &command(&actor_id, "atomic-result-complete", 'b', 50),
            &ExecutionCompletion {
                execution_id: permit.execution_id.clone(),
                expected_revision: started.revision,
                succeeded: true,
                receipt_digest: digest('c'),
                safe_detail: None,
                typed_result: Some(result),
            },
        )
        .is_err());
    assert_eq!(
        store
            .load_execution_record(&permit.execution_id)
            .unwrap()
            .state,
        ExecutionState::Started
    );
    let receipt_count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM execution_receipts WHERE execution_id=?1",
            params![permit.execution_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let result_event_count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM task_events
             WHERE task_id=?1 AND event_type='execution_result_recorded'",
            params![permit.task_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((receipt_count, result_event_count), (0, 0));
}

#[test]
fn typed_result_substitution_and_missing_payload_fail_closed() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(
            &command(&actor_id, "corrupt-result-issue", '4', 30),
            &permit,
        )
        .unwrap();
    let started = claim_and_start(&mut store, &actor_id, &permit, &lease, "corrupt-result", 40);
    let result = successful_result(&store, &permit.request_id);
    store
        .complete_execution(
            &command(&actor_id, "corrupt-result-complete", '5', 50),
            &ExecutionCompletion {
                execution_id: permit.execution_id.clone(),
                expected_revision: started.revision,
                succeeded: true,
                receipt_digest: digest('6'),
                safe_detail: None,
                typed_result: Some(result),
            },
        )
        .unwrap();
    store
        .connection()
        .execute(
            "UPDATE brokered_execution_results
             SET target_identity_digest=?2 WHERE execution_id=?1",
            params![permit.execution_id.as_str(), digest('9').as_str()],
        )
        .unwrap();
    assert!(matches!(
        store.load_brokered_execution_result(&permit.execution_id),
        Err(StoreError::Corrupt { .. })
    ));
    store
        .connection()
        .execute(
            "DELETE FROM brokered_execution_results WHERE execution_id=?1",
            params![permit.execution_id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        store.load_brokered_execution_result(&permit.execution_id),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn explicit_legacy_success_never_invents_a_typed_result() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(&command(&actor_id, "legacy-result-issue", '7', 30), &permit)
        .unwrap();
    let started = claim_and_start(&mut store, &actor_id, &permit, &lease, "legacy-result", 40);
    let result = successful_result(&store, &permit.request_id);
    store
        .complete_execution(
            &command(&actor_id, "legacy-result-complete", '8', 50),
            &ExecutionCompletion {
                execution_id: permit.execution_id.clone(),
                expected_revision: started.revision,
                succeeded: true,
                receipt_digest: digest('9'),
                safe_detail: None,
                typed_result: Some(result),
            },
        )
        .unwrap();
    store
        .connection()
        .execute(
            "DELETE FROM brokered_execution_results WHERE execution_id=?1",
            params![permit.execution_id.as_str()],
        )
        .unwrap();
    store
        .connection()
        .execute(
            "UPDATE executions SET typed_result_state='legacy_unavailable'
             WHERE execution_id=?1",
            params![permit.execution_id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        store.load_brokered_execution_result(&permit.execution_id),
        Err(StoreError::LegacyBrokeredResultUnavailable { .. })
    ));
}

#[test]
fn live_audit_failure_concludes_claim_with_exact_lease_and_idempotent_receipt() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, task_id, _run_id, approval, lease) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(&command(&actor_id, "issue-audit-failure", '1', 30), &permit)
        .unwrap();
    let LedgerOutcome::Applied(claimed) = store
        .claim_execution(
            &command(&actor_id, "claim-audit-failure", '2', 40),
            &claim(&permit, &lease),
        )
        .unwrap()
    else {
        panic!("execution claim must apply")
    };
    let mut stale = lease.clone();
    stale.revision += 1;
    let detail = BoundedText::new("audit writer was unavailable").unwrap();
    assert!(matches!(
        store.mark_claimed_execution_known_no_effect(
            &command(&actor_id, "stale-audit-failure", '3', 41),
            &permit.execution_id,
            claimed.revision,
            &detail,
            &stale,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));

    let conclude = command(&actor_id, "conclude-audit-failure", '4', 41);
    let LedgerOutcome::Applied(concluded) = store
        .mark_claimed_execution_known_no_effect(
            &conclude,
            &permit.execution_id,
            claimed.revision,
            &detail,
            &lease,
        )
        .unwrap()
    else {
        panic!("known-no-effect conclusion must apply")
    };
    assert_eq!(
        concluded.broker_state,
        Some(BrokerExecutionState::KnownNoEffect)
    );
    assert!(concluded.start_audit_proof_digest.is_none());
    assert!(matches!(
        store
            .mark_claimed_execution_known_no_effect(
                &conclude,
                &permit.execution_id,
                claimed.revision,
                &detail,
                &lease,
            )
            .unwrap(),
        LedgerOutcome::Replayed(_)
    ));
    let result_events: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM task_events
             WHERE task_id=?1
               AND event_type='execution_result_recorded'",
            params![task_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(result_events, 1);
}

#[test]
fn recovery_marks_started_execution_uncertain_without_retry() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(&command(&actor_id, "issue", '1', 30), &permit)
        .unwrap();
    claim_and_start(&mut store, &actor_id, &permit, &lease, "recover", 40);

    let report = store.recover_gateway(60).unwrap();
    assert_eq!(report.executions_uncertain, 1);
    let state: String = store
        .connection()
        .query_row(
            "SELECT state FROM executions WHERE execution_id=?1",
            params![permit.execution_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "uncertain");
    let receipts: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM execution_receipts WHERE execution_id=?1",
            params![permit.execution_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipts, 0);
}

#[test]
fn run_scoped_claimed_recovery_requires_takeover_and_does_not_touch_another_run() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (first_actor, first_task, first_run, first_approval, first_lease) =
        approved_fixture(&mut store);
    let first_permit = permit(&first_approval);
    store
        .issue_permit(
            &command(&first_actor, "issue-first-scoped", '1', 30),
            &first_permit,
        )
        .unwrap();
    store
        .claim_execution(
            &command(&first_actor, "claim-first-scoped", '2', 40),
            &claim(&first_permit, &first_lease),
        )
        .unwrap();

    let (second_actor, _second_task, _second_run, second_approval, second_lease) =
        approved_fixture(&mut store);
    let second_permit = permit(&second_approval);
    store
        .issue_permit(
            &command(&second_actor, "issue-second-scoped", '3', 30),
            &second_permit,
        )
        .unwrap();
    store
        .claim_execution(
            &command(&second_actor, "claim-second-scoped", '4', 40),
            &claim(&second_permit, &second_lease),
        )
        .unwrap();

    assert!(matches!(
        store.recover_brokered_executions_for_run(&first_run, 50),
        Err(StoreError::LedgerConflict { .. })
    ));
    let takeover = acquire_lease(
        &mut store,
        &first_actor,
        &first_task,
        &first_run,
        "scoped-claimed-takeover",
        201,
        400,
    );
    assert!(takeover.generation > first_lease.generation);
    let report = store
        .recover_brokered_executions_for_run(&first_run, 202)
        .unwrap();
    assert_eq!(report.executions_known_no_effect, 1);
    assert_eq!(report.executions_uncertain, 0);
    assert_eq!(
        store
            .load_execution_record(&first_permit.execution_id)
            .unwrap()
            .broker_state,
        Some(BrokerExecutionState::KnownNoEffect)
    );
    assert_eq!(
        store
            .load_execution_record(&second_permit.execution_id)
            .unwrap()
            .broker_state,
        Some(BrokerExecutionState::Claimed)
    );
    assert_eq!(
        store
            .recover_brokered_executions_for_run(&first_run, 203)
            .unwrap(),
        BrokeredExecutionRecoveryReport::default()
    );
}

#[test]
fn run_scoped_started_recovery_is_atomic_and_idempotent() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, task_id, run_id, approval, lease) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(
            &command(&actor_id, "issue-started-scoped", '5', 30),
            &permit,
        )
        .unwrap();
    claim_and_start(&mut store, &actor_id, &permit, &lease, "started-scoped", 40);
    let takeover = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "scoped-started-takeover",
        201,
        400,
    );
    assert!(takeover.generation > lease.generation);

    let report = store
        .recover_brokered_executions_for_run(&run_id, 202)
        .unwrap();
    assert_eq!(report.executions_known_no_effect, 0);
    assert_eq!(report.executions_uncertain, 1);
    assert_eq!(
        store
            .load_execution_record(&permit.execution_id)
            .unwrap()
            .state,
        ExecutionState::Uncertain
    );
    assert_eq!(
        store
            .recover_brokered_executions_for_run(&run_id, 203)
            .unwrap(),
        BrokeredExecutionRecoveryReport::default()
    );
}

#[test]
fn takeover_can_complete_started_effect_from_original_request_evidence() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, task_id, run_id, approval, lease) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(
            &command(&actor_id, "issue-evidence-recovery", '6', 30),
            &permit,
        )
        .unwrap();
    let started = claim_and_start(
        &mut store,
        &actor_id,
        &permit,
        &lease,
        "evidence-recovery",
        40,
    );
    let takeover = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "evidence-recovery-takeover",
        201,
        400,
    );

    let candidates = store
        .load_started_brokered_recovery_candidates(&takeover, 202)
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].execution, started);
    assert_eq!(candidates[0].request.request.request_id, permit.request_id);

    let typed_result = successful_result(&store, &permit.request_id);
    store
        .complete_execution(
            &command(&actor_id, "complete-evidence-recovery", '7', 202),
            &ExecutionCompletion {
                execution_id: permit.execution_id.clone(),
                expected_revision: started.revision,
                succeeded: true,
                receipt_digest: digest('7'),
                safe_detail: None,
                typed_result: Some(typed_result),
            },
        )
        .unwrap();
    assert_eq!(
        store
            .load_execution_record(&permit.execution_id)
            .unwrap()
            .state,
        ExecutionState::Succeeded
    );
    assert!(store
        .load_started_brokered_recovery_candidates(&takeover, 203)
        .unwrap()
        .is_empty());
}

fn runtime_binding(task_id: &TaskId, run_id: &RunId, generation: u64) -> RuntimeBindingRef {
    RuntimeBindingRef {
        binding_id: RuntimeBindingId::new(),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        agent_session_id: AgentSessionId::new(),
        runtime_instance_id: RuntimeInstanceId::new(),
        runtime_generation: generation,
        external_session: ExternalRef {
            kind: ExternalRefKind::AcpSession,
            authority: BoundedName::new("test").unwrap(),
            scope_digest: digest('3'),
            value: BoundedOpaque::new("session").unwrap(),
        },
    }
}

fn provider_permission(
    approval: &ApprovalRecord,
    binding: &RuntimeBindingRef,
    event_sequence: u64,
) -> RuntimePermissionRef {
    RuntimePermissionRef {
        binding_id: binding.binding_id.clone(),
        runtime_generation: binding.runtime_generation,
        event_sequence,
        run_id: approval.run_id.clone(),
        turn_id: TurnId::new(),
        tool_use_id: Some(ToolUseId::new()),
        request_id: approval.request_id.clone(),
        callback: Some(
            cosh_gateway_contracts::runtime::ProviderPermissionCallbackV2 {
                provider_session_digest: digest('a'),
                provider_request_id_digest: digest('b'),
                provider_tool_call_id_digest: digest('c'),
                ordered_option_set_digest: digest('d'),
                callback_payload_digest: digest('e'),
                normalized_operation_digest: approval.operation_digest.clone(),
            },
        ),
        core_callback: None,
    }
}

fn provider_approval_request(approval: &ApprovalRecord) -> ApprovalRequest {
    ApprovalRequest {
        approval_id: approval.approval_id.clone(),
        request_id: approval.request_id.clone(),
        task_id: approval.task_id.clone(),
        run_id: approval.run_id.clone(),
        summary: BoundedText::new("approve provider tool").unwrap(),
        expires_at_ms: approval.expires_at_ms,
    }
}

#[test]
fn provider_permission_resolution_is_exact_and_dispatch_start_is_not_replayed() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "provider-lease",
        5,
        100,
    );
    let binding = runtime_binding(&task_id, &run_id, lease.generation);
    store
        .bind_runtime(
            &command(&actor_id, "provider-bind", '4', 10),
            &binding,
            &lease,
        )
        .unwrap();
    let mut approval = approval(&actor_id, &task_id, &run_id);
    approval.created_at_ms = 12;
    approval.updated_at_ms = 12;
    let permission = provider_permission(&approval, &binding, 1);
    approval.permission = Some(permission.clone());
    assert!(matches!(
        store.create_approval(&command(&actor_id, "provider-unfenced", '4', 12), &approval,),
        Err(StoreError::LedgerConflict { .. })
    ));
    store
        .create_provider_approval(
            &command(&actor_id, "provider-pending", '5', 12),
            &provider_approval_request(&approval),
            &approval,
            &binding,
            &lease,
        )
        .unwrap();
    let waiting = store.load_task(&task_id).unwrap();
    assert_eq!(waiting.state(), TaskState::WaitingApproval);
    assert_eq!(waiting.revision(), 4);
    let durable_request_rows = store
        .connection()
        .query_row(
            "SELECT (
                 SELECT COUNT(*) FROM task_events
                 WHERE task_id=?1 AND event_type='approval_requested'
             ) + (
                 SELECT COUNT(*) FROM outbox
                 WHERE task_id=?1 AND delivery_kind='provider_approval_request'
             )",
            params![task_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(durable_request_rows, 2);

    let resolved = store
        .resolve_provider_permission(
            &command(&actor_id, "provider-resolve", '6', 20),
            &approval.approval_id,
            1,
            ApprovalResolution::Decide(ApprovalDecision::Approve),
            &permission,
            &lease,
        )
        .unwrap();
    let LedgerOutcome::Applied(prepared) = resolved else {
        panic!("provider resolution must apply")
    };
    let running = store.load_task(&task_id).unwrap();
    assert_eq!(running.state(), TaskState::Running);
    assert_eq!(running.revision(), 5);
    let resolved_events = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM task_events
             WHERE task_id=?1 AND event_type='approval_resolved'",
            params![task_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(resolved_events, 1);
    assert_eq!(prepared.state, ProviderPermissionDispatchState::Prepared);
    assert_eq!(
        prepared.decision,
        ProviderPermissionDispatchDecision::AllowOnce
    );
    assert_eq!(
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM permits", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );

    let start_command = command(&actor_id, "provider-start", '7', 21);
    let started = store
        .start_provider_permission_dispatch(&start_command, &approval.approval_id, 1, &lease)
        .unwrap();
    assert!(matches!(
        started,
        LedgerOutcome::Applied(ProviderPermissionDispatchRecord {
            state: ProviderPermissionDispatchState::WriteStarted,
            ..
        })
    ));
    assert!(matches!(
        store
            .start_provider_permission_dispatch(&start_command, &approval.approval_id, 1, &lease)
            .unwrap(),
        LedgerOutcome::Replayed(ProviderPermissionDispatchRecord {
            state: ProviderPermissionDispatchState::WriteStarted,
            ..
        })
    ));
    assert!(store
        .start_provider_permission_dispatch(
            &command(&actor_id, "provider-start-again", '8', 22),
            &approval.approval_id,
            2,
            &lease,
        )
        .is_err());
}

#[test]
fn provider_approval_event_failure_rolls_back_the_record_and_outbox() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "provider-create-rollback",
        5,
        100,
    );
    let binding = runtime_binding(&task_id, &run_id, lease.generation);
    store
        .bind_runtime(
            &command(&actor_id, "provider-create-rollback-bind", '1', 10),
            &binding,
            &lease,
        )
        .unwrap();
    let mut approval = approval(&actor_id, &task_id, &run_id);
    approval.created_at_ms = 12;
    approval.updated_at_ms = 12;
    approval.permission = Some(provider_permission(&approval, &binding, 1));
    store
        .connection()
        .execute_batch(
            "CREATE TEMP TRIGGER fail_provider_approval_event
             BEFORE INSERT ON task_events
             WHEN NEW.event_type='approval_requested'
             BEGIN SELECT RAISE(ABORT, 'injected approval event failure'); END;",
        )
        .unwrap();

    assert!(store
        .create_provider_approval(
            &command(&actor_id, "provider-create-rollback", '2', 12),
            &provider_approval_request(&approval),
            &approval,
            &binding,
            &lease,
        )
        .is_err());
    assert_eq!(
        store.load_task(&task_id).unwrap().state(),
        TaskState::Running
    );
    assert_eq!(
        store
            .load_runtime_binding_record(&binding.binding_id)
            .unwrap()
            .last_sequence,
        0
    );
    let rows = store
        .connection()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM approvals WHERE approval_id=?1)
                  + (SELECT COUNT(*) FROM outbox WHERE task_id=?2)
                  + (SELECT COUNT(*) FROM task_events
                     WHERE task_id=?2 AND event_type='approval_requested')",
            params![approval.approval_id.as_str(), task_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn provider_dispatch_insert_failure_rolls_back_the_decision_and_task_event() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "provider-resolve-rollback",
        5,
        100,
    );
    let binding = runtime_binding(&task_id, &run_id, lease.generation);
    store
        .bind_runtime(
            &command(&actor_id, "provider-resolve-rollback-bind", '3', 10),
            &binding,
            &lease,
        )
        .unwrap();
    let mut approval = approval(&actor_id, &task_id, &run_id);
    approval.created_at_ms = 12;
    approval.updated_at_ms = 12;
    let permission = provider_permission(&approval, &binding, 1);
    approval.permission = Some(permission.clone());
    store
        .create_provider_approval(
            &command(&actor_id, "provider-resolve-rollback-create", '4', 12),
            &provider_approval_request(&approval),
            &approval,
            &binding,
            &lease,
        )
        .unwrap();
    store
        .connection()
        .execute_batch(
            "CREATE TEMP TRIGGER fail_provider_dispatch_insert
             BEFORE INSERT ON provider_permission_dispatches
             BEGIN SELECT RAISE(ABORT, 'injected dispatch failure'); END;",
        )
        .unwrap();

    assert!(store
        .resolve_provider_permission(
            &command(&actor_id, "provider-resolve-rollback", '5', 20),
            &approval.approval_id,
            1,
            ApprovalResolution::Decide(ApprovalDecision::Approve),
            &permission,
            &lease,
        )
        .is_err());
    assert_eq!(
        store
            .load_approval_record(&approval.approval_id)
            .unwrap()
            .state,
        ApprovalState::Pending
    );
    assert_eq!(
        store.load_task(&task_id).unwrap().state(),
        TaskState::WaitingApproval
    );
    let resolved_rows = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM task_events
             WHERE task_id=?1 AND event_type='approval_resolved'",
            params![task_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(resolved_rows, 0);
}

#[test]
fn provider_abandonment_atomically_closes_the_exact_callback_and_sequence() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "provider-abandon",
        5,
        100,
    );
    let binding = runtime_binding(&task_id, &run_id, lease.generation);
    store
        .bind_runtime(
            &command(&actor_id, "provider-abandon-bind", '6', 10),
            &binding,
            &lease,
        )
        .unwrap();
    let mut approval = approval(&actor_id, &task_id, &run_id);
    approval.created_at_ms = 12;
    approval.updated_at_ms = 12;
    let permission = provider_permission(&approval, &binding, 1);
    approval.permission = Some(permission.clone());
    store
        .create_provider_approval(
            &command(&actor_id, "provider-abandon-create", '7', 12),
            &provider_approval_request(&approval),
            &approval,
            &binding,
            &lease,
        )
        .unwrap();

    assert!(store
        .abandon_provider_permission(
            &command(&actor_id, "provider-abandon-wrong-sequence", '8', 13),
            &approval.approval_id,
            &permission,
            &binding,
            3,
            &lease,
            ApprovalAbandonCause::ProviderCancelled,
        )
        .is_err());
    let abandoned = store
        .abandon_provider_permission(
            &command(&actor_id, "provider-abandon-exact", '9', 13),
            &approval.approval_id,
            &permission,
            &binding,
            2,
            &lease,
            ApprovalAbandonCause::ProviderCancelled,
        )
        .unwrap();
    assert!(matches!(
        abandoned,
        LedgerOutcome::Applied(ApprovalRecord {
            state: ApprovalState::Cancelled,
            ..
        })
    ));
    assert_eq!(
        store.load_task(&task_id).unwrap().state(),
        TaskState::Running
    );
    assert_eq!(
        store
            .load_runtime_binding_record(&binding.binding_id)
            .unwrap()
            .last_sequence,
        2
    );
    let abandoned_events = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM task_events
             WHERE task_id=?1 AND event_type='approval_abandoned'",
            params![task_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(abandoned_events, 1);
}

#[test]
fn provider_permission_rejects_wrong_turn_actor_and_generation() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "provider-fence",
        5,
        100,
    );
    let binding = runtime_binding(&task_id, &run_id, lease.generation);
    store
        .bind_runtime(
            &command(&actor_id, "provider-fence-bind", '9', 10),
            &binding,
            &lease,
        )
        .unwrap();
    let mut approval = approval(&actor_id, &task_id, &run_id);
    approval.created_at_ms = 12;
    approval.updated_at_ms = 12;
    let permission = provider_permission(&approval, &binding, 1);
    approval.permission = Some(permission.clone());
    let mut future_callback = approval.clone();
    future_callback.permission.as_mut().unwrap().event_sequence = 2;
    assert!(matches!(
        store.create_provider_approval(
            &command(&actor_id, "provider-unrecorded-sequence", '9', 12),
            &provider_approval_request(&future_callback),
            &future_callback,
            &binding,
            &lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    store
        .create_provider_approval(
            &command(&actor_id, "provider-fence-pending", 'a', 12),
            &provider_approval_request(&approval),
            &approval,
            &binding,
            &lease,
        )
        .unwrap();

    let mut wrong_turn = permission.clone();
    wrong_turn.turn_id = TurnId::new();
    assert!(store
        .resolve_provider_permission(
            &command(&actor_id, "wrong-turn", 'b', 20),
            &approval.approval_id,
            1,
            ApprovalResolution::Decide(ApprovalDecision::Approve),
            &wrong_turn,
            &lease,
        )
        .is_err());
    assert!(store
        .resolve_provider_permission(
            &command(&ActorId::new(), "wrong-actor", 'c', 20),
            &approval.approval_id,
            1,
            ApprovalResolution::Decide(ApprovalDecision::Approve),
            &permission,
            &lease,
        )
        .is_err());
    let mut wrong_generation = permission.clone();
    wrong_generation.runtime_generation += 1;
    assert!(store
        .resolve_provider_permission(
            &command(&actor_id, "wrong-generation", 'd', 20),
            &approval.approval_id,
            1,
            ApprovalResolution::Decide(ApprovalDecision::Approve),
            &wrong_generation,
            &lease,
        )
        .is_err());
    assert_eq!(
        store
            .load_approval_record(&approval.approval_id)
            .unwrap()
            .state,
        ApprovalState::Pending
    );
}

#[test]
fn provider_approval_expiry_is_deadline_fenced_and_idempotent() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "provider-expiry-lease",
        5,
        100,
    );
    let binding = runtime_binding(&task_id, &run_id, lease.generation);
    store
        .bind_runtime(
            &command(&actor_id, "provider-expiry-bind", 'e', 10),
            &binding,
            &lease,
        )
        .unwrap();
    let mut approval = approval(&actor_id, &task_id, &run_id);
    approval.created_at_ms = 12;
    approval.updated_at_ms = 12;
    approval.expires_at_ms = 80;
    let permission = provider_permission(&approval, &binding, 1);
    approval.permission = Some(permission.clone());
    store
        .create_provider_approval(
            &command(&actor_id, "provider-expiry-pending", 'f', 12),
            &provider_approval_request(&approval),
            &approval,
            &binding,
            &lease,
        )
        .unwrap();

    assert!(matches!(
        store.expire_provider_approval(
            &command(&actor_id, "provider-expiry-early", '0', 79),
            &approval.approval_id,
            &permission,
            &lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    assert!(matches!(
        store.resolve_provider_permission(
            &command(&actor_id, "provider-resolution-after-deadline", '1', 80),
            &approval.approval_id,
            1,
            ApprovalResolution::Decide(ApprovalDecision::Deny),
            &permission,
            &lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    assert_eq!(
        store
            .load_approval_record(&approval.approval_id)
            .unwrap()
            .state,
        ApprovalState::Pending
    );
    assert_eq!(
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM provider_permission_dispatches WHERE approval_id=?1",
                params![approval.approval_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    let mut wrong_lease = lease.clone();
    wrong_lease.revision += 1;
    assert!(matches!(
        store.expire_provider_approval(
            &command(&actor_id, "provider-expiry-wrong-lease", '2', 80),
            &approval.approval_id,
            &permission,
            &wrong_lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));

    let expire = command(&actor_id, "provider-expiry", '3', 80);
    let LedgerOutcome::Applied(expired) = store
        .expire_provider_approval(&expire, &approval.approval_id, &permission, &lease)
        .unwrap()
    else {
        panic!("first expiry must apply")
    };
    assert_eq!(expired.state, ApprovalState::Expired);
    assert_eq!(expired.revision, 2);
    assert!(expired.decided_by_actor_id.is_none());
    assert!(matches!(
        store
            .expire_provider_approval(&expire, &approval.approval_id, &permission, &lease)
            .unwrap(),
        LedgerOutcome::Replayed(ApprovalRecord {
            state: ApprovalState::Expired,
            revision: 2,
            ..
        })
    ));
    assert_eq!(
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM provider_permission_dispatches WHERE approval_id=?1",
                params![approval.approval_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn run_scoped_recovery_does_not_mutate_another_run() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let first_run = RunId::new();
    let second_run = RunId::new();
    let first_task = create_task(&mut store, &actor_id, &first_run);
    let second_task = create_task(&mut store, &actor_id, &second_run);
    let mut first = approval(&actor_id, &first_task, &first_run);
    let mut second = approval(&actor_id, &second_task, &second_run);
    let first_binding = runtime_binding(&first_task, &first_run, 1);
    let second_binding = runtime_binding(&second_task, &second_run, 1);
    let first_permission = provider_permission(&first, &first_binding, 1);
    let second_permission = provider_permission(&second, &second_binding, 1);
    store
        .create_approval(&command(&actor_id, "recover-first", '1', 10), &first)
        .unwrap();
    store
        .create_approval(&command(&actor_id, "recover-second", '2', 10), &second)
        .unwrap();
    first.permission = Some(first_permission);
    second.permission = Some(second_permission);
    for approval in [&first, &second] {
        store
            .connection()
            .execute(
                "UPDATE approvals SET permission_ref_json=?2 WHERE approval_id=?1",
                params![
                    approval.approval_id.as_str(),
                    serde_json::to_string(approval.permission.as_ref().unwrap()).unwrap(),
                ],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO provider_permission_dispatches(
                     approval_id, actor_id, task_id, run_id, permission_ref_json,
                     decision, state, revision, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'deny', 'prepared', 1, 11, 11)",
                params![
                    approval.approval_id.as_str(),
                    actor_id.as_str(),
                    approval.task_id.as_str(),
                    approval.run_id.as_str(),
                    serde_json::to_string(approval.permission.as_ref().unwrap()).unwrap(),
                ],
            )
            .unwrap();
    }

    assert_eq!(
        store
            .cancel_pending_approvals_for_run(&first_run, 20)
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .mark_provider_dispatches_unknown_for_run(&first_run, 20)
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .load_approval_record(&first.approval_id)
            .unwrap()
            .state,
        ApprovalState::Cancelled
    );
    assert_eq!(
        store
            .load_approval_record(&second.approval_id)
            .unwrap()
            .state,
        ApprovalState::Pending
    );
    assert_eq!(
        store
            .load_provider_permission_dispatch_record(&first.approval_id)
            .unwrap()
            .state,
        ProviderPermissionDispatchState::Abandoned
    );
    assert_eq!(
        store
            .load_provider_permission_dispatch_record(&second.approval_id)
            .unwrap()
            .state,
        ProviderPermissionDispatchState::Prepared
    );
}

#[test]
fn restart_marks_only_active_written_provider_responses_unknown() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let active_run = RunId::new();
    let terminal_run = RunId::new();
    let active_task = create_task(&mut store, &actor_id, &active_run);
    let terminal_task = create_task(&mut store, &actor_id, &terminal_run);
    append_task_event(
        &mut store,
        &actor_id,
        &terminal_task,
        4,
        "terminal-run-succeeded",
        2,
        TaskEvent::RunSucceeded {
            run_id: terminal_run.clone(),
        },
    );
    append_task_event(
        &mut store,
        &actor_id,
        &terminal_task,
        5,
        "terminal-task-succeeded",
        3,
        TaskEvent::TaskSucceeded,
    );

    let rows = [
        (
            ApprovalId::new(),
            RequestId::new(),
            &active_task,
            &active_run,
        ),
        (
            ApprovalId::new(),
            RequestId::new(),
            &terminal_task,
            &terminal_run,
        ),
    ];
    for (approval_id, request_id, task_id, run_id) in &rows {
        store
            .connection()
            .execute(
                "INSERT INTO approvals(
                     approval_id, request_id, actor_id, task_id, run_id, target_json,
                     operation_digest, input_digest, state, revision, expires_at_ms,
                     decided_by_actor_id, created_at_ms, updated_at_ms,
                     permission_ref_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, '{}', ?6, ?6, 'approved', 2, 100,
                         ?3, 1, 1, '{}')",
                params![
                    approval_id.as_str(),
                    request_id.as_str(),
                    actor_id.as_str(),
                    task_id.as_str(),
                    run_id.as_str(),
                    digest('7').as_str(),
                ],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO provider_permission_dispatches(
                     approval_id, actor_id, task_id, run_id, permission_ref_json,
                     decision, state, revision, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, '{}', 'allow_once', 'written', 3, 1, 1)",
                params![
                    approval_id.as_str(),
                    actor_id.as_str(),
                    task_id.as_str(),
                    run_id.as_str(),
                ],
            )
            .unwrap();
    }

    let report = store.recover_gateway(10).unwrap();
    assert_eq!(report.permission_dispatches_unknown, 1);
    assert_eq!(report.permission_dispatches_abandoned, 0);
    let states = rows
        .iter()
        .map(|(approval_id, _, _, _)| {
            store
                .connection()
                .query_row(
                    "SELECT state FROM provider_permission_dispatches WHERE approval_id=?1",
                    params![approval_id.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(states, ["unknown", "written"]);
}

#[test]
fn expired_active_lease_requires_delivered_start_and_run_scoped_binding_fence() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "expired-active",
        5,
        20,
    );
    assert!(store.load_expired_active_lease(20).unwrap().is_none());
    let event_id: String = store
        .connection()
        .query_row(
            "SELECT event_id FROM task_events WHERE task_id=?1 ORDER BY revision DESC LIMIT 1",
            params![task_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO outbox(
                 delivery_id, task_id, event_id, delivery_kind, payload_json,
                 state, attempt, next_attempt_at_ms, created_at_ms, delivered_at_ms)
             VALUES (?1, ?2, ?3, 'runtime_start', ?4, 'delivered', 1, 1, 1, 2)",
            params![
                DeliveryId::new().as_str(),
                task_id.as_str(),
                event_id,
                serde_json::json!({ "run_id": run_id }).to_string(),
            ],
        )
        .unwrap();
    let recovered = store.load_expired_active_lease(20).unwrap().unwrap();
    assert_eq!(recovered.run_id, run_id);
    assert_eq!(recovered.generation, lease.generation);

    let binding = runtime_binding(&task_id, &run_id, lease.generation);
    // Binding must have been recorded before the lease deadline.
    store
        .bind_runtime(
            &command(&actor_id, "expired-bind", '3', 10),
            &binding,
            &lease,
        )
        .unwrap();
    assert_eq!(
        store
            .mark_runtime_bindings_lost_for_run(&run_id, 20)
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .load_runtime_binding_record(&binding.binding_id)
            .unwrap()
            .state,
        RuntimeBindingState::Lost
    );
}

#[test]
fn runtime_generation_and_sequence_fence_stale_output() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(&mut store, &actor_id, &task_id, &run_id, "runtime", 5, 20);
    let first = runtime_binding(&task_id, &run_id, 1);
    store
        .bind_runtime(&command(&actor_id, "bind-1", '4', 10), &first, &lease)
        .unwrap();
    store
        .record_runtime_sequence(
            &first.binding_id,
            &first.runtime_instance_id,
            1,
            1,
            11,
            &lease,
        )
        .unwrap();
    assert!(matches!(
        store.record_runtime_sequence(
            &first.binding_id,
            &first.runtime_instance_id,
            1,
            1,
            12,
            &lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    let next_lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "runtime-next",
        20,
        100,
    );
    let second = runtime_binding(&task_id, &run_id, next_lease.generation);
    store
        .bind_runtime(&command(&actor_id, "bind-2", '5', 21), &second, &next_lease)
        .unwrap();
    assert!(matches!(
        store.record_runtime_sequence(
            &first.binding_id,
            &first.runtime_instance_id,
            1,
            2,
            22,
            &lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    let stale_generation = runtime_binding(&task_id, &run_id, 1);
    assert!(matches!(
        store.bind_runtime(
            &command(&actor_id, "stale", '6', 22),
            &stale_generation,
            &next_lease,
        ),
        Err(StoreError::GenerationFenced {
            expected: 2,
            actual: 1
        })
    ));
}

#[test]
fn expired_run_lease_takeover_increments_fencing_generation() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let first = LeaseCommand {
        command: command(&actor_id, "lease-1", '7', 10),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        lease_owner: BoundedOpaque::new("coordinator-a").unwrap(),
        expires_at_ms: 20,
    };
    let LedgerOutcome::Applied(first_record) = store.acquire_run_lease(&first).unwrap() else {
        panic!("first lease must apply")
    };
    assert_eq!(first_record.generation, 1);
    let renewal = LeaseCommand {
        command: command(&actor_id, "lease-renew", 'a', 12),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        lease_owner: BoundedOpaque::new("coordinator-a").unwrap(),
        expires_at_ms: 22,
    };
    let LedgerOutcome::Applied(renewed) = store
        .renew_run_lease(&renewal, first_record.generation, first_record.revision)
        .unwrap()
    else {
        panic!("renewal must apply")
    };
    assert_eq!(renewed.generation, 1);
    assert_eq!(renewed.revision, 2);
    let held = LeaseCommand {
        command: command(&actor_id, "lease-held", '8', 21),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        lease_owner: BoundedOpaque::new("coordinator-b").unwrap(),
        expires_at_ms: 30,
    };
    assert!(matches!(
        store.acquire_run_lease(&held),
        Err(StoreError::LedgerConflict { .. })
    ));
    let takeover = LeaseCommand {
        command: command(&actor_id, "lease-2", '9', 22),
        task_id,
        run_id,
        lease_owner: BoundedOpaque::new("coordinator-b").unwrap(),
        expires_at_ms: 30,
    };
    let LedgerOutcome::Applied(second_record) = store.acquire_run_lease(&takeover).unwrap() else {
        panic!("takeover must apply")
    };
    assert_eq!(second_record.generation, 2);
    assert_eq!(second_record.revision, 3);
    assert_eq!(
        store.load_run_lease(&second_record.run_id).unwrap(),
        second_record
    );
}

#[test]
fn released_run_lease_can_be_reacquired_only_with_a_new_generation() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let acquired = LeaseCommand {
        command: command(&actor_id, "lease-acquire", 'b', 10),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        lease_owner: BoundedOpaque::new("coordinator-a").unwrap(),
        expires_at_ms: 30,
    };
    let LedgerOutcome::Applied(first) = store.acquire_run_lease(&acquired).unwrap() else {
        panic!("lease must apply")
    };
    let released = store
        .release_run_lease(
            &command(&actor_id, "lease-release", 'c', 15),
            &LeaseClaim {
                task_id: task_id.clone(),
                run_id: run_id.clone(),
                lease_owner: first.lease_owner,
                generation: first.generation,
                revision: first.revision,
            },
        )
        .unwrap();
    let LedgerOutcome::Applied(released) = released else {
        panic!("release must apply")
    };
    assert_eq!(released.expires_at_ms, 15);
    let reacquire = LeaseCommand {
        command: command(&actor_id, "lease-reacquire", 'd', 15),
        task_id,
        run_id,
        lease_owner: BoundedOpaque::new("coordinator-b").unwrap(),
        expires_at_ms: 40,
    };
    let LedgerOutcome::Applied(second) = store.acquire_run_lease(&reacquire).unwrap() else {
        panic!("reacquire must apply")
    };
    assert_eq!(second.generation, 2);
}

#[test]
fn stale_lease_and_cross_task_run_claims_roll_back_atomically() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, task_id, run_id, approval, stale) = approved_fixture(&mut store);
    let renewal = LeaseCommand {
        command: command(&actor_id, "renew-stale", '2', 30),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        lease_owner: stale.lease_owner.clone(),
        expires_at_ms: 80,
    };
    store
        .renew_run_lease(&renewal, stale.generation, stale.revision)
        .unwrap();
    let permit = permit(&approval);
    store
        .issue_permit(&command(&actor_id, "issue-stale", '3', 35), &permit)
        .unwrap();
    assert!(matches!(
        store.claim_execution(
            &command(&actor_id, "consume-stale", '4', 40),
            &claim(&permit, &stale),
        ),
        Err(StoreError::LedgerConflict { .. })
    ));

    let other_run = RunId::new();
    let _other_task = create_task(&mut store, &actor_id, &other_run);
    let mut substituted = claim(&permit, &stale);
    substituted.run_id = other_run;
    assert!(matches!(
        store.claim_execution(
            &command(&actor_id, "consume-other-run", '5', 40),
            &substituted,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));

    let states = store
        .connection()
        .query_row(
            "SELECT p.state, e.state FROM permits p JOIN executions e
             ON e.execution_id=p.execution_id WHERE p.permit_id=?1",
            params![permit.permit_id.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap();
    assert_eq!(states, ("issued".to_owned(), "planned".to_owned()));
}

#[test]
fn lease_renewal_keeps_generation_authority_but_takeover_fences_it() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, task_id, run_id, approval, observed) = approved_fixture(&mut store);
    let LedgerOutcome::Applied(renewed) = store
        .renew_run_lease(
            &LeaseCommand {
                command: command(&actor_id, "renew-authority", '3', 30),
                task_id: task_id.clone(),
                run_id: run_id.clone(),
                lease_owner: observed.lease_owner.clone(),
                expires_at_ms: 220,
            },
            observed.generation,
            observed.revision,
        )
        .unwrap()
    else {
        panic!("renewal must apply")
    };
    let current = LeaseClaim {
        task_id: renewed.task_id,
        run_id: renewed.run_id,
        lease_owner: renewed.lease_owner,
        generation: renewed.generation,
        revision: renewed.revision,
    };
    assert_eq!(current.generation, observed.generation);
    assert_ne!(current.revision, observed.revision);

    let permit = permit(&approval);
    store
        .issue_permit(&command(&actor_id, "issue-renewed", '4', 35), &permit)
        .unwrap();
    assert!(matches!(
        store.claim_execution(
            &command(&actor_id, "claim-observed-revision", '5', 40),
            &claim(&permit, &current),
        ),
        Ok(LedgerOutcome::Applied(_))
    ));
}

#[test]
fn lease_generation_takeover_fences_an_already_issued_permit() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, task_id, run_id, approval, observed) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(
            &command(&actor_id, "issue-before-takeover", '6', 30),
            &permit,
        )
        .unwrap();
    store
        .connection()
        .execute(
            "UPDATE run_leases SET expires_at_ms=40 WHERE run_id=?1",
            [run_id.as_str()],
        )
        .unwrap();
    let LedgerOutcome::Applied(taken_over) = store
        .acquire_run_lease(&LeaseCommand {
            command: command(&actor_id, "takeover", '7', 40),
            task_id,
            run_id,
            lease_owner: BoundedOpaque::new("takeover-owner").unwrap(),
            expires_at_ms: 100,
        })
        .unwrap()
    else {
        panic!("takeover must apply")
    };
    let takeover = LeaseClaim {
        task_id: taken_over.task_id,
        run_id: taken_over.run_id,
        lease_owner: taken_over.lease_owner,
        generation: taken_over.generation,
        revision: taken_over.revision,
    };
    assert_ne!(takeover.generation, observed.generation);
    assert!(matches!(
        store.claim_execution(
            &command(&actor_id, "claim-after-takeover", '8', 45),
            &claim(&permit, &takeover),
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    assert_eq!(
        store.load_permit_record(&permit.permit_id).unwrap().state,
        PermitState::Issued
    );
}

#[test]
fn permit_deadline_cannot_outlive_approval_and_overflow_rolls_back() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, task_id, run_id, approved, _lease) = approved_fixture(&mut store);
    let mut widened = permit(&approved);
    widened.valid_until_ms = approved.expires_at_ms + 1;
    assert!(matches!(
        store.issue_permit(&command(&actor_id, "issue-wide", '6', 30), &widened),
        Err(StoreError::LedgerConflict { .. })
    ));

    let mut overflow = approval(&actor_id, &task_id, &run_id);
    overflow.approval_id = ApprovalId::new();
    overflow.request_id = RequestId::new();
    overflow.expires_at_ms = u64::MAX;
    assert!(matches!(
        store.create_approval(&command(&actor_id, "create-overflow", '7', 40), &overflow,),
        Err(StoreError::LedgerConflict { .. })
    ));

    let (permits, executions, approvals): (i64, i64, i64) = store
        .connection()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM permits),
                    (SELECT COUNT(*) FROM executions),
                    (SELECT COUNT(*) FROM approvals WHERE approval_id=?1)",
            params![overflow.approval_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((permits, executions, approvals), (0, 0, 0));
}

#[test]
fn runtime_acceptance_requires_current_lease_and_monotonic_generation() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let stale = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "runtime-stale",
        5,
        30,
    );
    let first = runtime_binding(&task_id, &run_id, 1);
    store
        .bind_runtime(&command(&actor_id, "bind-first", '8', 10), &first, &stale)
        .unwrap();
    let skipped = runtime_binding(&task_id, &run_id, 3);
    assert!(matches!(
        store.bind_runtime(&command(&actor_id, "bind-skip", '9', 11), &skipped, &stale,),
        Err(StoreError::GenerationFenced {
            expected: 1,
            actual: 3
        })
    ));

    let renewal = LeaseCommand {
        command: command(&actor_id, "runtime-renew", 'a', 12),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        lease_owner: stale.lease_owner.clone(),
        expires_at_ms: 40,
    };
    store
        .renew_run_lease(&renewal, stale.generation, stale.revision)
        .unwrap();
    assert!(matches!(
        store.record_runtime_sequence(
            &first.binding_id,
            &first.runtime_instance_id,
            1,
            1,
            13,
            &stale,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    assert_eq!(
        store
            .load_runtime_binding_record(&first.binding_id)
            .unwrap()
            .last_sequence,
        0
    );
}

#[test]
fn runtime_binding_allows_lease_generation_gap_after_pre_bind_crash() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let first_lease = acquire_lease(&mut store, &actor_id, &task_id, &run_id, "gap-first", 5, 20);
    let first = runtime_binding(&task_id, &run_id, first_lease.generation);
    store
        .bind_runtime(
            &command(&actor_id, "gap-bind-first", 'b', 10),
            &first,
            &first_lease,
        )
        .unwrap();

    let crashed_lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "gap-crashed",
        20,
        30,
    );
    assert_eq!(crashed_lease.generation, 2);
    let recovered_lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "gap-recovered",
        30,
        100,
    );
    assert_eq!(recovered_lease.generation, 3);

    let recovered = runtime_binding(&task_id, &run_id, recovered_lease.generation);
    let outcome = store
        .bind_runtime(
            &command(&actor_id, "gap-bind-recovered", 'c', 31),
            &recovered,
            &recovered_lease,
        )
        .unwrap();
    assert!(matches!(outcome, LedgerOutcome::Applied(_)));
    assert_eq!(
        store
            .load_runtime_binding_record(&first.binding_id)
            .unwrap()
            .state,
        RuntimeBindingState::Lost
    );
    assert_eq!(
        store
            .load_runtime_binding_record(&recovered.binding_id)
            .unwrap()
            .state,
        RuntimeBindingState::Active
    );
}

#[test]
fn terminal_receipt_corruption_fails_load_and_recovery_without_mutation() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(&command(&actor_id, "issue-receipt", 'b', 30), &permit)
        .unwrap();
    let started = claim_and_start(&mut store, &actor_id, &permit, &lease, "receipt", 40);
    let typed_result = successful_result(&store, &permit.request_id);
    store
        .complete_execution(
            &command(&actor_id, "complete-receipt", 'd', 50),
            &ExecutionCompletion {
                execution_id: permit.execution_id.clone(),
                expected_revision: started.revision,
                succeeded: true,
                receipt_digest: digest('e'),
                safe_detail: None,
                typed_result: Some(typed_result),
            },
        )
        .unwrap();
    store
        .connection()
        .execute(
            "UPDATE execution_receipts SET state='failed' WHERE execution_id=?1",
            params![permit.execution_id.as_str()],
        )
        .unwrap();

    assert!(matches!(
        store.load_execution_record(&permit.execution_id),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(matches!(
        store.recover_gateway(60),
        Err(StoreError::Corrupt { .. })
    ));
    let state: String = store
        .connection()
        .query_row(
            "SELECT state FROM executions WHERE execution_id=?1",
            params![permit.execution_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "succeeded");
}

#[test]
fn brokered_start_audit_proof_substitution_is_corruption() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease) = approved_fixture(&mut store);
    let permit = permit(&approval);
    store
        .issue_permit(&command(&actor_id, "issue-audit-proof", '1', 30), &permit)
        .unwrap();
    claim_and_start(&mut store, &actor_id, &permit, &lease, "audit-proof", 40);
    store
        .connection()
        .execute(
            "UPDATE security_audit_proofs SET proof_digest=?2 WHERE execution_id=?1",
            params![permit.execution_id.as_str(), digest('2').as_str()],
        )
        .unwrap();

    assert!(matches!(
        store.load_execution_record(&permit.execution_id),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(matches!(
        store.recover_gateway(50),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn typed_brokered_operation_substitution_is_corruption() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (_actor_id, _task_id, _run_id, approval, _lease) = approved_fixture(&mut store);
    let substituted = BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
        checkpoint_id: CheckpointId::new(),
    });
    store
        .connection()
        .execute(
            "UPDATE brokered_requests SET operation_json=?2 WHERE request_id=?1",
            params![
                approval.request_id.as_str(),
                serde_json::to_string(&substituted).unwrap()
            ],
        )
        .unwrap();

    assert!(matches!(
        store.load_brokered_request(&approval.request_id),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn brokered_ack_dispatch_is_payload_bound_idempotent_and_non_replayable_after_start() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease, brokered) =
        pending_brokered_fixture(&mut store);
    let payload = digest('a');
    let prepare = command(&actor_id, "prepare-brokered-ack", '1', 14);

    let LedgerOutcome::Applied(prepared) = store
        .prepare_brokered_acknowledgement_dispatch(
            &prepare,
            &approval.approval_id,
            &brokered,
            &payload,
            &lease,
        )
        .unwrap()
    else {
        panic!("acknowledgement preparation must apply")
    };
    assert_eq!(prepared.state, BrokeredRuntimeDispatchState::Prepared);
    assert!(matches!(
        store
            .prepare_brokered_acknowledgement_dispatch(
                &prepare,
                &approval.approval_id,
                &brokered,
                &payload,
                &lease,
            )
            .unwrap(),
        LedgerOutcome::Replayed(_)
    ));
    let renewal = LeaseCommand {
        command: command(&actor_id, "renew-brokered-dispatch", 'f', 14),
        task_id: lease.task_id.clone(),
        run_id: lease.run_id.clone(),
        lease_owner: lease.lease_owner.clone(),
        expires_at_ms: 300,
    };
    let LedgerOutcome::Applied(renewed) = store
        .renew_run_lease(&renewal, lease.generation, lease.revision)
        .unwrap()
    else {
        panic!("lease renewal must apply")
    };
    let lease = LeaseClaim {
        task_id: renewed.task_id,
        run_id: renewed.run_id,
        lease_owner: renewed.lease_owner,
        generation: renewed.generation,
        revision: renewed.revision,
    };
    assert!(matches!(
        store.load_brokered_runtime_dispatch(
            &actor_id,
            BrokeredRuntimeDispatchKind::Acknowledgement,
            &brokered,
            &digest('b'),
            &lease,
            15,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));

    let start = command(&actor_id, "start-brokered-ack", '2', 15);
    let LedgerOutcome::Applied(started) = store
        .start_brokered_runtime_dispatch(
            &start,
            BrokeredRuntimeDispatchKind::Acknowledgement,
            &brokered,
            &payload,
            prepared.revision,
            &lease,
        )
        .unwrap()
    else {
        panic!("dispatch start must apply")
    };
    assert!(matches!(
        store
            .start_brokered_runtime_dispatch(
                &start,
                BrokeredRuntimeDispatchKind::Acknowledgement,
                &brokered,
                &payload,
                prepared.revision,
                &lease,
            )
            .unwrap(),
        LedgerOutcome::Replayed(_)
    ));
    assert!(matches!(
        store.start_brokered_runtime_dispatch(
            &command(&actor_id, "retry-brokered-ack", '3', 16),
            BrokeredRuntimeDispatchKind::Acknowledgement,
            &brokered,
            &payload,
            started.revision,
            &lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));

    let complete = command(&actor_id, "complete-brokered-ack", '4', 16);
    let LedgerOutcome::Applied(delivered) = store
        .complete_brokered_runtime_dispatch(
            &complete,
            BrokeredRuntimeDispatchKind::Acknowledgement,
            &brokered,
            &payload,
            started.revision,
            &lease,
        )
        .unwrap()
    else {
        panic!("dispatch completion must apply")
    };
    assert_eq!(delivered.state, BrokeredRuntimeDispatchState::Delivered);
    assert!(matches!(
        store
            .complete_brokered_runtime_dispatch(
                &complete,
                BrokeredRuntimeDispatchKind::Acknowledgement,
                &brokered,
                &payload,
                started.revision,
                &lease,
            )
            .unwrap(),
        LedgerOutcome::Replayed(_)
    ));
}

#[test]
fn brokered_started_dispatch_recovers_unknown_and_takeover_fences_prepared_dispatch() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, task_id, run_id, approval, lease, brokered) =
        pending_brokered_fixture(&mut store);
    let payload = digest('c');
    let LedgerOutcome::Applied(prepared) = store
        .prepare_brokered_acknowledgement_dispatch(
            &command(&actor_id, "prepare-lost-ack", '5', 14),
            &approval.approval_id,
            &brokered,
            &payload,
            &lease,
        )
        .unwrap()
    else {
        panic!("dispatch preparation must apply")
    };

    let takeover = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "dispatch-takeover",
        201,
        400,
    );
    assert!(takeover.generation > lease.generation);
    assert!(matches!(
        store.start_brokered_runtime_dispatch(
            &command(&actor_id, "stale-dispatch-start", '6', 202),
            BrokeredRuntimeDispatchKind::Acknowledgement,
            &brokered,
            &payload,
            prepared.revision,
            &takeover,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));

    let mut second = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease, brokered) =
        pending_brokered_fixture(&mut second);
    let LedgerOutcome::Applied(prepared) = second
        .prepare_brokered_acknowledgement_dispatch(
            &command(&actor_id, "prepare-recovery-ack", '7', 14),
            &approval.approval_id,
            &brokered,
            &payload,
            &lease,
        )
        .unwrap()
    else {
        panic!("dispatch preparation must apply")
    };
    second
        .start_brokered_runtime_dispatch(
            &command(&actor_id, "start-recovery-ack", '8', 15),
            BrokeredRuntimeDispatchKind::Acknowledgement,
            &brokered,
            &payload,
            prepared.revision,
            &lease,
        )
        .unwrap();
    let report = second.recover_gateway(30).unwrap();
    assert_eq!(report.brokered_dispatches_unknown, 1);
    let state: String = second
        .connection()
        .query_row(
            "SELECT state FROM brokered_runtime_dispatches
             WHERE request_id=?1 AND dispatch_kind='acknowledgement'",
            params![approval.request_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "unknown");
}

#[test]
fn live_brokered_dispatch_unknown_is_idempotent_and_never_restarts() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease, brokered) =
        pending_brokered_fixture(&mut store);
    let payload = digest('a');
    let LedgerOutcome::Applied(prepared) = store
        .prepare_brokered_acknowledgement_dispatch(
            &command(&actor_id, "prepare-live-unknown", 'b', 14),
            &approval.approval_id,
            &brokered,
            &payload,
            &lease,
        )
        .unwrap()
    else {
        panic!("dispatch preparation must apply")
    };
    let LedgerOutcome::Applied(started) = store
        .start_brokered_runtime_dispatch(
            &command(&actor_id, "start-live-unknown", 'c', 15),
            BrokeredRuntimeDispatchKind::Acknowledgement,
            &brokered,
            &payload,
            prepared.revision,
            &lease,
        )
        .unwrap()
    else {
        panic!("dispatch start must apply")
    };
    let unknown_command = command(&actor_id, "mark-live-unknown", 'd', 16);
    let LedgerOutcome::Applied(unknown) = store
        .mark_brokered_runtime_dispatch_unknown(
            &unknown_command,
            BrokeredRuntimeDispatchKind::Acknowledgement,
            &brokered,
            &payload,
            started.revision,
            &lease,
        )
        .unwrap()
    else {
        panic!("unknown transition must apply")
    };
    assert_eq!(unknown.state, BrokeredRuntimeDispatchState::Unknown);
    assert!(matches!(
        store
            .mark_brokered_runtime_dispatch_unknown(
                &unknown_command,
                BrokeredRuntimeDispatchKind::Acknowledgement,
                &brokered,
                &payload,
                started.revision,
                &lease,
            )
            .unwrap(),
        LedgerOutcome::Replayed(_)
    ));
    assert!(matches!(
        store.start_brokered_runtime_dispatch(
            &command(&actor_id, "restart-live-unknown", 'e', 17),
            BrokeredRuntimeDispatchKind::Acknowledgement,
            &brokered,
            &payload,
            unknown.revision,
            &lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
}

#[test]
fn brokered_denied_result_cannot_be_prepared_before_denial_is_durable() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease, brokered) =
        pending_brokered_fixture(&mut store);
    let delivery = denied_delivery(&approval.request_id);
    assert!(matches!(
        store.prepare_brokered_denied_result_dispatch(
            &command(&actor_id, "premature-denied-result", '1', 14),
            &approval.approval_id,
            &brokered,
            &delivery,
            &lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    store
        .resolve_approval(
            &command(&actor_id, "deny-brokered", '2', 20),
            &approval.approval_id,
            approval.revision,
            ApprovalResolution::Decide(ApprovalDecision::Deny),
        )
        .unwrap();
    let forged_denial = BrokeredExecutionDelivery {
        request_id: approval.request_id.clone(),
        outcome: BrokeredExecutionOutcome::Denied {
            code: DenialCode::PolicyDenied,
            safe_message: BoundedText::new("substituted policy result").unwrap(),
        },
    };
    assert!(matches!(
        store.prepare_brokered_denied_result_dispatch(
            &command(&actor_id, "prepare-forged-denial", '4', 21),
            &approval.approval_id,
            &brokered,
            &forged_denial,
            &lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    let LedgerOutcome::Applied(prepared) = store
        .prepare_brokered_denied_result_dispatch(
            &command(&actor_id, "prepare-denied-result", '3', 21),
            &approval.approval_id,
            &brokered,
            &delivery,
            &lease,
        )
        .unwrap()
    else {
        panic!("durable denial must enable result preparation")
    };
    assert_eq!(prepared.state, BrokeredRuntimeDispatchState::Prepared);
    assert_eq!(
        prepared.payload_digest,
        brokered_delivery_digest(&delivery).unwrap()
    );
}

#[test]
fn result_dispatch_rejects_outcome_substitution_against_durable_source() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease, brokered) =
        pending_brokered_fixture(&mut store);
    store
        .resolve_approval(
            &command(&actor_id, "substitution-approve", 'a', 20),
            &approval.approval_id,
            approval.revision,
            ApprovalResolution::Decide(ApprovalDecision::Approve),
        )
        .unwrap();
    let approved = store.load_approval_record(&approval.approval_id).unwrap();
    let permit = permit(&approved);
    store
        .issue_permit(&command(&actor_id, "substitution-issue", 'b', 30), &permit)
        .unwrap();
    let started = claim_and_start(&mut store, &actor_id, &permit, &lease, "substitution", 40);
    store
        .complete_execution(
            &command(&actor_id, "substitution-fail", 'c', 50),
            &ExecutionCompletion {
                execution_id: permit.execution_id.clone(),
                expected_revision: started.revision,
                succeeded: false,
                receipt_digest: digest('d'),
                safe_detail: Some(BoundedText::new("target failed").unwrap()),
                typed_result: None,
            },
        )
        .unwrap();
    let forged = BrokeredExecutionDelivery {
        request_id: permit.request_id.clone(),
        outcome: BrokeredExecutionOutcome::Succeeded {
            execution_id: permit.execution_id.clone(),
            result: successful_result(&store, &permit.request_id),
        },
    };
    assert!(matches!(
        store.prepare_brokered_execution_result_dispatch(
            &command(&actor_id, "substitution-forged", 'e', 51),
            &permit.execution_id,
            &brokered,
            &forged,
            &lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    let correct = BrokeredExecutionDelivery {
        request_id: permit.request_id.clone(),
        outcome: BrokeredExecutionOutcome::Failed {
            execution_id: permit.execution_id.clone(),
            error: ContractError::new(
                "checkpoint_execution_failed",
                ErrorCategory::Internal,
                false,
                "Checkpoint execution failed without an external effect retry",
            )
            .unwrap(),
        },
    };
    let LedgerOutcome::Applied(prepared) = store
        .prepare_brokered_execution_result_dispatch(
            &command(&actor_id, "substitution-correct", 'f', 51),
            &permit.execution_id,
            &brokered,
            &correct,
            &lease,
        )
        .unwrap()
    else {
        panic!("matching failed outcome must prepare")
    };
    assert_eq!(
        prepared.payload_digest,
        brokered_delivery_digest(&correct).unwrap()
    );
}

#[test]
fn brokered_dispatch_reference_substitution_is_detected_as_corruption() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease, brokered) =
        pending_brokered_fixture(&mut store);
    let payload = digest('4');
    store
        .prepare_brokered_acknowledgement_dispatch(
            &command(&actor_id, "prepare-corrupt-ack", '5', 14),
            &approval.approval_id,
            &brokered,
            &payload,
            &lease,
        )
        .unwrap();
    let mut substituted = brokered.clone();
    substituted.request_id = RequestId::new();
    store
        .connection()
        .execute(
            "UPDATE brokered_runtime_dispatches SET brokered_ref_json=?2
             WHERE request_id=?1 AND dispatch_kind='acknowledgement'",
            params![
                approval.request_id.as_str(),
                serde_json::to_string(&substituted).unwrap()
            ],
        )
        .unwrap();

    assert!(matches!(
        store.load_brokered_runtime_dispatch_record(
            &approval.request_id,
            BrokeredRuntimeDispatchKind::Acknowledgement,
        ),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(matches!(
        store.load_brokered_runtime_dispatch(
            &actor_id,
            BrokeredRuntimeDispatchKind::Acknowledgement,
            &brokered,
            &payload,
            &lease,
            15,
        ),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn durable_dispatch_read_survives_runtime_loss_and_reports_missing_kind() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, run_id, approval, lease, brokered) =
        pending_brokered_fixture(&mut store);
    assert!(matches!(
        store.load_brokered_runtime_dispatch_record(
            &approval.request_id,
            BrokeredRuntimeDispatchKind::Acknowledgement,
        ),
        Err(StoreError::LedgerNotFound { .. })
    ));
    let payload = digest('6');
    let LedgerOutcome::Applied(prepared) = store
        .prepare_brokered_acknowledgement_dispatch(
            &command(&actor_id, "prepare-durable-read", '7', 14),
            &approval.approval_id,
            &brokered,
            &payload,
            &lease,
        )
        .unwrap()
    else {
        panic!("dispatch preparation must apply")
    };
    store
        .mark_runtime_bindings_lost_for_run(&run_id, 15)
        .unwrap();

    assert_eq!(
        store
            .load_brokered_runtime_dispatch_record(
                &approval.request_id,
                BrokeredRuntimeDispatchKind::Acknowledgement,
            )
            .unwrap(),
        prepared
    );
    assert!(matches!(
        store.load_brokered_runtime_dispatch_record(
            &approval.request_id,
            BrokeredRuntimeDispatchKind::Result,
        ),
        Err(StoreError::LedgerNotFound { .. })
    ));
}

#[test]
fn durable_dispatch_read_keeps_acknowledgement_and_result_isolated() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease, brokered) =
        pending_brokered_fixture(&mut store);
    let LedgerOutcome::Applied(acknowledgement) = store
        .prepare_brokered_acknowledgement_dispatch(
            &command(&actor_id, "prepare-isolated-ack", '8', 14),
            &approval.approval_id,
            &brokered,
            &digest('8'),
            &lease,
        )
        .unwrap()
    else {
        panic!("acknowledgement preparation must apply")
    };
    store
        .resolve_approval(
            &command(&actor_id, "deny-isolated", '9', 20),
            &approval.approval_id,
            approval.revision,
            ApprovalResolution::Decide(ApprovalDecision::Deny),
        )
        .unwrap();
    let LedgerOutcome::Applied(result) = store
        .prepare_brokered_denied_result_dispatch(
            &command(&actor_id, "prepare-isolated-result", 'a', 21),
            &approval.approval_id,
            &brokered,
            &denied_delivery(&approval.request_id),
            &lease,
        )
        .unwrap()
    else {
        panic!("result preparation must apply")
    };

    assert_eq!(
        store
            .load_brokered_runtime_dispatch_record(
                &approval.request_id,
                BrokeredRuntimeDispatchKind::Acknowledgement,
            )
            .unwrap(),
        acknowledgement
    );
    assert_eq!(
        store
            .load_brokered_runtime_dispatch_record(
                &approval.request_id,
                BrokeredRuntimeDispatchKind::Result,
            )
            .unwrap(),
        result
    );
}

#[test]
fn live_unknown_execution_is_atomic_replay_safe_and_enables_result_preparation() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let (actor_id, _task_id, _run_id, approval, lease) = approved_fixture(&mut store);
    let brokered = brokered_reference(&store, &approval);
    let permit = permit(&approval);
    store
        .issue_permit(&command(&actor_id, "issue-unknown", '9', 30), &permit)
        .unwrap();
    let started = claim_and_start(&mut store, &actor_id, &permit, &lease, "unknown", 40);
    let uncertain_command = command(&actor_id, "target-unknown", 'a', 50);
    let detail = BoundedText::new("target response was indeterminate").unwrap();

    let LedgerOutcome::Applied(uncertain) = store
        .mark_execution_uncertain(
            &uncertain_command,
            &permit.execution_id,
            started.revision,
            &detail,
            &lease,
        )
        .unwrap()
    else {
        panic!("live uncertainty must apply")
    };
    assert_eq!(uncertain.state, ExecutionState::Uncertain);
    assert!(matches!(
        store
            .mark_execution_uncertain(
                &uncertain_command,
                &permit.execution_id,
                started.revision,
                &detail,
                &lease,
            )
            .unwrap(),
        LedgerOutcome::Replayed(_)
    ));

    let delivery = uncertain_delivery(&permit);
    let LedgerOutcome::Applied(result) = store
        .prepare_brokered_execution_result_dispatch(
            &command(&actor_id, "prepare-unknown-result", 'b', 51),
            &permit.execution_id,
            &brokered,
            &delivery,
            &lease,
        )
        .unwrap()
    else {
        panic!("uncertain result preparation must apply")
    };
    assert_eq!(result.state, BrokeredRuntimeDispatchState::Prepared);
    assert!(matches!(
        result.source,
        BrokeredRuntimeDispatchSource::Execution { execution_id }
            if execution_id == permit.execution_id
    ));
}

#[test]
fn task_and_ledger_commands_share_one_idempotency_namespace() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let approval = approval(&actor_id, &task_id, &run_id);
    let task_key = format!("task-{}", task_id.as_str());
    let result = store.create_approval(&command(&actor_id, &task_key, 'f', 10), &approval);
    assert!(matches!(result, Err(StoreError::IdempotencyConflict)));
    let count: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM approvals", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn runtime_input_response_is_private_digest_only_and_dispatch_is_once_only() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "input-lease",
        2,
        200,
    );
    let binding = runtime_binding(&task_id, &run_id, lease.generation);
    store
        .bind_runtime(&command(&actor_id, "input-bind", '2', 3), &binding, &lease)
        .unwrap();
    let request = runtime_input_request(&run_id);
    let record_command = command(&actor_id, "input-record", '3', 4);
    let LedgerOutcome::Applied(record) = store
        .record_runtime_input_request(
            &record_command,
            &request,
            100,
            &binding.binding_id,
            &binding.runtime_instance_id,
            binding.runtime_generation,
            1,
            &lease,
        )
        .unwrap()
    else {
        panic!("input request must apply")
    };
    assert_eq!(record.state, RuntimeInputRequestState::Pending);
    assert_eq!(
        store.load_task(&task_id).unwrap().state(),
        TaskState::WaitingInput
    );
    assert!(matches!(
        store
            .record_runtime_input_request(
                &record_command,
                &request,
                100,
                &binding.binding_id,
                &binding.runtime_instance_id,
                binding.runtime_generation,
                1,
                &lease,
            )
            .unwrap(),
        LedgerOutcome::Replayed(_)
    ));

    let secret = "response-secret-must-stay-private";
    let response = RuntimeInputResponse::Text {
        text: BoundedText::new(secret).unwrap(),
    };
    let resolve_command = command(&actor_id, "input-resolve", '4', 5);
    let LedgerOutcome::Applied(prepared) = store
        .resolve_runtime_input(&resolve_command, request.request_id(), 4, &response)
        .unwrap()
    else {
        panic!("input resolution must apply")
    };
    assert_eq!(prepared.state, RuntimeInputDispatchState::Prepared);
    assert_eq!(
        store.load_task(&task_id).unwrap().state(),
        TaskState::Running
    );

    let task_history: String = store
        .connection()
        .query_row(
            "SELECT group_concat(payload_json, '') FROM task_events WHERE task_id=?1",
            params![task_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let receipts: String = store
        .connection()
        .query_row(
            "SELECT group_concat(result_json, '') FROM ledger_receipts WHERE actor_id=?1",
            params![actor_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let private_response: String = store
        .connection()
        .query_row(
            "SELECT response_json FROM runtime_input_dispatches WHERE request_id=?1",
            params![request.request_id().as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!task_history.contains(secret));
    assert!(!receipts.contains(secret));
    assert!(private_response.contains(secret));

    let stale_lease = LeaseClaim {
        task_id: lease.task_id.clone(),
        run_id: lease.run_id.clone(),
        lease_owner: lease.lease_owner.clone(),
        generation: lease.generation + 1,
        revision: lease.revision,
    };
    assert!(matches!(
        store.start_runtime_input_dispatch(
            &command(&actor_id, "input-start-stale-fence", 'a', 6),
            request.request_id(),
            &prepared.response_digest,
            prepared.revision,
            &stale_lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    assert_eq!(
        store
            .load_runtime_input_dispatch(request.request_id())
            .unwrap()
            .state,
        RuntimeInputDispatchState::Prepared
    );

    let start_command = command(&actor_id, "input-start", '5', 6);
    let LedgerOutcome::Applied(started) = store
        .start_runtime_input_dispatch(
            &start_command,
            request.request_id(),
            &prepared.response_digest,
            prepared.revision,
            &lease,
        )
        .unwrap()
    else {
        panic!("dispatch start must apply exactly once")
    };
    assert_eq!(started.state, RuntimeInputDispatchState::Started);
    let complete_command = command(&actor_id, "input-complete", '6', 7);
    let LedgerOutcome::Applied(delivered) = store
        .complete_runtime_input_dispatch(
            &complete_command,
            request.request_id(),
            &prepared.response_digest,
            started.revision,
            &lease,
        )
        .unwrap()
    else {
        panic!("dispatch completion must apply")
    };
    assert_eq!(delivered.state, RuntimeInputDispatchState::Delivered);
    assert!(matches!(
        store
            .start_runtime_input_dispatch(
                &start_command,
                request.request_id(),
                &prepared.response_digest,
                prepared.revision,
                &lease,
            )
            .unwrap(),
        LedgerOutcome::Replayed(record)
            if record.state == RuntimeInputDispatchState::Delivered
    ));
    assert!(matches!(
        store.start_runtime_input_dispatch(
            &command(&actor_id, "input-start-again", '7', 8),
            request.request_id(),
            &prepared.response_digest,
            prepared.revision,
            &lease,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
}

#[test]
fn runtime_input_expiry_and_stale_fence_fail_without_dispatch() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "expired-input-lease",
        2,
        200,
    );
    let binding = runtime_binding(&task_id, &run_id, lease.generation);
    store
        .bind_runtime(
            &command(&actor_id, "expired-input-bind", '8', 3),
            &binding,
            &lease,
        )
        .unwrap();
    let request = runtime_input_request(&run_id);
    store
        .record_runtime_input_request(
            &command(&actor_id, "expired-input-record", '9', 4),
            &request,
            5,
            &binding.binding_id,
            &binding.runtime_instance_id,
            binding.runtime_generation,
            1,
            &lease,
        )
        .unwrap();
    let response = RuntimeInputResponse::Text {
        text: BoundedText::new("too late").unwrap(),
    };
    assert!(matches!(
        store.resolve_runtime_input(
            &command(&actor_id, "expired-input-resolve", 'a', 5),
            request.request_id(),
            4,
            &response,
        ),
        Err(StoreError::LedgerConflict { .. })
    ));
    assert_eq!(store.load_task(&task_id).unwrap().revision(), 4);
    assert!(matches!(
        store.load_runtime_input_dispatch(request.request_id()),
        Err(StoreError::LedgerNotFound { .. })
    ));
    let LedgerOutcome::Applied(expired) = store
        .expire_runtime_input_request(
            &command(&actor_id, "expired-input-converge", 'b', 5),
            request.request_id(),
            1,
        )
        .unwrap()
    else {
        panic!("expired input request must converge")
    };
    assert_eq!(expired.state, RuntimeInputRequestState::Expired);
    assert_eq!(
        store.load_task(&task_id).unwrap().state(),
        TaskState::Suspended
    );
}

#[test]
fn task_cancel_or_suspend_wins_before_runtime_input_dispatch_start() {
    for winner in ["cancel", "suspend"] {
        let mut store = SqliteTaskStore::open_in_memory().unwrap();
        let actor_id = ActorId::new();
        let run_id = RunId::new();
        let task_id = create_task(&mut store, &actor_id, &run_id);
        let lease = acquire_lease(
            &mut store,
            &actor_id,
            &task_id,
            &run_id,
            &format!("{winner}-input-lease"),
            2,
            200,
        );
        let binding = runtime_binding(&task_id, &run_id, lease.generation);
        store
            .bind_runtime(
                &command(&actor_id, &format!("{winner}-input-bind"), '2', 3),
                &binding,
                &lease,
            )
            .unwrap();
        let request = runtime_input_request(&run_id);
        store
            .record_runtime_input_request(
                &command(&actor_id, &format!("{winner}-input-record"), '3', 4),
                &request,
                100,
                &binding.binding_id,
                &binding.runtime_instance_id,
                binding.runtime_generation,
                1,
                &lease,
            )
            .unwrap();
        let response = RuntimeInputResponse::Text {
            text: BoundedText::new("do not deliver after Task transition").unwrap(),
        };
        let LedgerOutcome::Applied(prepared) = store
            .resolve_runtime_input(
                &command(&actor_id, &format!("{winner}-input-resolve"), '4', 5),
                request.request_id(),
                4,
                &response,
            )
            .unwrap()
        else {
            panic!("input resolution must apply")
        };
        let event = if winner == "cancel" {
            TaskEvent::CancellationRequested {
                run_id: run_id.clone(),
                cause: cosh_gateway_contracts::task::CancelReason::UserRequested,
            }
        } else {
            TaskEvent::RunSuspended {
                run_id: run_id.clone(),
                reason: cosh_gateway_contracts::task::SuspensionCode::OperatorRequired,
            }
        };
        append_task_event(
            &mut store,
            &actor_id,
            &task_id,
            6,
            &format!("{winner}-wins-before-input-start"),
            6,
            event,
        );

        let start_key = format!("{winner}-input-start-after-task-transition");
        assert!(matches!(
            store.start_runtime_input_dispatch(
                &command(&actor_id, &start_key, '5', 7),
                request.request_id(),
                &prepared.response_digest,
                prepared.revision,
                &lease,
            ),
            Err(StoreError::LedgerConflict { .. })
        ));
        let unchanged = store
            .load_runtime_input_dispatch(request.request_id())
            .unwrap();
        assert_eq!(unchanged.state, RuntimeInputDispatchState::Prepared);
        assert_eq!(unchanged.revision, prepared.revision);
        let receipt_count: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM ledger_receipts
                 WHERE actor_id=?1 AND idempotency_key=?2",
                params![actor_id.as_str(), start_key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(receipt_count, 0);
    }
}

#[test]
fn started_runtime_input_recovery_is_atomic_unknown_and_task_suspended() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway/state.db");
    let (actor_id, task_id, run_id, request_id, response_digest) = {
        let mut store = SqliteTaskStore::open(&path).unwrap();
        let actor_id = ActorId::new();
        let run_id = RunId::new();
        let task_id = create_task(&mut store, &actor_id, &run_id);
        let lease = acquire_lease(
            &mut store,
            &actor_id,
            &task_id,
            &run_id,
            "recover-input-lease",
            2,
            6,
        );
        let binding = runtime_binding(&task_id, &run_id, lease.generation);
        store
            .bind_runtime(
                &command(&actor_id, "recover-input-bind", 'b', 3),
                &binding,
                &lease,
            )
            .unwrap();
        let request = runtime_input_request(&run_id);
        store
            .record_runtime_input_request(
                &command(&actor_id, "recover-input-record", 'c', 4),
                &request,
                100,
                &binding.binding_id,
                &binding.runtime_instance_id,
                binding.runtime_generation,
                1,
                &lease,
            )
            .unwrap();
        let response = RuntimeInputResponse::Text {
            text: BoundedText::new("recover privately").unwrap(),
        };
        let LedgerOutcome::Applied(prepared) = store
            .resolve_runtime_input(
                &command(&actor_id, "recover-input-resolve", 'd', 5),
                request.request_id(),
                4,
                &response,
            )
            .unwrap()
        else {
            panic!("resolution must apply")
        };
        store
            .start_runtime_input_dispatch(
                &command(&actor_id, "recover-input-start", 'e', 5),
                request.request_id(),
                &prepared.response_digest,
                prepared.revision,
                &lease,
            )
            .unwrap();
        let takeover = acquire_lease(
            &mut store,
            &actor_id,
            &task_id,
            &run_id,
            "recover-input-takeover",
            7,
            200,
        );
        let LedgerOutcome::Applied(1) = store
            .recover_runtime_input_dispatch_for_run(
                &command(&actor_id, "recover-input", 'f', 8),
                &run_id,
                &takeover,
            )
            .unwrap()
        else {
            panic!("one started dispatch must recover")
        };
        (
            actor_id,
            task_id,
            run_id,
            request.request_id().clone(),
            prepared.response_digest,
        )
    };
    let mut reopened = SqliteTaskStore::open(&path).unwrap();
    assert_eq!(
        reopened.load_task(&task_id).unwrap().state(),
        TaskState::Suspended
    );
    let dispatch = reopened.load_runtime_input_dispatch(&request_id).unwrap();
    assert_eq!(dispatch.state, RuntimeInputDispatchState::Unknown);
    assert_eq!(dispatch.response_digest, response_digest);
    let lease = reopened.load_run_lease(&run_id).unwrap();
    let takeover = LeaseClaim {
        task_id: lease.task_id,
        run_id: lease.run_id,
        lease_owner: lease.lease_owner,
        generation: lease.generation,
        revision: lease.revision,
    };
    assert!(matches!(
        reopened.recover_runtime_input_dispatch_for_run(
            &command(&actor_id, "recover-input-again", '1', 9),
            &run_id,
            &takeover,
        ),
        Ok(LedgerOutcome::Applied(0))
    ));
}

#[test]
fn prepared_runtime_input_restart_is_atomic_unknown_and_task_suspended() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway/prepared-input.db");
    let (task_id, request_id) = {
        let mut store = SqliteTaskStore::open(&path).unwrap();
        let actor_id = ActorId::new();
        let run_id = RunId::new();
        let task_id = create_task(&mut store, &actor_id, &run_id);
        let lease = acquire_lease(
            &mut store,
            &actor_id,
            &task_id,
            &run_id,
            "prepared-restart-lease",
            2,
            200,
        );
        let binding = runtime_binding(&task_id, &run_id, lease.generation);
        store
            .bind_runtime(
                &command(&actor_id, "prepared-restart-bind", 'b', 3),
                &binding,
                &lease,
            )
            .unwrap();
        let request = runtime_input_request(&run_id);
        store
            .record_runtime_input_request(
                &command(&actor_id, "prepared-restart-record", 'c', 4),
                &request,
                100,
                &binding.binding_id,
                &binding.runtime_instance_id,
                binding.runtime_generation,
                1,
                &lease,
            )
            .unwrap();
        store
            .resolve_runtime_input(
                &command(&actor_id, "prepared-restart-resolve", 'd', 5),
                request.request_id(),
                4,
                &RuntimeInputResponse::Text {
                    text: BoundedText::new("prepared response lost with process").unwrap(),
                },
            )
            .unwrap();
        (task_id, request.request_id().clone())
    };

    let mut reopened = SqliteTaskStore::open(&path).unwrap();
    let report = reopened.recover_gateway(6).unwrap();
    assert_eq!(report.runtime_input_dispatches_unknown, 1);
    assert_eq!(report.runtime_input_requests_cancelled, 0);
    assert_eq!(
        reopened
            .load_runtime_input_dispatch(&request_id)
            .unwrap()
            .state,
        RuntimeInputDispatchState::Unknown
    );
    assert_eq!(
        reopened.load_task(&task_id).unwrap().state(),
        TaskState::Suspended
    );
}

#[test]
fn live_runtime_input_unknown_is_atomic_and_replay_safe() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "live-input-unknown-lease",
        2,
        200,
    );
    let binding = runtime_binding(&task_id, &run_id, lease.generation);
    store
        .bind_runtime(
            &command(&actor_id, "live-input-unknown-bind", 'e', 3),
            &binding,
            &lease,
        )
        .unwrap();
    let request = runtime_input_request(&run_id);
    store
        .record_runtime_input_request(
            &command(&actor_id, "live-input-unknown-record", 'f', 4),
            &request,
            100,
            &binding.binding_id,
            &binding.runtime_instance_id,
            binding.runtime_generation,
            1,
            &lease,
        )
        .unwrap();
    let LedgerOutcome::Applied(prepared) = store
        .resolve_runtime_input(
            &command(&actor_id, "live-input-unknown-resolve", '1', 5),
            request.request_id(),
            4,
            &RuntimeInputResponse::Text {
                text: BoundedText::new("transport outcome unknown").unwrap(),
            },
        )
        .unwrap()
    else {
        panic!("input resolution must apply")
    };
    let LedgerOutcome::Applied(started) = store
        .start_runtime_input_dispatch(
            &command(&actor_id, "live-input-unknown-start", '2', 6),
            request.request_id(),
            &prepared.response_digest,
            prepared.revision,
            &lease,
        )
        .unwrap()
    else {
        panic!("input dispatch start must apply")
    };
    let unknown_command = command(&actor_id, "live-input-unknown", '3', 7);
    let LedgerOutcome::Applied(unknown) = store
        .mark_runtime_input_dispatch_unknown(
            &unknown_command,
            request.request_id(),
            &started.response_digest,
            started.revision,
            &lease,
        )
        .unwrap()
    else {
        panic!("input uncertainty must apply")
    };
    assert_eq!(unknown.state, RuntimeInputDispatchState::Unknown);
    assert_eq!(
        store.load_task(&task_id).unwrap().state(),
        TaskState::Suspended
    );
    assert!(matches!(
        store
            .mark_runtime_input_dispatch_unknown(
                &unknown_command,
                request.request_id(),
                &started.response_digest,
                started.revision,
                &lease,
            )
            .unwrap(),
        LedgerOutcome::Replayed(record)
            if record.state == RuntimeInputDispatchState::Unknown
    ));
}

#[test]
fn run_takeover_cancels_pending_runtime_input_and_suspends_task() {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "pending-input-lease",
        2,
        6,
    );
    let binding = runtime_binding(&task_id, &run_id, lease.generation);
    store
        .bind_runtime(
            &command(&actor_id, "pending-input-bind", '2', 3),
            &binding,
            &lease,
        )
        .unwrap();
    let request = runtime_input_request(&run_id);
    store
        .record_runtime_input_request(
            &command(&actor_id, "pending-input-record", '3', 4),
            &request,
            100,
            &binding.binding_id,
            &binding.runtime_instance_id,
            binding.runtime_generation,
            1,
            &lease,
        )
        .unwrap();
    let takeover = acquire_lease(
        &mut store,
        &actor_id,
        &task_id,
        &run_id,
        "pending-input-takeover",
        7,
        200,
    );
    let recovery_command = command(&actor_id, "recover-pending-input", '4', 8);
    assert!(matches!(
        store
            .recover_runtime_input_dispatch_for_run(&recovery_command, &run_id, &takeover)
            .unwrap(),
        LedgerOutcome::Applied(1)
    ));
    assert_eq!(
        store
            .load_runtime_input_request(request.request_id())
            .unwrap()
            .state,
        RuntimeInputRequestState::Cancelled
    );
    assert_eq!(
        store.load_task(&task_id).unwrap().state(),
        TaskState::Suspended
    );
    assert!(matches!(
        store
            .recover_runtime_input_dispatch_for_run(&recovery_command, &run_id, &takeover)
            .unwrap(),
        LedgerOutcome::Replayed(1)
    ));
}

#[test]
fn failed_run_input_is_recovered_before_atomic_retry_admission() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway/state.db");
    let (actor_id, task_id, previous_run_id, request_id) = {
        let mut store = SqliteTaskStore::open(&path).unwrap();
        let actor_id = ActorId::new();
        let run_id = RunId::new();
        let task_id = create_task(&mut store, &actor_id, &run_id);
        let lease = acquire_lease(
            &mut store,
            &actor_id,
            &task_id,
            &run_id,
            "failed-input-lease",
            2,
            6,
        );
        let binding = runtime_binding(&task_id, &run_id, lease.generation);
        store
            .bind_runtime(
                &command(&actor_id, "failed-input-bind", '5', 3),
                &binding,
                &lease,
            )
            .unwrap();
        let request = runtime_input_request(&run_id);
        store
            .record_runtime_input_request(
                &command(&actor_id, "failed-input-record", '6', 4),
                &request,
                100,
                &binding.binding_id,
                &binding.runtime_instance_id,
                binding.runtime_generation,
                1,
                &lease,
            )
            .unwrap();
        append_task_event(
            &mut store,
            &actor_id,
            &task_id,
            5,
            "failed-before-input-cleanup",
            5,
            TaskEvent::RunFailed {
                run_id: run_id.clone(),
                error: ContractError::new(
                    "runtime_failed",
                    ErrorCategory::RuntimeUnavailable,
                    true,
                    "Runtime failed while input was pending",
                )
                .unwrap(),
            },
        );
        assert_eq!(
            store
                .mark_runtime_bindings_lost_for_run(&run_id, 6)
                .unwrap(),
            1
        );
        (actor_id, task_id, run_id, request.request_id().clone())
    };

    let mut store = SqliteTaskStore::open(&path).unwrap();
    let next_run_id = RunId::new();
    let mut correlation = Correlation::new(InstallationId::new());
    correlation.actor_id = Some(actor_id.clone());
    correlation.task_id = Some(task_id.clone());
    let retry_event = TaskEventEnvelope {
        header: ContractHeader::new(ContractSchema::TaskEvent, MessageId::new(), 1, correlation),
        task_id: task_id.clone(),
        revision: 6,
        event: TaskEvent::RunRetryQueued {
            previous_run_id: previous_run_id.clone(),
            next_run_id: next_run_id.clone(),
        },
    };
    let retry_commit = TaskCommit {
        actor_id: actor_id.clone(),
        idempotency_key: IdempotencyKey::new("retry-after-input-cleanup").unwrap(),
        command_digest: digest('7'),
        expected_revision: Some(5),
        outbox: vec![OutboxIntent {
            delivery_id: DeliveryId::new(),
            event_id: retry_event.header.message_id.clone(),
            delivery_kind: BoundedName::new("runtime_start").unwrap(),
            payload: serde_json::json!({
                "schema_version": 1,
                "actor": { "actor_id": actor_id.as_str() },
                "task_id": task_id.as_str(),
                "run_id": next_run_id.as_str(),
            }),
            next_attempt_at_ms: 10,
        }],
        events: vec![retry_event],
        committed_at_ms: 10,
    };
    assert!(matches!(
        store.commit_retry_task(&retry_commit, &previous_run_id),
        Err(StoreError::LedgerConflict { message })
            if message.contains("unsettled Runtime input")
    ));
    assert_eq!(store.load_task(&task_id).unwrap().revision(), 5);

    let report = store.recover_gateway(8).unwrap();
    assert_eq!(report.runtime_input_requests_cancelled, 1);
    assert_eq!(report.runtime_input_dispatches_unknown, 0);
    assert_eq!(store.load_task(&task_id).unwrap().revision(), 5);
    assert_eq!(
        store.load_runtime_input_request(&request_id).unwrap().state,
        RuntimeInputRequestState::Cancelled
    );
    assert!(matches!(
        store
            .commit_retry_task(&retry_commit, &previous_run_id)
            .unwrap(),
        CommitOutcome::Applied(_)
    ));
    assert_eq!(
        store.load_task(&task_id).unwrap().state(),
        TaskState::Queued
    );
    assert_eq!(
        store.load_task(&task_id).unwrap().active_run_id(),
        Some(&next_run_id)
    );
}

#[test]
fn terminal_task_before_input_cleanup_recovers_without_duplicate_task_event() {
    for terminal in ["failed", "cancelled"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("gateway/{terminal}.db"));
        let (task_id, request_id, expected_revision, expected_state) = {
            let mut store = SqliteTaskStore::open(&path).unwrap();
            let actor_id = ActorId::new();
            let run_id = RunId::new();
            let task_id = create_task(&mut store, &actor_id, &run_id);
            let lease = acquire_lease(
                &mut store,
                &actor_id,
                &task_id,
                &run_id,
                &format!("{terminal}-cleanup-lease"),
                2,
                100,
            );
            let binding = runtime_binding(&task_id, &run_id, lease.generation);
            store
                .bind_runtime(
                    &command(&actor_id, &format!("{terminal}-cleanup-bind"), '8', 3),
                    &binding,
                    &lease,
                )
                .unwrap();
            let request = runtime_input_request(&run_id);
            store
                .record_runtime_input_request(
                    &command(&actor_id, &format!("{terminal}-cleanup-record"), '9', 4),
                    &request,
                    90,
                    &binding.binding_id,
                    &binding.runtime_instance_id,
                    binding.runtime_generation,
                    1,
                    &lease,
                )
                .unwrap();
            if terminal == "failed" {
                append_task_event(
                    &mut store,
                    &actor_id,
                    &task_id,
                    5,
                    "terminal-input-run-failed",
                    5,
                    TaskEvent::RunFailed {
                        run_id: run_id.clone(),
                        error: ContractError::new(
                            "runtime_failed",
                            ErrorCategory::RuntimeUnavailable,
                            false,
                            "Runtime failed while input was pending",
                        )
                        .unwrap(),
                    },
                );
                append_task_event(
                    &mut store,
                    &actor_id,
                    &task_id,
                    6,
                    "terminal-input-task-failed",
                    6,
                    TaskEvent::TaskFailed {
                        error: ContractError::new(
                            "task_failed",
                            ErrorCategory::RuntimeUnavailable,
                            false,
                            "Task failed after Runtime failure",
                        )
                        .unwrap(),
                    },
                );
                (task_id, request.request_id().clone(), 6, TaskState::Failed)
            } else {
                append_task_event(
                    &mut store,
                    &actor_id,
                    &task_id,
                    5,
                    "terminal-input-cancel-requested",
                    5,
                    TaskEvent::CancellationRequested {
                        run_id: run_id.clone(),
                        cause: cosh_gateway_contracts::task::CancelReason::UserRequested,
                    },
                );
                append_task_event(
                    &mut store,
                    &actor_id,
                    &task_id,
                    6,
                    "terminal-input-run-cancelled",
                    6,
                    TaskEvent::RunCancelled {
                        run_id,
                        stage: cosh_gateway_contracts::task::CancellationStage::Runtime,
                    },
                );
                append_task_event(
                    &mut store,
                    &actor_id,
                    &task_id,
                    7,
                    "terminal-input-task-cancelled",
                    7,
                    TaskEvent::TaskCancelled,
                );
                (
                    task_id,
                    request.request_id().clone(),
                    7,
                    TaskState::Cancelled,
                )
            }
        };

        let mut reopened = SqliteTaskStore::open(&path).unwrap();
        let report = reopened.recover_gateway(50).unwrap();
        assert_eq!(report.runtime_input_requests_cancelled, 1, "{terminal}");
        let task = reopened.load_task(&task_id).unwrap();
        assert_eq!(task.state(), expected_state, "{terminal}");
        assert_eq!(task.revision(), expected_revision, "{terminal}");
        assert_eq!(
            reopened
                .load_runtime_input_request(&request_id)
                .unwrap()
                .state,
            RuntimeInputRequestState::Cancelled,
            "{terminal}"
        );
    }
}
