use cosh_gateway_contracts::{
    capability::{
        BrokeredOperation, CapabilityRequest, CapabilityScope, ExecutionPermit,
        OperationDescriptor, RuntimeExecutionFence, WorkspaceCheckpointCreateV1,
    },
    common::{
        ActorKind, ActorRef, AuthAssurance, BoundedName, BoundedOpaque, BoundedText,
        ContractHeader, ContractSchema, Correlation, Digest, IdempotencyKey, RuntimeBindingRef,
        RuntimeSelector, TargetRef,
    },
    external::{ExternalRef, ExternalRefKind},
    ids::{
        ActorId, AgentSessionId, CheckpointId, ExecutionId, InstallationId, MessageId, PermitId,
        RequestId, RunId, RuntimeBindingId, RuntimeInstanceId, TaskId,
    },
    runtime::{
        BrokeredOperationResult, WorkspaceCheckpointCreateV1Outcome,
        WorkspaceCheckpointCreateV1Result,
    },
    task::{TaskEvent, TaskEventEnvelope},
};

use super::*;
use crate::storage::{
    CommitOutcome, ExecutionClaim, ExecutionState, LeaseClaim, LeaseCommand, LedgerCommand,
    LedgerOutcome, PermitState, TaskCommit,
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
    let events = vec![
        event(
            1,
            TaskEvent::TaskSubmitted {
                intent_digest: digest('0'),
                target: target(),
            },
        ),
        event(
            2,
            TaskEvent::TaskQueued {
                run_id: run_id.clone(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("acp").unwrap(),
                    profile: Some(BoundedName::new("test").unwrap()),
                },
            },
        ),
        event(
            3,
            TaskEvent::RunStarted {
                run_id: run_id.clone(),
            },
        ),
    ];
    let result = store
        .commit_task(&TaskCommit {
            actor_id: actor_id.clone(),
            idempotency_key: IdempotencyKey::new("create-task").unwrap(),
            command_digest: digest('1'),
            expected_revision: Some(0),
            events,
            outbox: Vec::new(),
            committed_at_ms: 1,
        })
        .unwrap();
    assert!(matches!(result, CommitOutcome::Applied(_)));
    task_id
}

fn acquire_lease(
    store: &mut SqliteTaskStore,
    actor_id: &ActorId,
    task_id: &TaskId,
    run_id: &RunId,
) -> LeaseClaim {
    let outcome = store
        .acquire_run_lease(&LeaseCommand {
            command: command(actor_id, "lease", '2', 10),
            task_id: task_id.clone(),
            run_id: run_id.clone(),
            lease_owner: BoundedOpaque::new("coordinator").unwrap(),
            expires_at_ms: 100,
        })
        .unwrap();
    let LedgerOutcome::Applied(lease) = outcome else {
        panic!("lease must be newly acquired")
    };
    LeaseClaim {
        task_id: lease.task_id,
        run_id: lease.run_id,
        lease_owner: lease.lease_owner,
        generation: lease.generation,
        revision: lease.revision,
    }
}

fn permit(
    actor_id: &ActorId,
    task_id: &TaskId,
    run_id: &RunId,
    runtime_fence: RuntimeExecutionFence,
) -> ExecutionPermit {
    ExecutionPermit {
        permit_id: PermitId::new(),
        request_id: RequestId::new(),
        actor_id: actor_id.clone(),
        approval_id: None,
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        execution_id: ExecutionId::new(),
        target: target(),
        target_identity_digest: digest('a'),
        runtime_fence,
        operation_digest: digest('3'),
        input_digest: digest('4'),
        policy_revision: 1,
        valid_until_ms: 90,
        single_use: true,
    }
}

fn claim(permit: &ExecutionPermit, lease: LeaseClaim) -> ExecutionClaim {
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
        lease,
    }
}

#[derive(Clone)]
struct Operation {
    target: TargetRef,
    target_identity_digest: Digest,
    runtime_fence: RuntimeExecutionFence,
    operation_digest: Digest,
    input_digest: Digest,
    checkpoint_id: CheckpointId,
}

impl BoundExecutionOperation for Operation {
    fn target(&self) -> &TargetRef {
        &self.target
    }

    fn target_identity_digest(&self) -> &Digest {
        &self.target_identity_digest
    }

    fn runtime_fence(&self) -> &RuntimeExecutionFence {
        &self.runtime_fence
    }

    fn operation_digest(&self) -> &Digest {
        &self.operation_digest
    }

    fn input_digest(&self) -> &Digest {
        &self.input_digest
    }
}

struct Audit;

impl SecurityAuditGate<Operation> for Audit {
    fn persist_start(
        &mut self,
        _execution: &crate::storage::ExecutionRecord,
        _operation: &Operation,
    ) -> Result<crate::storage::SecurityAuditProof, SecurityAuditError> {
        Ok(crate::storage::SecurityAuditProof {
            proof_digest: digest('b'),
            persisted_at_ms: 25,
        })
    }
}

struct FailingAudit;

impl SecurityAuditGate<Operation> for FailingAudit {
    fn persist_start(
        &mut self,
        _execution: &crate::storage::ExecutionRecord,
        _operation: &Operation,
    ) -> Result<crate::storage::SecurityAuditProof, SecurityAuditError> {
        Err(SecurityAuditError)
    }
}

struct AdvancingAudit;

impl SecurityAuditGate<Operation> for AdvancingAudit {
    fn persist_start(
        &mut self,
        _execution: &crate::storage::ExecutionRecord,
        _operation: &Operation,
    ) -> Result<crate::storage::SecurityAuditProof, SecurityAuditError> {
        Ok(crate::storage::SecurityAuditProof {
            proof_digest: digest('f'),
            persisted_at_ms: 35,
        })
    }
}

struct Target {
    calls: usize,
    outcome: ExecutionTargetOutcome,
}

impl ExecutionTarget<Operation> for Target {
    fn execute(&mut self, _operation: &Operation) -> ExecutionTargetOutcome {
        self.calls += 1;
        self.outcome.clone()
    }
}

struct Fixture {
    store: SqliteTaskStore,
    actor_id: ActorId,
    permit: ExecutionPermit,
    claim: ExecutionClaim,
    operation: Operation,
}

fn fixture() -> Fixture {
    let mut store = SqliteTaskStore::open_in_memory().unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_started_task(&mut store, &actor_id, &run_id);
    let lease = acquire_lease(&mut store, &actor_id, &task_id, &run_id);
    let binding = RuntimeBindingRef {
        binding_id: RuntimeBindingId::new(),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        agent_session_id: AgentSessionId::new(),
        runtime_instance_id: RuntimeInstanceId::new(),
        runtime_generation: lease.generation,
        external_session: ExternalRef {
            kind: ExternalRefKind::AcpSession,
            authority: BoundedName::new("test").unwrap(),
            scope_digest: digest('d'),
            value: BoundedOpaque::new("execution-test").unwrap(),
        },
    };
    store
        .bind_runtime(&command(&actor_id, "bind", 'c', 11), &binding, &lease)
        .unwrap();
    let permit = permit(
        &actor_id,
        &task_id,
        &run_id,
        RuntimeExecutionFence {
            binding_id: binding.binding_id,
            runtime_generation: binding.runtime_generation,
            lease_generation: lease.generation,
            lease_revision: lease.revision,
        },
    );
    let request = CapabilityRequest {
        request_id: permit.request_id.clone(),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        actor: ActorRef {
            actor_id: actor_id.clone(),
            actor_kind: ActorKind::Human,
            issuer: BoundedName::new("local-os").unwrap(),
            assurance: AuthAssurance::LocalOs,
        },
        target: permit.target.clone(),
        operation: OperationDescriptor {
            namespace: BoundedName::new("workspace").unwrap(),
            name: BoundedName::new("checkpoint_create").unwrap(),
            arguments_digest: permit.operation_digest.clone(),
        },
        operation_digest: permit.operation_digest.clone(),
        requested_scope: CapabilityScope {
            resource: BoundedName::new("workspace").unwrap(),
            access: BoundedName::new("checkpoint").unwrap(),
        },
        input_digest: permit.input_digest.clone(),
        expires_at_ms: permit.valid_until_ms,
    };
    let checkpoint_id = CheckpointId::new();
    store
        .create_brokered_request(
            &command(&actor_id, "brokered-request", 'e', 12),
            &request,
            &BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
                checkpoint_id: checkpoint_id.clone(),
            }),
            &permit.target_identity_digest,
            &permit.runtime_fence,
            None,
        )
        .unwrap();
    store
        .issue_permit(&command(&actor_id, "issue", '5', 20), &permit)
        .unwrap();
    let claim = claim(&permit, lease);
    let operation = Operation {
        target: permit.target.clone(),
        target_identity_digest: permit.target_identity_digest.clone(),
        runtime_fence: permit.runtime_fence.clone(),
        operation_digest: permit.operation_digest.clone(),
        input_digest: permit.input_digest.clone(),
        checkpoint_id,
    };
    Fixture {
        store,
        actor_id,
        permit,
        claim,
        operation,
    }
}

fn typed_result(operation: &Operation) -> BrokeredOperationResult {
    BrokeredOperationResult::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1Result {
        checkpoint_id: operation.checkpoint_id.clone(),
        outcome: WorkspaceCheckpointCreateV1Outcome::Skipped {
            reason: BoundedText::new("test result").unwrap(),
        },
    })
}

#[test]
fn conclusive_result_consumes_permit_and_records_receipt() {
    let mut fixture = fixture();
    let mut target = Target {
        calls: 0,
        outcome: ExecutionTargetOutcome::Conclusive {
            succeeded: true,
            receipt_digest: digest('6'),
            safe_detail: Some(BoundedText::new("completed").unwrap()),
            typed_result: Some(typed_result(&fixture.operation)),
        },
    };
    let mut audit = Audit;
    let result = GovernedExecutionCoordinator::new(&mut fixture.store)
        .execute(
            &command(&fixture.actor_id, "claim", '7', 30),
            |_| Ok(command(&fixture.actor_id, "start", '9', 31)),
            || Ok(command(&fixture.actor_id, "complete", '8', 40)),
            &fixture.claim,
            &fixture.operation,
            &mut target,
            &mut audit,
        )
        .unwrap();

    assert!(result.succeeded);
    assert_eq!(result.typed_result, Some(typed_result(&fixture.operation)));
    assert_eq!(result.execution.state, ExecutionState::Succeeded);
    assert_eq!(target.calls, 1);
    assert_eq!(
        fixture
            .store
            .load_permit_record(&fixture.permit.permit_id)
            .unwrap()
            .state,
        PermitState::Consumed
    );
}

#[test]
fn successful_target_without_typed_result_cannot_commit_a_receipt() {
    let mut fixture = fixture();
    let mut target = Target {
        calls: 0,
        outcome: ExecutionTargetOutcome::Conclusive {
            succeeded: true,
            receipt_digest: digest('1'),
            safe_detail: None,
            typed_result: None,
        },
    };
    let mut audit = Audit;
    let result = GovernedExecutionCoordinator::new(&mut fixture.store).execute(
        &command(&fixture.actor_id, "missing-result-claim", '2', 30),
        |_| Ok(command(&fixture.actor_id, "missing-result-start", '3', 31)),
        || {
            Ok(command(
                &fixture.actor_id,
                "missing-result-complete",
                '4',
                40,
            ))
        },
        &fixture.claim,
        &fixture.operation,
        &mut target,
        &mut audit,
    );

    assert!(matches!(
        result,
        Err(GovernedExecutionError::CompletionUnknown { .. })
    ));
    assert_eq!(target.calls, 1);
    assert_eq!(
        fixture
            .store
            .load_execution_record(&fixture.permit.execution_id)
            .unwrap()
            .state,
        ExecutionState::Started
    );
}

#[test]
fn start_command_is_built_after_an_advancing_audit_clock() {
    let mut fixture = fixture();
    let mut target = Target {
        calls: 0,
        outcome: ExecutionTargetOutcome::Conclusive {
            succeeded: true,
            receipt_digest: digest('6'),
            safe_detail: None,
            typed_result: Some(typed_result(&fixture.operation)),
        },
    };
    let mut audit = AdvancingAudit;
    let actor_id = fixture.actor_id.clone();
    let result = GovernedExecutionCoordinator::new(&mut fixture.store).execute(
        &command(&actor_id, "advancing-claim", '7', 30),
        |proof| {
            assert_eq!(proof.persisted_at_ms, 35);
            Ok(command(&actor_id, "advancing-start", '8', 36))
        },
        || Ok(command(&actor_id, "advancing-complete", '9', 40)),
        &fixture.claim,
        &fixture.operation,
        &mut target,
        &mut audit,
    );

    assert!(result.is_ok());
    assert_eq!(target.calls, 1);
}

#[test]
fn post_audit_start_command_build_failure_is_fail_closed_before_target() {
    let mut fixture = fixture();
    let mut target = Target {
        calls: 0,
        outcome: ExecutionTargetOutcome::Conclusive {
            succeeded: true,
            receipt_digest: digest('6'),
            safe_detail: None,
            typed_result: Some(typed_result(&fixture.operation)),
        },
    };
    let mut audit = Audit;
    let execution_id = fixture.permit.execution_id.clone();
    let result = GovernedExecutionCoordinator::new(&mut fixture.store).execute(
        &command(&fixture.actor_id, "build-fail-claim", '7', 30),
        |_| {
            Err(GovernedExecutionError::CommandBuild {
                execution_id: execution_id.clone(),
                stage: ExecutionCommandBuildStage::Start,
                message: BoundedText::new("trusted clock unavailable").unwrap(),
            })
        },
        || Ok(command(&fixture.actor_id, "build-fail-terminal", '8', 32)),
        &fixture.claim,
        &fixture.operation,
        &mut target,
        &mut audit,
    );

    assert!(matches!(
        result,
        Err(GovernedExecutionError::CommandBuild {
            stage: ExecutionCommandBuildStage::Start,
            ..
        })
    ));
    assert_eq!(target.calls, 0);
    assert_eq!(
        fixture
            .store
            .load_execution_record(&fixture.permit.execution_id)
            .unwrap()
            .broker_state,
        Some(crate::storage::BrokerExecutionState::KnownNoEffect)
    );
}

#[test]
fn post_target_terminal_command_build_failure_propagates_without_retry() {
    let mut fixture = fixture();
    let mut target = Target {
        calls: 0,
        outcome: ExecutionTargetOutcome::Conclusive {
            succeeded: true,
            receipt_digest: digest('7'),
            safe_detail: None,
            typed_result: Some(typed_result(&fixture.operation)),
        },
    };
    let mut audit = Audit;
    let execution_id = fixture.permit.execution_id.clone();
    let result = GovernedExecutionCoordinator::new(&mut fixture.store).execute(
        &command(&fixture.actor_id, "terminal-build-claim", '8', 30),
        |_| Ok(command(&fixture.actor_id, "terminal-build-start", '9', 31)),
        || {
            Err(GovernedExecutionError::CommandBuild {
                execution_id,
                stage: ExecutionCommandBuildStage::Terminal,
                message: BoundedText::new("canonical receipt digest unavailable").unwrap(),
            })
        },
        &fixture.claim,
        &fixture.operation,
        &mut target,
        &mut audit,
    );

    assert!(matches!(
        result,
        Err(GovernedExecutionError::CommandBuild {
            stage: ExecutionCommandBuildStage::Terminal,
            ..
        })
    ));
    assert_eq!(target.calls, 1);
    assert_eq!(
        fixture
            .store
            .load_execution_record(&fixture.permit.execution_id)
            .unwrap()
            .state,
        ExecutionState::Started
    );
}

#[test]
fn changed_operation_never_consumes_or_invokes_target() {
    let mut fixture = fixture();
    fixture.operation.operation_digest = digest('9');
    let mut target = Target {
        calls: 0,
        outcome: ExecutionTargetOutcome::Conclusive {
            succeeded: true,
            receipt_digest: digest('a'),
            safe_detail: None,
            typed_result: Some(typed_result(&fixture.operation)),
        },
    };
    let mut audit = Audit;
    let result = GovernedExecutionCoordinator::new(&mut fixture.store).execute(
        &command(&fixture.actor_id, "claim", 'b', 30),
        |_| Ok(command(&fixture.actor_id, "start", 'd', 31)),
        || Ok(command(&fixture.actor_id, "complete", 'c', 40)),
        &fixture.claim,
        &fixture.operation,
        &mut target,
        &mut audit,
    );

    assert!(matches!(
        result,
        Err(GovernedExecutionError::BindingMismatch)
    ));
    assert_eq!(target.calls, 0);
    assert_eq!(
        fixture
            .store
            .load_permit_record(&fixture.permit.permit_id)
            .unwrap()
            .state,
        PermitState::Issued
    );
}

#[test]
fn audit_failure_immediately_concludes_known_no_effect_without_target_call() {
    let mut fixture = fixture();
    let mut target = Target {
        calls: 0,
        outcome: ExecutionTargetOutcome::Unknown { safe_detail: None },
    };
    let mut audit = FailingAudit;
    let result = GovernedExecutionCoordinator::new(&mut fixture.store).execute(
        &command(&fixture.actor_id, "audit-claim", '0', 30),
        |_| Ok(command(&fixture.actor_id, "audit-start", '1', 31)),
        || Ok(command(&fixture.actor_id, "audit-complete", '2', 40)),
        &fixture.claim,
        &fixture.operation,
        &mut target,
        &mut audit,
    );
    assert!(matches!(
        result,
        Err(GovernedExecutionError::SecurityAuditKnownNoEffect { .. })
    ));
    assert_eq!(target.calls, 0);
    let claimed = fixture
        .store
        .load_execution_record(&fixture.permit.execution_id)
        .unwrap();
    assert_eq!(claimed.state, ExecutionState::Planned);
    assert_eq!(
        claimed.broker_state,
        Some(crate::storage::BrokerExecutionState::KnownNoEffect)
    );
    assert!(claimed.start_audit_proof_digest.is_none());

    let report = fixture.store.recover_gateway(50).unwrap();
    assert_eq!(report.executions_known_no_effect, 0);
    let recovered = fixture
        .store
        .load_execution_record(&fixture.permit.execution_id)
        .unwrap();
    assert_eq!(
        recovered.broker_state,
        Some(crate::storage::BrokerExecutionState::KnownNoEffect)
    );
    assert!(recovered.start_audit_proof_digest.is_none());
}

#[test]
fn unknown_target_result_is_marked_uncertain_and_cannot_retry() {
    let mut fixture = fixture();
    let mut target = Target {
        calls: 0,
        outcome: ExecutionTargetOutcome::Unknown {
            safe_detail: Some(BoundedText::new("connection lost").unwrap()),
        },
    };
    let mut audit = Audit;
    let result = GovernedExecutionCoordinator::new(&mut fixture.store).execute(
        &command(&fixture.actor_id, "claim", 'd', 30),
        |_| Ok(command(&fixture.actor_id, "start", 'e', 31)),
        || Ok(command(&fixture.actor_id, "complete", 'e', 40)),
        &fixture.claim,
        &fixture.operation,
        &mut target,
        &mut audit,
    );
    assert!(matches!(
        result,
        Err(GovernedExecutionError::OutcomeUnknown { .. })
    ));
    assert_eq!(
        fixture
            .store
            .load_execution_record(&fixture.permit.execution_id)
            .unwrap()
            .state,
        ExecutionState::Uncertain
    );

    let retry = GovernedExecutionCoordinator::new(&mut fixture.store).execute(
        &command(&fixture.actor_id, "retry", 'f', 31),
        |_| Ok(command(&fixture.actor_id, "retry-start", '0', 32)),
        || Ok(command(&fixture.actor_id, "retry-complete", '1', 41)),
        &fixture.claim,
        &fixture.operation,
        &mut target,
        &mut audit,
    );
    assert!(matches!(retry, Err(GovernedExecutionError::Store(_))));
    assert_eq!(target.calls, 1);
}

#[test]
fn receipt_failure_is_non_retryable_after_target_returns() {
    let mut fixture = fixture();
    let mut target = Target {
        calls: 0,
        outcome: ExecutionTargetOutcome::Conclusive {
            succeeded: false,
            receipt_digest: digest('2'),
            safe_detail: Some(BoundedText::new("target rejected operation").unwrap()),
            typed_result: None,
        },
    };
    let another_actor = ActorId::new();
    let mut audit = Audit;
    let result = GovernedExecutionCoordinator::new(&mut fixture.store).execute(
        &command(&fixture.actor_id, "claim", '3', 30),
        |_| Ok(command(&fixture.actor_id, "start", '5', 31)),
        || Ok(command(&another_actor, "complete", '4', 40)),
        &fixture.claim,
        &fixture.operation,
        &mut target,
        &mut audit,
    );

    assert!(matches!(
        result,
        Err(GovernedExecutionError::CompletionUnknown { .. })
    ));
    assert_eq!(target.calls, 1);
    assert_eq!(
        fixture
            .store
            .load_execution_record(&fixture.permit.execution_id)
            .unwrap()
            .state,
        ExecutionState::Started
    );
}

#[test]
fn replayed_start_command_never_invokes_target_again() {
    let mut fixture = fixture();
    let claim = command(&fixture.actor_id, "claim-replay", '5', 30);
    let start = command(&fixture.actor_id, "start-replay", '8', 31);
    let complete = command(&fixture.actor_id, "complete-replay", '6', 40);
    let mut target = Target {
        calls: 0,
        outcome: ExecutionTargetOutcome::Conclusive {
            succeeded: true,
            receipt_digest: digest('7'),
            safe_detail: None,
            typed_result: Some(typed_result(&fixture.operation)),
        },
    };
    let mut audit = Audit;

    GovernedExecutionCoordinator::new(&mut fixture.store)
        .execute(
            &claim,
            |_| Ok(start.clone()),
            || Ok(complete.clone()),
            &fixture.claim,
            &fixture.operation,
            &mut target,
            &mut audit,
        )
        .unwrap();
    let replay = GovernedExecutionCoordinator::new(&mut fixture.store).execute(
        &claim,
        |_| Ok(start.clone()),
        || Ok(complete.clone()),
        &fixture.claim,
        &fixture.operation,
        &mut target,
        &mut audit,
    );

    assert!(matches!(
        replay,
        Err(GovernedExecutionError::OutcomeUnknown { .. })
    ));
    assert_eq!(target.calls, 1);
}
