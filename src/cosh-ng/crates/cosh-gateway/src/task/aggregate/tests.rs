use cosh_gateway_contracts::capability::ApprovalDecision;
use cosh_gateway_contracts::common::{
    BoundedName, BoundedOpaque, BoundedText, ContractHeader, Correlation, Digest, RuntimeSelector,
};
use cosh_gateway_contracts::error::{ContractError, ErrorCategory};
use cosh_gateway_contracts::ids::{
    InputRequestId, InstallationId, MessageId, PermitId, RequestId, TurnId,
};
use cosh_gateway_contracts::runtime::RuntimeInputRequest;
use cosh_gateway_contracts::task::{
    CancelReason, CancellationStage, RuntimeUpdate, SuspensionCode, UncertaintyCode,
};

use super::*;

fn target() -> TargetRef {
    TargetRef {
        kind: BoundedName::new("local").unwrap(),
        authority: BoundedName::new("test").unwrap(),
        identifier: BoundedOpaque::new("target").unwrap(),
    }
}

fn envelope(
    task_id: &TaskId,
    actor_id: &ActorId,
    revision: u64,
    event: TaskEvent,
) -> TaskEventEnvelope {
    let mut correlation = Correlation::new(InstallationId::new());
    correlation.actor_id = Some(actor_id.clone());
    correlation.task_id = Some(task_id.clone());
    TaskEventEnvelope {
        header: ContractHeader::new(
            ContractSchema::TaskEvent,
            MessageId::new(),
            revision,
            correlation,
        ),
        task_id: task_id.clone(),
        revision,
        event,
    }
}

fn submitted(task_id: &TaskId, actor_id: &ActorId) -> TaskEventEnvelope {
    envelope(
        task_id,
        actor_id,
        1,
        TaskEvent::TaskSubmitted {
            intent_digest: Digest::parse("a".repeat(64)).unwrap(),
            target: target(),
        },
    )
}

fn running(task_id: &TaskId, actor_id: &ActorId, run_id: &RunId) -> TaskAggregate {
    TaskAggregate::replay(&[
        submitted(task_id, actor_id),
        envelope(
            task_id,
            actor_id,
            2,
            TaskEvent::TaskQueued {
                run_id: run_id.clone(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("core").unwrap(),
                    profile: None,
                },
            },
        ),
        envelope(
            task_id,
            actor_id,
            3,
            TaskEvent::RunStarted {
                run_id: run_id.clone(),
            },
        ),
    ])
    .unwrap()
}

fn plan_execution(
    aggregate: &mut TaskAggregate,
    task_id: &TaskId,
    actor_id: &ActorId,
    revision: u64,
) -> ExecutionId {
    let execution_id = ExecutionId::new();
    aggregate
        .apply(&envelope(
            task_id,
            actor_id,
            revision,
            TaskEvent::ExecutionPlanned {
                execution_id: execution_id.clone(),
                permit_id: PermitId::new(),
            },
        ))
        .unwrap();
    execution_id
}

fn input_request(run_id: &RunId) -> RuntimeInputRequest {
    RuntimeInputRequest::new(
        InputRequestId::new(),
        run_id.clone(),
        TurnId::new(),
        None,
        BoundedText::new("Need bounded input").unwrap(),
        Vec::new(),
        true,
        false,
    )
    .unwrap()
}

#[test]
fn input_request_and_digest_only_submission_are_exact_and_single_use() {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let mut aggregate = running(&task_id, &actor_id, &run_id);
    let request = input_request(&run_id);
    aggregate
        .apply(&envelope(
            &task_id,
            &actor_id,
            4,
            TaskEvent::InputRequested {
                request: request.clone(),
            },
        ))
        .unwrap();
    assert_eq!(aggregate.state(), TaskState::WaitingInput);
    assert_eq!(
        aggregate.pending_input_request_id(),
        Some(request.request_id())
    );

    let wrong = envelope(
        &task_id,
        &actor_id,
        5,
        TaskEvent::InputSubmitted {
            request_id: InputRequestId::new(),
            run_id: run_id.clone(),
            response_digest: Digest::parse("b".repeat(64)).unwrap(),
        },
    );
    let before = aggregate.clone();
    assert_eq!(
        aggregate.apply(&wrong),
        Err(AggregateError::InputNotPending)
    );
    assert_eq!(aggregate, before);

    aggregate
        .apply(&envelope(
            &task_id,
            &actor_id,
            5,
            TaskEvent::InputSubmitted {
                request_id: request.request_id().clone(),
                run_id: run_id.clone(),
                response_digest: Digest::parse("c".repeat(64)).unwrap(),
            },
        ))
        .unwrap();
    assert_eq!(aggregate.state(), TaskState::Running);
    assert!(aggregate.pending_input_request_id().is_none());

    let duplicate = envelope(
        &task_id,
        &actor_id,
        6,
        TaskEvent::InputSubmitted {
            request_id: request.request_id().clone(),
            run_id,
            response_digest: Digest::parse("c".repeat(64)).unwrap(),
        },
    );
    let before = aggregate.clone();
    assert!(matches!(
        aggregate.apply(&duplicate),
        Err(AggregateError::InvalidTransition { .. })
    ));
    assert_eq!(aggregate, before);
}

#[test]
fn input_request_rejects_cancellation_and_planned_execution_conflicts() {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    for planned in [false, true] {
        let mut aggregate = running(&task_id, &actor_id, &run_id);
        if planned {
            plan_execution(&mut aggregate, &task_id, &actor_id, 4);
        } else {
            aggregate.cancellation_requested = true;
        }
        let revision = aggregate.revision() + 1;
        let request = envelope(
            &task_id,
            &actor_id,
            revision,
            TaskEvent::InputRequested {
                request: input_request(&run_id),
            },
        );
        let before = aggregate.clone();
        assert!(matches!(
            aggregate.apply(&request),
            Err(AggregateError::InvalidTransition { .. })
        ));
        assert_eq!(aggregate, before);
    }
}

#[test]
fn reducer_accepts_success_lifecycle() {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let events = vec![
        submitted(&task_id, &actor_id),
        envelope(
            &task_id,
            &actor_id,
            2,
            TaskEvent::TaskQueued {
                run_id: run_id.clone(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("cosh_core").unwrap(),
                    profile: None,
                },
            },
        ),
        envelope(
            &task_id,
            &actor_id,
            3,
            TaskEvent::RunStarted {
                run_id: run_id.clone(),
            },
        ),
        envelope(
            &task_id,
            &actor_id,
            4,
            TaskEvent::RunSucceeded {
                run_id: run_id.clone(),
            },
        ),
        envelope(&task_id, &actor_id, 5, TaskEvent::TaskSucceeded),
    ];

    let aggregate = TaskAggregate::replay(&events).unwrap();
    assert_eq!(aggregate.state(), TaskState::Succeeded);
    assert_eq!(aggregate.revision(), 5);
    assert_eq!(aggregate.active_run_id(), Some(&run_id));
}

#[test]
fn reducer_rejects_revision_gap_without_mutation() {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let mut aggregate = TaskAggregate::replay(&[submitted(&task_id, &actor_id)]).unwrap();
    let before = aggregate.clone();
    let error = aggregate
        .apply(&envelope(
            &task_id,
            &actor_id,
            3,
            TaskEvent::TaskQueued {
                run_id: RunId::new(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("core").unwrap(),
                    profile: None,
                },
            },
        ))
        .unwrap_err();
    assert!(matches!(error, AggregateError::RevisionGap { .. }));
    assert_eq!(aggregate, before);
}

#[test]
fn reducer_rejects_in_memory_unsupported_schema_version() {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let mut event = submitted(&task_id, &actor_id);
    event.header.schema_version = TASK_EVENT_SCHEMA_VERSION + 1;

    assert!(matches!(
        TaskAggregate::replay(&[event]),
        Err(AggregateError::WrongSchemaVersion { .. })
    ));
}

#[test]
fn approval_denial_returns_the_active_runtime_to_running() {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let approval_id = ApprovalId::new();
    let mut aggregate = TaskAggregate::replay(&[
        submitted(&task_id, &actor_id),
        envelope(
            &task_id,
            &actor_id,
            2,
            TaskEvent::TaskQueued {
                run_id: run_id.clone(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("core").unwrap(),
                    profile: None,
                },
            },
        ),
        envelope(
            &task_id,
            &actor_id,
            3,
            TaskEvent::RunStarted {
                run_id: run_id.clone(),
            },
        ),
    ])
    .unwrap();

    aggregate
        .apply(&envelope(
            &task_id,
            &actor_id,
            4,
            TaskEvent::ApprovalRequested {
                approval: cosh_gateway_contracts::capability::ApprovalRequest {
                    approval_id: approval_id.clone(),
                    request_id: RequestId::new(),
                    task_id: task_id.clone(),
                    run_id,
                    summary: BoundedText::new("approve package update").unwrap(),
                    expires_at_ms: 100,
                },
            },
        ))
        .unwrap();
    assert_eq!(aggregate.state(), TaskState::WaitingApproval);

    aggregate
        .apply(&envelope(
            &task_id,
            &actor_id,
            5,
            TaskEvent::ApprovalResolved {
                approval_id,
                decision: ApprovalDecision::Deny,
            },
        ))
        .unwrap();
    assert_eq!(aggregate.state(), TaskState::Running);
}

#[test]
fn runtime_loss_settles_a_task_waiting_for_approval() {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let approval_id = ApprovalId::new();
    let mut aggregate = TaskAggregate::replay(&[
        submitted(&task_id, &actor_id),
        envelope(
            &task_id,
            &actor_id,
            2,
            TaskEvent::TaskQueued {
                run_id: run_id.clone(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("acp").unwrap(),
                    profile: None,
                },
            },
        ),
        envelope(
            &task_id,
            &actor_id,
            3,
            TaskEvent::RunStarted {
                run_id: run_id.clone(),
            },
        ),
        envelope(
            &task_id,
            &actor_id,
            4,
            TaskEvent::ApprovalRequested {
                approval: cosh_gateway_contracts::capability::ApprovalRequest {
                    approval_id,
                    request_id: RequestId::new(),
                    task_id: task_id.clone(),
                    run_id: run_id.clone(),
                    summary: BoundedText::new("approve provider tool").unwrap(),
                    expires_at_ms: 100,
                },
            },
        ),
    ])
    .unwrap();

    let error = ContractError::new(
        "runtime_lost",
        ErrorCategory::RuntimeUnavailable,
        false,
        "Runtime cannot be reattached",
    )
    .unwrap();
    aggregate
        .apply(&envelope(
            &task_id,
            &actor_id,
            5,
            TaskEvent::RunFailed {
                run_id,
                error: error.clone(),
            },
        ))
        .unwrap();
    aggregate
        .apply(&envelope(
            &task_id,
            &actor_id,
            6,
            TaskEvent::TaskFailed { error },
        ))
        .unwrap();
    assert_eq!(aggregate.state(), TaskState::Failed);
}

#[test]
fn terminal_task_cannot_reopen() {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let mut aggregate = TaskAggregate::replay(&[
        submitted(&task_id, &actor_id),
        envelope(
            &task_id,
            &actor_id,
            2,
            TaskEvent::TaskQueued {
                run_id: run_id.clone(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("core").unwrap(),
                    profile: None,
                },
            },
        ),
        envelope(
            &task_id,
            &actor_id,
            3,
            TaskEvent::RunStarted {
                run_id: run_id.clone(),
            },
        ),
        envelope(&task_id, &actor_id, 4, TaskEvent::RunSucceeded { run_id }),
        envelope(&task_id, &actor_id, 5, TaskEvent::TaskSucceeded),
    ])
    .unwrap();

    assert!(matches!(
        aggregate.apply(&envelope(
            &task_id,
            &actor_id,
            6,
            TaskEvent::TaskQueued {
                run_id: RunId::new(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("core").unwrap(),
                    profile: None,
                },
            },
        )),
        Err(AggregateError::InvalidTransition { .. })
    ));
}

#[test]
fn run_terminal_fact_rejects_later_runtime_events() {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let mut aggregate = TaskAggregate::replay(&[
        submitted(&task_id, &actor_id),
        envelope(
            &task_id,
            &actor_id,
            2,
            TaskEvent::TaskQueued {
                run_id: run_id.clone(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("core").unwrap(),
                    profile: None,
                },
            },
        ),
        envelope(
            &task_id,
            &actor_id,
            3,
            TaskEvent::RunStarted {
                run_id: run_id.clone(),
            },
        ),
        envelope(
            &task_id,
            &actor_id,
            4,
            TaskEvent::RunSucceeded {
                run_id: run_id.clone(),
            },
        ),
    ])
    .unwrap();
    let before = aggregate.clone();

    assert!(matches!(
        aggregate.apply(&envelope(
            &task_id,
            &actor_id,
            5,
            TaskEvent::RuntimeEventRecorded {
                run_id: run_id.clone(),
                update: RuntimeUpdate::Progress {
                    summary: BoundedText::new("late progress").unwrap(),
                },
            },
        )),
        Err(AggregateError::InvalidTransition { .. })
    ));
    assert_eq!(aggregate, before);
    assert!(matches!(
        aggregate.apply(&envelope(
            &task_id,
            &actor_id,
            5,
            TaskEvent::RunSucceeded { run_id },
        )),
        Err(AggregateError::InvalidTransition { .. })
    ));
    assert_eq!(aggregate, before);
}

#[test]
fn unresolved_planned_execution_blocks_run_completion() {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();

    let mut suspended = running(&task_id, &actor_id, &run_id);
    plan_execution(&mut suspended, &task_id, &actor_id, 4);
    let before = suspended.clone();
    assert!(matches!(
        suspended.apply(&envelope(
            &task_id,
            &actor_id,
            5,
            TaskEvent::RunSuspended {
                run_id: run_id.clone(),
                reason: SuspensionCode::RuntimeUnavailable,
            },
        )),
        Err(AggregateError::InvalidTransition { .. })
    ));
    assert_eq!(suspended, before);

    let mut failed = before.clone();
    assert!(matches!(
        failed.apply(&envelope(
            &task_id,
            &actor_id,
            5,
            TaskEvent::RunFailed {
                run_id: run_id.clone(),
                error: ContractError::new(
                    "runtime_lost",
                    ErrorCategory::RuntimeUnavailable,
                    true,
                    "runtime lost",
                )
                .unwrap(),
            },
        )),
        Err(AggregateError::InvalidTransition { .. })
    ));
    assert_eq!(failed, before);

    let mut cancelled = before.clone();
    cancelled
        .apply(&envelope(
            &task_id,
            &actor_id,
            5,
            TaskEvent::CancellationRequested {
                run_id: run_id.clone(),
                cause: CancelReason::UserRequested,
            },
        ))
        .unwrap();
    let before_cancelled = cancelled.clone();
    assert!(matches!(
        cancelled.apply(&envelope(
            &task_id,
            &actor_id,
            6,
            TaskEvent::RunCancelled {
                run_id,
                stage: CancellationStage::Execution,
            },
        )),
        Err(AggregateError::InvalidTransition { .. })
    ));
    assert_eq!(cancelled, before_cancelled);
}

#[test]
fn retry_rejects_planned_or_uncertain_execution() {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let next_run_id = RunId::new();
    let approval_id = ApprovalId::new();
    let mut planned = running(&task_id, &actor_id, &run_id);
    plan_execution(&mut planned, &task_id, &actor_id, 4);
    planned
        .apply(&envelope(
            &task_id,
            &actor_id,
            5,
            TaskEvent::ApprovalRequested {
                approval: cosh_gateway_contracts::capability::ApprovalRequest {
                    approval_id: approval_id.clone(),
                    request_id: RequestId::new(),
                    task_id: task_id.clone(),
                    run_id: run_id.clone(),
                    summary: BoundedText::new("approve execution").unwrap(),
                    expires_at_ms: 100,
                },
            },
        ))
        .unwrap();
    planned
        .apply(&envelope(
            &task_id,
            &actor_id,
            6,
            TaskEvent::ApprovalResolved {
                approval_id,
                decision: ApprovalDecision::Deny,
            },
        ))
        .unwrap();
    let before_planned_retry = planned.clone();
    assert!(matches!(
        planned.apply(&envelope(
            &task_id,
            &actor_id,
            7,
            TaskEvent::RunRetryQueued {
                previous_run_id: run_id.clone(),
                next_run_id: next_run_id.clone(),
            },
        )),
        Err(AggregateError::InvalidTransition { .. })
    ));
    assert_eq!(planned, before_planned_retry);

    let mut uncertain = running(&task_id, &actor_id, &run_id);
    let execution_id = plan_execution(&mut uncertain, &task_id, &actor_id, 4);
    uncertain
        .apply(&envelope(
            &task_id,
            &actor_id,
            5,
            TaskEvent::ExecutionUncertain {
                execution_id,
                reason: UncertaintyCode::TransportLost,
            },
        ))
        .unwrap();
    let before_uncertain_retry = uncertain.clone();
    assert!(matches!(
        uncertain.apply(&envelope(
            &task_id,
            &actor_id,
            6,
            TaskEvent::RunRetryQueued {
                previous_run_id: run_id,
                next_run_id,
            },
        )),
        Err(AggregateError::InvalidTransition { .. })
    ));
    assert_eq!(uncertain, before_uncertain_retry);

    let mut failed_uncertain = before_uncertain_retry.clone();
    let failed_run_id = failed_uncertain.active_run_id().unwrap().clone();
    assert!(matches!(
        failed_uncertain.apply(&envelope(
            &task_id,
            &actor_id,
            6,
            TaskEvent::RunFailed {
                run_id: failed_run_id,
                error: ContractError::new(
                    "uncertain_execution",
                    ErrorCategory::Conflict,
                    false,
                    "execution outcome is uncertain",
                )
                .unwrap(),
            },
        )),
        Err(AggregateError::InvalidTransition { .. })
    ));
    assert_eq!(failed_uncertain, before_uncertain_retry);

    let mut cancelled_uncertain = before_uncertain_retry.clone();
    let uncertain_run_id = cancelled_uncertain.active_run_id().unwrap().clone();
    assert!(!cancelled_uncertain.active_run_can_be_cancelled(&uncertain_run_id));
    cancelled_uncertain
        .apply(&envelope(
            &task_id,
            &actor_id,
            6,
            TaskEvent::CancellationRequested {
                run_id: uncertain_run_id.clone(),
                cause: CancelReason::UserRequested,
            },
        ))
        .unwrap();
    let before_uncertain_cancel = cancelled_uncertain.clone();
    assert!(matches!(
        cancelled_uncertain.apply(&envelope(
            &task_id,
            &actor_id,
            7,
            TaskEvent::RunCancelled {
                run_id: uncertain_run_id,
                stage: CancellationStage::Execution,
            },
        )),
        Err(AggregateError::InvalidTransition { .. })
    ));
    assert_eq!(cancelled_uncertain, before_uncertain_cancel);
}

#[test]
fn retryable_failed_run_can_be_explicitly_abandoned() {
    let task_id = TaskId::new();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let mut aggregate = running(&task_id, &actor_id, &run_id);
    aggregate
        .apply(&envelope(
            &task_id,
            &actor_id,
            4,
            TaskEvent::RunFailed {
                run_id: run_id.clone(),
                error: ContractError::new(
                    "retryable_runtime_failure",
                    ErrorCategory::RuntimeUnavailable,
                    true,
                    "Runtime is temporarily unavailable",
                )
                .unwrap(),
            },
        ))
        .unwrap();
    assert!(aggregate.active_run_can_be_cancelled(&run_id));
    aggregate
        .apply(&envelope(
            &task_id,
            &actor_id,
            5,
            TaskEvent::CancellationRequested {
                run_id: run_id.clone(),
                cause: CancelReason::UserRequested,
            },
        ))
        .unwrap();
    aggregate
        .apply(&envelope(
            &task_id,
            &actor_id,
            6,
            TaskEvent::RunCancelled {
                run_id,
                stage: CancellationStage::Runtime,
            },
        ))
        .unwrap();
    aggregate
        .apply(&envelope(&task_id, &actor_id, 7, TaskEvent::TaskCancelled))
        .unwrap();
    assert_eq!(aggregate.state(), TaskState::Cancelled);
}
