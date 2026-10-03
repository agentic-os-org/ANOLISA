use cosh_gateway_contracts::capability::{ApprovalDecision, ApprovalRequest};
use cosh_gateway_contracts::common::{
    BoundedName, BoundedOpaque, BoundedText, ContractHeader, Correlation, Digest,
    RuntimeBindingRef, RuntimeSelector, TargetRef,
};
use cosh_gateway_contracts::error::{ContractError, ErrorCategory};
use cosh_gateway_contracts::external::{ExternalRef, ExternalRefKind};
use cosh_gateway_contracts::ids::{
    ActorId, AgentSessionId, ApprovalId, ExecutionId, InputRequestId, InstallationId, MessageId,
    PermitId, RequestId, RunId, RuntimeBindingId, RuntimeInstanceId, TaskId, TurnId,
};
use cosh_gateway_contracts::runtime::RuntimeInputRequest;
use cosh_gateway_contracts::task::{
    ApprovalAbandonCause, CancelReason, CancellationStage, ExecutionOutcome, RuntimeUpdate,
    SuspensionCode, TaskEvent, TaskEventEnvelope, TaskEventKind, TaskState, UncertaintyCode,
};

use super::{AggregateError, PendingInputIdentity, RunOutcome, TaskAggregate};

const STATES: [TaskState; 9] = [
    TaskState::Submitted,
    TaskState::Queued,
    TaskState::Running,
    TaskState::WaitingApproval,
    TaskState::WaitingInput,
    TaskState::Suspended,
    TaskState::Succeeded,
    TaskState::Failed,
    TaskState::Cancelled,
];

const EVENT_KINDS: [TaskEventKind; 22] = [
    TaskEventKind::TaskSubmitted,
    TaskEventKind::TaskQueued,
    TaskEventKind::RunStarted,
    TaskEventKind::RuntimeBound,
    TaskEventKind::RuntimeEventRecorded,
    TaskEventKind::InputRequested,
    TaskEventKind::InputSubmitted,
    TaskEventKind::ApprovalRequested,
    TaskEventKind::ApprovalResolved,
    TaskEventKind::ApprovalAbandoned,
    TaskEventKind::ExecutionPlanned,
    TaskEventKind::ExecutionResultRecorded,
    TaskEventKind::ExecutionUncertain,
    TaskEventKind::CancellationRequested,
    TaskEventKind::RunCancelled,
    TaskEventKind::RunSuspended,
    TaskEventKind::RunSucceeded,
    TaskEventKind::RunFailed,
    TaskEventKind::RunRetryQueued,
    TaskEventKind::TaskSucceeded,
    TaskEventKind::TaskFailed,
    TaskEventKind::TaskCancelled,
];

struct Fixture {
    aggregate: TaskAggregate,
    approval_id: ApprovalId,
    input_request_id: InputRequestId,
}

fn fixture(state: TaskState) -> Fixture {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let approval_id = ApprovalId::new();
    let input_request_id = InputRequestId::new();
    let (active_run_id, run_outcome) = match state {
        TaskState::Submitted => (None, RunOutcome::None),
        TaskState::Queued => (Some(run_id), RunOutcome::None),
        TaskState::Running => (Some(run_id), RunOutcome::Active),
        TaskState::WaitingApproval | TaskState::WaitingInput | TaskState::Suspended => {
            (Some(run_id), RunOutcome::Suspended)
        }
        TaskState::Succeeded => (Some(run_id), RunOutcome::Succeeded),
        TaskState::Failed => (Some(run_id), RunOutcome::Failed),
        TaskState::Cancelled => (Some(run_id), RunOutcome::Cancelled),
    };
    let pending_approvals = if state == TaskState::WaitingApproval {
        [approval_id.clone()].into_iter().collect()
    } else {
        Default::default()
    };
    let pending_input = if state == TaskState::WaitingInput {
        Some(PendingInputIdentity {
            request_id: input_request_id.clone(),
            run_id: active_run_id.clone().unwrap(),
        })
    } else {
        None
    };
    Fixture {
        aggregate: TaskAggregate {
            task_id,
            owner_actor_id: actor_id,
            target: target(),
            revision: 41,
            state,
            active_run_id,
            run_outcome,
            cancellation_requested: false,
            pending_approvals,
            pending_input,
            planned_executions: Default::default(),
        },
        approval_id,
        input_request_id,
    }
}

fn target() -> TargetRef {
    TargetRef {
        kind: BoundedName::new("local").unwrap(),
        authority: BoundedName::new("matrix").unwrap(),
        identifier: BoundedOpaque::new("target").unwrap(),
    }
}

fn error() -> ContractError {
    ContractError::new(
        "matrix_failure",
        ErrorCategory::Internal,
        false,
        "matrix failure",
    )
    .unwrap()
}

fn envelope(aggregate: &TaskAggregate, event: TaskEvent) -> TaskEventEnvelope {
    let revision = aggregate.revision + 1;
    let mut correlation = Correlation::new(InstallationId::new());
    correlation.actor_id = Some(aggregate.owner_actor_id.clone());
    correlation.task_id = Some(aggregate.task_id.clone());
    TaskEventEnvelope {
        header: ContractHeader::new(
            cosh_gateway_contracts::common::ContractSchema::TaskEvent,
            MessageId::new(),
            revision,
            correlation,
        ),
        task_id: aggregate.task_id.clone(),
        revision,
        event,
    }
}

fn active_run(aggregate: &TaskAggregate) -> RunId {
    aggregate.active_run_id.clone().unwrap_or_default()
}

fn prepare_event(fixture: &mut Fixture, kind: TaskEventKind) -> TaskEvent {
    let aggregate = &mut fixture.aggregate;
    let run_id = active_run(aggregate);
    match kind {
        TaskEventKind::TaskSubmitted => TaskEvent::TaskSubmitted {
            intent_digest: Digest::parse("a".repeat(64)).unwrap(),
            target: target(),
        },
        TaskEventKind::TaskQueued => TaskEvent::TaskQueued {
            run_id: RunId::new(),
            runtime: RuntimeSelector {
                runtime: BoundedName::new("core").unwrap(),
                profile: None,
            },
        },
        TaskEventKind::RunStarted => TaskEvent::RunStarted { run_id },
        TaskEventKind::RuntimeBound => TaskEvent::RuntimeBound {
            run_id: run_id.clone(),
            binding: RuntimeBindingRef {
                binding_id: RuntimeBindingId::new(),
                task_id: aggregate.task_id.clone(),
                run_id,
                agent_session_id: AgentSessionId::new(),
                runtime_instance_id: RuntimeInstanceId::new(),
                runtime_generation: 1,
                external_session: ExternalRef {
                    kind: ExternalRefKind::AcpSession,
                    authority: BoundedName::new("matrix").unwrap(),
                    scope_digest: Digest::parse("b".repeat(64)).unwrap(),
                    value: BoundedOpaque::new("session").unwrap(),
                },
            },
        },
        TaskEventKind::RuntimeEventRecorded => TaskEvent::RuntimeEventRecorded {
            run_id,
            update: RuntimeUpdate::Progress {
                summary: BoundedText::new("progress").unwrap(),
            },
        },
        TaskEventKind::InputRequested => TaskEvent::InputRequested {
            request: RuntimeInputRequest::new(
                InputRequestId::new(),
                run_id,
                TurnId::new(),
                None,
                BoundedText::new("matrix input").unwrap(),
                Vec::new(),
                true,
                false,
            )
            .unwrap(),
        },
        TaskEventKind::InputSubmitted => TaskEvent::InputSubmitted {
            request_id: fixture.input_request_id.clone(),
            run_id,
            response_digest: Digest::parse("c".repeat(64)).unwrap(),
        },
        TaskEventKind::ApprovalRequested => TaskEvent::ApprovalRequested {
            approval: ApprovalRequest {
                approval_id: ApprovalId::new(),
                request_id: RequestId::new(),
                task_id: aggregate.task_id.clone(),
                run_id,
                summary: BoundedText::new("approve matrix operation").unwrap(),
                expires_at_ms: 100,
            },
        },
        TaskEventKind::ApprovalResolved => TaskEvent::ApprovalResolved {
            approval_id: fixture.approval_id.clone(),
            decision: ApprovalDecision::Approve,
        },
        TaskEventKind::ApprovalAbandoned => TaskEvent::ApprovalAbandoned {
            approval_id: fixture.approval_id.clone(),
            cause: ApprovalAbandonCause::ProviderCancelled,
        },
        TaskEventKind::ExecutionPlanned => TaskEvent::ExecutionPlanned {
            execution_id: ExecutionId::new(),
            permit_id: PermitId::new(),
        },
        TaskEventKind::ExecutionResultRecorded => {
            let execution_id = ExecutionId::new();
            aggregate.planned_executions.insert(execution_id.clone());
            TaskEvent::ExecutionResultRecorded {
                execution_id,
                outcome: ExecutionOutcome::Succeeded { evidence_ref: None },
            }
        }
        TaskEventKind::ExecutionUncertain => {
            let execution_id = ExecutionId::new();
            aggregate.planned_executions.insert(execution_id.clone());
            TaskEvent::ExecutionUncertain {
                execution_id,
                reason: UncertaintyCode::TransportLost,
            }
        }
        TaskEventKind::CancellationRequested => TaskEvent::CancellationRequested {
            run_id,
            cause: CancelReason::UserRequested,
        },
        TaskEventKind::RunCancelled => {
            aggregate.cancellation_requested = true;
            TaskEvent::RunCancelled {
                run_id,
                stage: CancellationStage::Runtime,
            }
        }
        TaskEventKind::RunSuspended => TaskEvent::RunSuspended {
            run_id,
            reason: SuspensionCode::RuntimeUnavailable,
        },
        TaskEventKind::RunSucceeded => TaskEvent::RunSucceeded { run_id },
        TaskEventKind::RunFailed => TaskEvent::RunFailed {
            run_id,
            error: error(),
        },
        TaskEventKind::RunRetryQueued => TaskEvent::RunRetryQueued {
            previous_run_id: run_id,
            next_run_id: RunId::new(),
        },
        TaskEventKind::TaskSucceeded => {
            if aggregate.state == TaskState::Running {
                aggregate.run_outcome = RunOutcome::Succeeded;
            }
            TaskEvent::TaskSucceeded
        }
        TaskEventKind::TaskFailed => {
            if aggregate.state == TaskState::Suspended {
                aggregate.run_outcome = RunOutcome::Failed;
            }
            TaskEvent::TaskFailed { error: error() }
        }
        TaskEventKind::TaskCancelled => {
            if aggregate.state == TaskState::Queued {
                aggregate.cancellation_requested = true;
            } else if aggregate.state == TaskState::Suspended {
                aggregate.cancellation_requested = true;
                aggregate.run_outcome = RunOutcome::Cancelled;
            }
            TaskEvent::TaskCancelled
        }
    }
}

fn expected(state: TaskState, kind: TaskEventKind) -> bool {
    match state {
        TaskState::Submitted => matches!(
            kind,
            TaskEventKind::TaskQueued | TaskEventKind::TaskCancelled
        ),
        TaskState::Queued => matches!(
            kind,
            TaskEventKind::RunStarted
                | TaskEventKind::CancellationRequested
                | TaskEventKind::RunCancelled
                | TaskEventKind::RunSuspended
                | TaskEventKind::RunFailed
                | TaskEventKind::TaskCancelled
        ),
        TaskState::Running => matches!(
            kind,
            TaskEventKind::RuntimeBound
                | TaskEventKind::RuntimeEventRecorded
                | TaskEventKind::InputRequested
                | TaskEventKind::ApprovalRequested
                | TaskEventKind::ExecutionPlanned
                | TaskEventKind::ExecutionResultRecorded
                | TaskEventKind::ExecutionUncertain
                | TaskEventKind::CancellationRequested
                | TaskEventKind::RunCancelled
                | TaskEventKind::RunSuspended
                | TaskEventKind::RunSucceeded
                | TaskEventKind::RunFailed
                | TaskEventKind::TaskSucceeded
        ),
        TaskState::WaitingApproval => matches!(
            kind,
            TaskEventKind::ApprovalResolved
                | TaskEventKind::ApprovalAbandoned
                | TaskEventKind::CancellationRequested
                | TaskEventKind::RunCancelled
                | TaskEventKind::RunFailed
        ),
        TaskState::WaitingInput => matches!(
            kind,
            TaskEventKind::InputSubmitted
                | TaskEventKind::CancellationRequested
                | TaskEventKind::RunCancelled
                | TaskEventKind::RunSuspended
                | TaskEventKind::RunFailed
        ),
        TaskState::Suspended => matches!(
            kind,
            TaskEventKind::CancellationRequested
                | TaskEventKind::RunCancelled
                | TaskEventKind::RunFailed
                | TaskEventKind::RunRetryQueued
                | TaskEventKind::TaskFailed
                | TaskEventKind::TaskCancelled
        ),
        TaskState::Succeeded | TaskState::Failed | TaskState::Cancelled => false,
    }
}

#[test]
fn every_task_state_event_pair_has_an_explicit_transition_result() {
    let mut checked = 0;
    for state in STATES {
        for kind in EVENT_KINDS {
            let mut fixture = fixture(state);
            let event = prepare_event(&mut fixture, kind);
            let before = fixture.aggregate.clone();
            let envelope = envelope(&fixture.aggregate, event);
            let result = fixture.aggregate.apply(&envelope);

            if expected(state, kind) {
                assert!(
                    result.is_ok(),
                    "{kind:?} must be accepted from {state:?}, got {result:?}"
                );
                assert_eq!(fixture.aggregate.revision(), before.revision() + 1);
                if state == TaskState::WaitingInput
                    && matches!(kind, TaskEventKind::RunCancelled | TaskEventKind::RunFailed)
                {
                    assert!(fixture.aggregate.pending_input.is_none());
                }
            } else {
                assert!(
                    matches!(result, Err(AggregateError::InvalidTransition { .. })),
                    "{kind:?} must be rejected from {state:?}, got {result:?}"
                );
                assert_eq!(fixture.aggregate, before);
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 198);
}

#[test]
fn retry_and_terminal_cancel_clear_legacy_pending_input_identity() {
    let mut retry = fixture(TaskState::Suspended);
    let run_id = active_run(&retry.aggregate);
    retry.aggregate.pending_input = Some(PendingInputIdentity {
        request_id: retry.input_request_id.clone(),
        run_id: run_id.clone(),
    });
    let event = envelope(
        &retry.aggregate,
        TaskEvent::RunRetryQueued {
            previous_run_id: run_id,
            next_run_id: RunId::new(),
        },
    );
    retry.aggregate.apply(&event).unwrap();
    assert!(retry.aggregate.pending_input.is_none());

    let mut cancelled = fixture(TaskState::Suspended);
    let run_id = active_run(&cancelled.aggregate);
    cancelled.aggregate.run_outcome = RunOutcome::Cancelled;
    cancelled.aggregate.cancellation_requested = true;
    cancelled.aggregate.pending_input = Some(PendingInputIdentity {
        request_id: cancelled.input_request_id.clone(),
        run_id,
    });
    let event = envelope(&cancelled.aggregate, TaskEvent::TaskCancelled);
    cancelled.aggregate.apply(&event).unwrap();
    assert!(cancelled.aggregate.pending_input.is_none());
}

#[test]
fn terminal_states_are_absorbing_for_every_event_kind() {
    for state in [
        TaskState::Succeeded,
        TaskState::Failed,
        TaskState::Cancelled,
    ] {
        for kind in EVENT_KINDS {
            let mut fixture = fixture(state);
            let event = prepare_event(&mut fixture, kind);
            let before = fixture.aggregate.clone();
            let envelope = envelope(&fixture.aggregate, event);
            assert!(matches!(
                fixture.aggregate.apply(&envelope),
                Err(AggregateError::InvalidTransition { .. })
            ));
            assert_eq!(fixture.aggregate, before, "{state:?} accepted {kind:?}");
        }
    }
}

#[test]
fn every_state_rejects_stale_and_future_revisions_without_mutation() {
    for state in STATES {
        for revision in [40, 43] {
            let mut fixture = fixture(state);
            let event = prepare_event(&mut fixture, TaskEventKind::TaskSubmitted);
            let before = fixture.aggregate.clone();
            let mut envelope = envelope(&fixture.aggregate, event);
            envelope.revision = revision;
            assert!(matches!(
                fixture.aggregate.apply(&envelope),
                Err(AggregateError::RevisionGap {
                    expected: 42,
                    actual
                }) if actual == revision
            ));
            assert_eq!(fixture.aggregate, before);
        }
    }
}

#[test]
fn maximum_revision_rejects_an_event_without_mutation() {
    let mut fixture = fixture(TaskState::Submitted);
    let event = prepare_event(&mut fixture, TaskEventKind::TaskCancelled);
    let mut envelope = envelope(&fixture.aggregate, event);
    envelope.revision = u64::MAX;
    fixture.aggregate.revision = u64::MAX;
    let before = fixture.aggregate.clone();

    assert_eq!(
        fixture.aggregate.apply(&envelope),
        Err(AggregateError::RevisionOverflow)
    );
    assert_eq!(fixture.aggregate, before);
}

#[test]
fn cancellation_converges_from_every_active_state() {
    for state in [
        TaskState::Queued,
        TaskState::Running,
        TaskState::WaitingApproval,
        TaskState::WaitingInput,
        TaskState::Suspended,
    ] {
        let mut fixture = fixture(state);
        let requested = prepare_event(&mut fixture, TaskEventKind::CancellationRequested);
        let requested = envelope(&fixture.aggregate, requested);
        fixture.aggregate.apply(&requested).unwrap();
        assert!(fixture.aggregate.cancellation_requested());

        let cancelled = prepare_event(&mut fixture, TaskEventKind::RunCancelled);
        let cancelled = envelope(&fixture.aggregate, cancelled);
        fixture.aggregate.apply(&cancelled).unwrap();
        assert_eq!(fixture.aggregate.state(), TaskState::Suspended);

        let completed = prepare_event(&mut fixture, TaskEventKind::TaskCancelled);
        let completed = envelope(&fixture.aggregate, completed);
        fixture.aggregate.apply(&completed).unwrap();
        assert_eq!(fixture.aggregate.state(), TaskState::Cancelled);
    }
}

#[test]
fn approval_input_and_suspend_boundaries_fail_closed() {
    let mut waiting_approval = fixture(TaskState::WaitingApproval);
    let unknown = TaskEvent::ApprovalResolved {
        approval_id: ApprovalId::new(),
        decision: ApprovalDecision::Approve,
    };
    let before = waiting_approval.aggregate.clone();
    let unknown = envelope(&waiting_approval.aggregate, unknown);
    assert_eq!(
        waiting_approval.aggregate.apply(&unknown),
        Err(AggregateError::ApprovalNotPending)
    );
    assert_eq!(waiting_approval.aggregate, before);

    let unknown = TaskEvent::ApprovalAbandoned {
        approval_id: ApprovalId::new(),
        cause: ApprovalAbandonCause::ProviderCancelled,
    };
    let before = waiting_approval.aggregate.clone();
    let unknown = envelope(&waiting_approval.aggregate, unknown);
    assert_eq!(
        waiting_approval.aggregate.apply(&unknown),
        Err(AggregateError::ApprovalNotPending)
    );
    assert_eq!(waiting_approval.aggregate, before);

    let mut waiting_input = fixture(TaskState::WaitingInput);
    let retry = prepare_event(&mut waiting_input, TaskEventKind::RunRetryQueued);
    let before = waiting_input.aggregate.clone();
    let retry = envelope(&waiting_input.aggregate, retry);
    assert!(matches!(
        waiting_input.aggregate.apply(&retry),
        Err(AggregateError::InvalidTransition { .. })
    ));
    assert_eq!(waiting_input.aggregate, before);

    let mut suspended = fixture(TaskState::Suspended);
    suspended.aggregate.cancellation_requested = true;
    let retry = prepare_event(&mut suspended, TaskEventKind::RunRetryQueued);
    let before = suspended.aggregate.clone();
    let retry = envelope(&suspended.aggregate, retry);
    assert!(matches!(
        suspended.aggregate.apply(&retry),
        Err(AggregateError::InvalidTransition { .. })
    ));
    assert_eq!(suspended.aggregate, before);
}
