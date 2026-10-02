//! Pure reducer for versioned Task lifecycle events.

use std::collections::BTreeSet;

use cosh_gateway_contracts::common::{ContractSchema, TargetRef, TASK_EVENT_SCHEMA_VERSION};
use cosh_gateway_contracts::ids::{
    ActorId, ApprovalId, ExecutionId, InputRequestId, RunId, TaskId,
};
use cosh_gateway_contracts::task::{TaskEvent, TaskEventEnvelope, TaskState};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Task projection reduced exclusively from immutable Task events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAggregate {
    task_id: TaskId,
    owner_actor_id: ActorId,
    target: TargetRef,
    revision: u64,
    state: TaskState,
    active_run_id: Option<RunId>,
    run_outcome: RunOutcome,
    cancellation_requested: bool,
    pending_approvals: BTreeSet<ApprovalId>,
    #[serde(default)]
    pending_input: Option<PendingInputIdentity>,
    planned_executions: BTreeSet<ExecutionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingInputIdentity {
    request_id: InputRequestId,
    run_id: RunId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RunOutcome {
    None,
    Active,
    Suspended,
    Succeeded,
    Failed,
    Cancelled,
    Uncertain,
}

/// A Task event violates identity, revision, or lifecycle invariants.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AggregateError {
    /// Event schema is not the Task event contract.
    #[error("event header does not declare the Task event schema")]
    WrongSchema,
    /// Event schema version is unsupported even when constructed in memory.
    #[error("event schema version must be {expected}, got {actual}")]
    WrongSchemaVersion {
        /// Version accepted by this reducer.
        expected: u16,
        /// Version carried by the rejected event.
        actual: u16,
    },
    /// Event and aggregate Task identities differ.
    #[error("event Task identity does not match the aggregate")]
    TaskMismatch,
    /// Header correlation does not bind the same Task.
    #[error("event correlation does not bind the aggregate Task")]
    CorrelationMismatch,
    /// Event revisions must be consecutive.
    #[error("event revision must be {expected}, got {actual}")]
    RevisionGap {
        /// Next revision required by the aggregate.
        expected: u64,
        /// Revision carried by the rejected event.
        actual: u64,
    },
    /// The aggregate has exhausted the revision number space.
    #[error("Task revision cannot advance beyond u64::MAX")]
    RevisionOverflow,
    /// The first event is not a valid Task creation event.
    #[error("the first Task event must be task_submitted with an actor correlation")]
    InvalidFirstEvent,
    /// The event is illegal for the current lifecycle state.
    #[error("event {event} is invalid while Task state is {state:?}")]
    InvalidTransition {
        /// Stable event discriminator.
        event: String,
        /// State that rejected the event.
        state: TaskState,
    },
    /// An event references a Run other than the active Run.
    #[error("event Run identity does not match the active Run")]
    RunMismatch,
    /// An approval event references an unknown or already-resolved approval.
    #[error("approval is not pending for this Task")]
    ApprovalNotPending,
    /// An input submission does not match the one pending Runtime request.
    #[error("runtime input request is not pending for this Task")]
    InputNotPending,
    /// An execution result references an execution that was not planned.
    #[error("execution is not planned for this Task")]
    ExecutionNotPlanned,
}

impl TaskAggregate {
    /// Rebuilds a Task projection from a non-empty ordered event stream.
    ///
    /// # Errors
    ///
    /// Returns the first identity, revision, or transition violation.
    pub fn replay(events: &[TaskEventEnvelope]) -> Result<Self, AggregateError> {
        let (first, rest) = events
            .split_first()
            .ok_or(AggregateError::InvalidFirstEvent)?;
        validate_task_header(first)?;
        if first.revision != 1 {
            return Err(AggregateError::RevisionGap {
                expected: 1,
                actual: first.revision,
            });
        }
        let TaskEvent::TaskSubmitted { target, .. } = &first.event else {
            return Err(AggregateError::InvalidFirstEvent);
        };
        let owner_actor_id = first
            .header
            .correlation
            .actor_id
            .clone()
            .ok_or(AggregateError::InvalidFirstEvent)?;
        let mut aggregate = Self {
            task_id: first.task_id.clone(),
            owner_actor_id,
            target: target.clone(),
            revision: 1,
            state: TaskState::Submitted,
            active_run_id: None,
            run_outcome: RunOutcome::None,
            cancellation_requested: false,
            pending_approvals: BTreeSet::new(),
            pending_input: None,
            planned_executions: BTreeSet::new(),
        };
        for envelope in rest {
            aggregate.apply(envelope)?;
        }
        Ok(aggregate)
    }

    /// Applies one consecutive event after validating the complete transition.
    ///
    /// # Errors
    ///
    /// Returns an invariant error without modifying the aggregate.
    pub fn apply(&mut self, envelope: &TaskEventEnvelope) -> Result<(), AggregateError> {
        validate_task_header(envelope)?;
        if envelope.task_id != self.task_id {
            return Err(AggregateError::TaskMismatch);
        }
        if envelope.header.correlation.actor_id.as_ref() != Some(&self.owner_actor_id) {
            return Err(AggregateError::CorrelationMismatch);
        }
        let expected = self
            .revision
            .checked_add(1)
            .ok_or(AggregateError::RevisionOverflow)?;
        if envelope.revision != expected {
            return Err(AggregateError::RevisionGap {
                expected,
                actual: envelope.revision,
            });
        }

        let mut next = self.clone();
        next.reduce(&envelope.event)?;
        next.revision = envelope.revision;
        *self = next;
        Ok(())
    }

    /// Returns the durable Task identity.
    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    /// Returns the actor that owns the Task.
    #[must_use]
    pub fn owner_actor_id(&self) -> &ActorId {
        &self.owner_actor_id
    }

    /// Returns the target reference selected when the Task was admitted.
    #[must_use]
    pub fn target(&self) -> &TargetRef {
        &self.target
    }

    /// Returns the latest committed Task revision.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the current durable lifecycle state.
    #[must_use]
    pub fn state(&self) -> TaskState {
        self.state
    }

    /// Returns whether durable cancellation won admission for the active Run.
    #[must_use]
    pub fn cancellation_requested(&self) -> bool {
        self.cancellation_requested
    }

    /// Returns the current Run identity, when one has been allocated.
    #[must_use]
    pub fn active_run_id(&self) -> Option<&RunId> {
        self.active_run_id.as_ref()
    }

    pub(crate) fn active_run_is_running(&self, run_id: &RunId) -> bool {
        self.active_run_id.as_ref() == Some(run_id)
            && self.state == TaskState::Running
            && self.run_outcome == RunOutcome::Active
    }

    pub(crate) fn active_run_can_be_cancelled(&self, run_id: &RunId) -> bool {
        self.active_run_id.as_ref() == Some(run_id)
            && self.planned_executions.is_empty()
            && matches!(
                self.run_outcome,
                RunOutcome::None | RunOutcome::Active | RunOutcome::Suspended | RunOutcome::Failed
            )
    }

    /// Returns the one input request currently blocking the active Run.
    #[must_use]
    pub fn pending_input_request_id(&self) -> Option<&InputRequestId> {
        self.pending_input
            .as_ref()
            .map(|pending| &pending.request_id)
    }

    fn reduce(&mut self, event: &TaskEvent) -> Result<(), AggregateError> {
        match event {
            TaskEvent::TaskSubmitted { .. } => return self.invalid(event),
            TaskEvent::TaskQueued { run_id, .. } => {
                self.require_state(event, &[TaskState::Submitted])?;
                self.state = TaskState::Queued;
                self.active_run_id = Some(run_id.clone());
                self.run_outcome = RunOutcome::None;
            }
            TaskEvent::RunStarted { run_id } => {
                self.require_state(event, &[TaskState::Queued])?;
                self.require_run(run_id)?;
                self.state = TaskState::Running;
                self.run_outcome = RunOutcome::Active;
            }
            TaskEvent::RuntimeBound { run_id, binding } => {
                self.require_running_event(event, run_id)?;
                if binding.task_id != self.task_id || binding.run_id != *run_id {
                    return Err(AggregateError::CorrelationMismatch);
                }
            }
            TaskEvent::RuntimeEventRecorded { run_id, .. } => {
                self.require_running_event(event, run_id)?;
            }
            TaskEvent::InputRequested { request } => {
                self.require_running_event(event, request.run_id())?;
                if self.cancellation_requested
                    || !self.planned_executions.is_empty()
                    || self.pending_input.is_some()
                {
                    return self.invalid(event);
                }
                self.pending_input = Some(PendingInputIdentity {
                    request_id: request.request_id().clone(),
                    run_id: request.run_id().clone(),
                });
                self.state = TaskState::WaitingInput;
                self.run_outcome = RunOutcome::Suspended;
            }
            TaskEvent::InputSubmitted {
                request_id, run_id, ..
            } => {
                self.require_state(event, &[TaskState::WaitingInput])?;
                self.require_run(run_id)?;
                let Some(pending) = &self.pending_input else {
                    return Err(AggregateError::InputNotPending);
                };
                if &pending.request_id != request_id
                    || &pending.run_id != run_id
                    || self.cancellation_requested
                    || !self.planned_executions.is_empty()
                {
                    return Err(AggregateError::InputNotPending);
                }
                self.pending_input = None;
                self.state = TaskState::Running;
                self.run_outcome = RunOutcome::Active;
            }
            TaskEvent::ApprovalRequested { approval } => {
                self.require_running_event(event, &approval.run_id)?;
                if approval.task_id != self.task_id
                    || !self.pending_approvals.insert(approval.approval_id.clone())
                {
                    return Err(AggregateError::ApprovalNotPending);
                }
                self.state = TaskState::WaitingApproval;
                self.run_outcome = RunOutcome::Suspended;
            }
            TaskEvent::ApprovalResolved {
                approval_id,
                decision,
            } => {
                self.require_state(event, &[TaskState::WaitingApproval])?;
                if !self.pending_approvals.remove(approval_id) {
                    return Err(AggregateError::ApprovalNotPending);
                }
                if self.pending_approvals.is_empty() {
                    // Denying one provider tool does not terminate its prompt
                    // turn. The Runtime remains active and may continue with a
                    // safer plan; suspension requires an explicit Run event.
                    let _ = decision;
                    self.state = TaskState::Running;
                    self.run_outcome = RunOutcome::Active;
                }
            }
            TaskEvent::ApprovalAbandoned { approval_id, .. } => {
                self.require_state(event, &[TaskState::WaitingApproval])?;
                if !self.pending_approvals.remove(approval_id) {
                    return Err(AggregateError::ApprovalNotPending);
                }
                if self.pending_approvals.is_empty() {
                    self.state = TaskState::Running;
                    self.run_outcome = RunOutcome::Active;
                }
            }
            TaskEvent::ExecutionPlanned { execution_id, .. } => {
                self.require_active(event)?;
                if !self.planned_executions.insert(execution_id.clone()) {
                    return Err(AggregateError::ExecutionNotPlanned);
                }
            }
            TaskEvent::ExecutionResultRecorded { execution_id, .. } => {
                self.require_active(event)?;
                if !self.planned_executions.remove(execution_id) {
                    return Err(AggregateError::ExecutionNotPlanned);
                }
            }
            TaskEvent::ExecutionUncertain { execution_id, .. } => {
                self.require_active(event)?;
                if !self.planned_executions.remove(execution_id) {
                    return Err(AggregateError::ExecutionNotPlanned);
                }
                self.state = TaskState::Suspended;
                self.run_outcome = RunOutcome::Uncertain;
            }
            TaskEvent::CancellationRequested { run_id, .. } => {
                self.require_state(
                    event,
                    &[
                        TaskState::Queued,
                        TaskState::Running,
                        TaskState::WaitingApproval,
                        TaskState::WaitingInput,
                        TaskState::Suspended,
                    ],
                )?;
                self.require_run(run_id)?;
                if self.state == TaskState::Running && self.run_outcome != RunOutcome::Active {
                    return self.invalid(event);
                }
                if self.cancellation_requested {
                    return self.invalid(event);
                }
                self.cancellation_requested = true;
            }
            TaskEvent::RunCancelled { run_id, .. } => {
                self.require_state(
                    event,
                    &[
                        TaskState::Queued,
                        TaskState::Running,
                        TaskState::WaitingApproval,
                        TaskState::WaitingInput,
                        TaskState::Suspended,
                    ],
                )?;
                self.require_run(run_id)?;
                if self.run_outcome == RunOutcome::Succeeded {
                    return self.invalid(event);
                }
                if !self.cancellation_requested
                    || self.run_outcome == RunOutcome::Uncertain
                    || !self.planned_executions.is_empty()
                {
                    return self.invalid(event);
                }
                self.state = TaskState::Suspended;
                self.run_outcome = RunOutcome::Cancelled;
                self.pending_approvals.clear();
                self.pending_input = None;
            }
            TaskEvent::RunSuspended { run_id, .. } => {
                self.require_state(
                    event,
                    &[
                        TaskState::Queued,
                        TaskState::Running,
                        TaskState::WaitingInput,
                    ],
                )?;
                self.require_run(run_id)?;
                if (self.state == TaskState::Queued && self.run_outcome != RunOutcome::None)
                    || (self.state == TaskState::Running && self.run_outcome != RunOutcome::Active)
                    || (self.state == TaskState::WaitingInput
                        && self.run_outcome != RunOutcome::Suspended)
                    || !self.planned_executions.is_empty()
                {
                    return self.invalid(event);
                }
                self.state = TaskState::Suspended;
                self.run_outcome = RunOutcome::Suspended;
                self.pending_input = None;
            }
            TaskEvent::RunSucceeded { run_id } => {
                self.require_running_event(event, run_id)?;
                if !self.planned_executions.is_empty() {
                    return self.invalid(event);
                }
                self.run_outcome = RunOutcome::Succeeded;
            }
            TaskEvent::RunFailed { run_id, .. } => {
                self.require_state(
                    event,
                    &[
                        TaskState::Queued,
                        TaskState::Running,
                        TaskState::WaitingApproval,
                        TaskState::WaitingInput,
                        TaskState::Suspended,
                    ],
                )?;
                self.require_run(run_id)?;
                if (self.state == TaskState::Queued && self.run_outcome != RunOutcome::None)
                    || (self.state == TaskState::Running && self.run_outcome != RunOutcome::Active)
                    || (matches!(
                        self.state,
                        TaskState::WaitingApproval | TaskState::WaitingInput | TaskState::Suspended
                    ) && self.run_outcome != RunOutcome::Suspended)
                    || !self.planned_executions.is_empty()
                {
                    return self.invalid(event);
                }
                self.state = TaskState::Suspended;
                self.run_outcome = RunOutcome::Failed;
                self.pending_approvals.clear();
                self.pending_input = None;
            }
            TaskEvent::RunRetryQueued {
                previous_run_id,
                next_run_id,
            } => {
                self.require_state(event, &[TaskState::Suspended])?;
                self.require_run(previous_run_id)?;
                if !matches!(self.run_outcome, RunOutcome::Suspended | RunOutcome::Failed)
                    || self.cancellation_requested
                    || !self.planned_executions.is_empty()
                {
                    return self.invalid(event);
                }
                self.state = TaskState::Queued;
                self.active_run_id = Some(next_run_id.clone());
                self.run_outcome = RunOutcome::None;
                self.pending_approvals.clear();
                self.pending_input = None;
            }
            TaskEvent::TaskSucceeded => {
                self.require_state(event, &[TaskState::Running])?;
                if self.run_outcome != RunOutcome::Succeeded {
                    return self.invalid(event);
                }
                self.state = TaskState::Succeeded;
            }
            TaskEvent::TaskFailed { .. } => {
                self.require_state(event, &[TaskState::Suspended])?;
                if self.run_outcome != RunOutcome::Failed {
                    return self.invalid(event);
                }
                self.state = TaskState::Failed;
            }
            TaskEvent::TaskCancelled => {
                if self.state == TaskState::Submitted {
                    self.state = TaskState::Cancelled;
                    return Ok(());
                }
                self.require_state(
                    event,
                    &[
                        TaskState::Queued,
                        TaskState::Running,
                        TaskState::WaitingApproval,
                        TaskState::WaitingInput,
                        TaskState::Suspended,
                    ],
                )?;
                if !self.cancellation_requested
                    || (self.active_run_id.is_some()
                        && self.run_outcome != RunOutcome::Cancelled
                        && self.state != TaskState::Queued)
                {
                    return self.invalid(event);
                }
                self.state = TaskState::Cancelled;
                self.pending_input = None;
            }
        }
        Ok(())
    }

    fn require_running_event(
        &self,
        event: &TaskEvent,
        run_id: &RunId,
    ) -> Result<(), AggregateError> {
        self.require_active(event)?;
        self.require_run(run_id)
    }

    fn require_active(&self, event: &TaskEvent) -> Result<(), AggregateError> {
        self.require_state(event, &[TaskState::Running])?;
        if self.run_outcome == RunOutcome::Active {
            Ok(())
        } else {
            self.invalid(event)
        }
    }

    fn require_run(&self, run_id: &RunId) -> Result<(), AggregateError> {
        if self.active_run_id.as_ref() == Some(run_id) {
            Ok(())
        } else {
            Err(AggregateError::RunMismatch)
        }
    }

    fn require_state(
        &self,
        event: &TaskEvent,
        allowed: &[TaskState],
    ) -> Result<(), AggregateError> {
        if allowed.contains(&self.state) {
            Ok(())
        } else {
            self.invalid(event)
        }
    }

    fn invalid<T>(&self, event: &TaskEvent) -> Result<T, AggregateError> {
        Err(AggregateError::InvalidTransition {
            event: task_event_kind_name(event),
            state: self.state,
        })
    }
}

fn validate_task_header(envelope: &TaskEventEnvelope) -> Result<(), AggregateError> {
    if envelope.header.schema != ContractSchema::TaskEvent {
        return Err(AggregateError::WrongSchema);
    }
    if envelope.header.schema_version != TASK_EVENT_SCHEMA_VERSION {
        return Err(AggregateError::WrongSchemaVersion {
            expected: TASK_EVENT_SCHEMA_VERSION,
            actual: envelope.header.schema_version,
        });
    }
    if envelope.header.correlation.task_id.as_ref() != Some(&envelope.task_id) {
        return Err(AggregateError::CorrelationMismatch);
    }
    Ok(())
}

fn task_event_kind_name(event: &TaskEvent) -> String {
    serde_json::to_value(event.kind())
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod transition_matrix_tests;
