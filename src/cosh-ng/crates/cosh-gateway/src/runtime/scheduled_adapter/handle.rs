impl ScheduledAgentRuntimeHandle {
    fn poll_event(&mut self) -> RuntimePoll {
        let event = match self.port.next_event(deadline(EVENT_POLL_TIMEOUT)) {
            Ok(event) => event,
            Err(AgentRuntimePortError::Deadline { .. }) => return RuntimePoll::Pending,
            Err(error) => return self.fail(map_port_error(error)),
        };
        let expected_sequence = match self.last_sequence.checked_add(1) {
            Some(sequence) => sequence,
            None => {
                return self.fail(contract_error(
                    "runtime_event_sequence_exhausted",
                    ErrorCategory::Internal,
                    false,
                    "The Runtime event sequence exceeded its supported range",
                ));
            }
        };
        if let Err(error) = validate_event_ids(
            &self.actor_id,
            &self.task_id,
            &self.run_id,
            &self.binding_id,
            expected_sequence,
            &event,
        ) {
            return self.fail(error);
        }
        self.last_sequence = expected_sequence;

        match event.event {
            AgentRuntimeEvent::TurnStarted { turn_id }
                if turn_id == self.turn_id && !self.turn_started =>
            {
                self.turn_started = true;
                RuntimePoll::Observed {
                    sequence: self.last_sequence,
                }
            }
            AgentRuntimeEvent::MessageChunk {
                content: ContentPart::Text { text },
                ..
            } if self.turn_started => RuntimePoll::Update {
                sequence: self.last_sequence,
                update: cosh_gateway_contracts::task::RuntimeUpdate::Progress { summary: text },
            },
            AgentRuntimeEvent::MessageChunk { .. }
            | AgentRuntimeEvent::ToolCallObserved { .. }
            | AgentRuntimeEvent::ToolInvocationUpdated { .. }
            | AgentRuntimeEvent::UsageUpdated { .. }
                if self.turn_started =>
            {
                RuntimePoll::Observed {
                    sequence: self.last_sequence,
                }
            }
            AgentRuntimeEvent::PermissionRequested { .. } => self.fail(contract_error(
                "runtime_permission_authority_missing",
                ErrorCategory::Internal,
                false,
                "The Runtime permission event omitted its execution authority",
            )),
            AgentRuntimeEvent::InputRequested { request }
                if self.turn_started
                    && request.run_id() == &self.run_id
                    && request.turn_id() == &self.turn_id
                    && self.pending_permission.is_none()
                    && self.pending_brokered.is_none()
                    && self.pending_input.is_none() =>
            {
                self.pending_input = Some(request.request_id().clone());
                RuntimePoll::InputRequested {
                    sequence: self.last_sequence,
                    request,
                }
            }
            AgentRuntimeEvent::ExecutionPermissionRequested {
                turn_id,
                tool_use_id,
                request,
                summary,
                callback,
            } if self.turn_started
                && turn_id == self.turn_id
                && request.task_id == self.task_id
                && request.run_id == self.run_id
                && request.actor.actor_id == self.actor_id
                && request.target == self.target
                && callback.normalized_operation_digest == request.operation_digest
                && self.pending_permission.is_none()
                && self.pending_brokered.is_none()
                && self.pending_input.is_none() =>
            {
                let permission = RuntimePermissionRef {
                    binding_id: self.binding_id.clone(),
                    runtime_generation: self.binding.runtime_generation,
                    event_sequence: self.last_sequence,
                    run_id: self.run_id.clone(),
                    turn_id,
                    tool_use_id,
                    request_id: request.request_id.clone(),
                    callback: Some(callback),
                    core_callback: None,
                };
                self.pending_permission = Some(permission.clone());
                RuntimePoll::PermissionRequested {
                    permission,
                    request: Box::new(request),
                    summary,
                }
            }
            AgentRuntimeEvent::CoreExecutionPermissionRequested {
                turn_id,
                tool_use_id,
                request,
                summary,
                callback,
            } if self.turn_started
                && turn_id == self.turn_id
                && request.task_id == self.task_id
                && request.run_id == self.run_id
                && request.actor.actor_id == self.actor_id
                && request.target == self.target
                && callback.normalized_operation_digest == request.operation_digest
                && self.pending_permission.is_none()
                && self.pending_brokered.is_none()
                && self.pending_input.is_none() =>
            {
                let permission = RuntimePermissionRef {
                    binding_id: self.binding_id.clone(),
                    runtime_generation: self.binding.runtime_generation,
                    event_sequence: self.last_sequence,
                    run_id: self.run_id.clone(),
                    turn_id,
                    tool_use_id: Some(tool_use_id),
                    request_id: request.request_id.clone(),
                    callback: None,
                    core_callback: Some(callback),
                };
                self.pending_permission = Some(permission.clone());
                RuntimePoll::PermissionRequested {
                    permission,
                    request: Box::new(request),
                    summary,
                }
            }
            AgentRuntimeEvent::ExecutionPermissionsAbandoned {
                turn_id,
                request_ids,
            } if self.turn_started
                && turn_id == self.turn_id
                && request_ids.len() == 1
                && self.pending_permission.as_ref().is_some_and(|permission| {
                    request_ids.first() == Some(&permission.request_id)
                }) =>
            {
                let permission = self
                    .pending_permission
                    .take()
                    .unwrap_or_else(|| unreachable!("guard requires a pending permission"));
                self.expected_cancellation =
                    Some(RuntimeCancellationCause::ProviderPermissionAbandoned {
                        permission: permission.clone(),
                    });
                RuntimePoll::PermissionAbandoned {
                    sequence: self.last_sequence,
                    permission,
                }
            }
            AgentRuntimeEvent::BrokeredExecutionRequested {
                turn_id,
                tool_use_id,
                request,
                operation,
                summary,
            } if self.turn_started
                && turn_id == self.turn_id
                && request.task_id == self.task_id
                && request.run_id == self.run_id
                && request.actor.actor_id == self.actor_id
                && request.target == self.target
                && self.pending_permission.is_none()
                && self.pending_brokered.is_none() =>
            {
                let brokered = BrokeredExecutionRef {
                    binding_id: self.binding_id.clone(),
                    runtime_generation: self.binding.runtime_generation,
                    event_sequence: self.last_sequence,
                    run_id: self.run_id.clone(),
                    turn_id,
                    tool_use_id,
                    request_id: request.request_id.clone(),
                    operation: operation.clone(),
                };
                self.pending_brokered = Some(PendingBrokeredCallback {
                    reference: brokered.clone(),
                    acknowledged: false,
                    dispatch_indeterminate: false,
                });
                RuntimePoll::BrokeredExecutionRequested {
                    brokered,
                    request: Box::new(request),
                    operation,
                    summary,
                }
            }
            AgentRuntimeEvent::Completed { turn_id, outcome }
                if self.turn_started && turn_id == self.turn_id =>
            {
                self.settle_turn(outcome)
            }
            AgentRuntimeEvent::TransportFailed { error } => {
                self.terminal = true;
                RuntimePoll::Failed(error)
            }
            _ => self.fail(contract_error(
                "runtime_event_order_invalid",
                ErrorCategory::Internal,
                false,
                "The Runtime emitted an event outside its active lifecycle",
            )),
        }
    }

    fn settle_turn(&mut self, outcome: TurnOutcome) -> RuntimePoll {
        let expected_cancellation = self.expected_cancellation.take();
        let poll = match (outcome, expected_cancellation) {
            (
                TurnOutcome::Cancelled,
                Some(
                    cause @ (RuntimeCancellationCause::ProviderPermissionDenied { .. }
                    | RuntimeCancellationCause::ProviderPermissionAbandoned { .. }),
                ),
            ) => RuntimePoll::Cancelled { cause },
            (outcome, Some(RuntimeCancellationCause::ProviderPermissionAbandoned { .. }))
                if !matches!(outcome, TurnOutcome::Cancelled) =>
            {
                RuntimePoll::Failed(contract_error(
                    "runtime_permission_abandonment_terminal_invalid",
                    ErrorCategory::Internal,
                    false,
                    "The Runtime abandoned a permission callback without cancelling the turn",
                ))
            }
            (TurnOutcome::Completed, _) => RuntimePoll::Succeeded,
            (TurnOutcome::LimitReached { .. }, _) => RuntimePoll::Failed(contract_error(
                "runtime_turn_limit_reached",
                ErrorCategory::RuntimeUnavailable,
                false,
                "The Agent turn stopped after reaching a configured limit",
            )),
            (TurnOutcome::Refused, _) => RuntimePoll::Failed(contract_error(
                "runtime_turn_refused",
                ErrorCategory::RuntimeUnavailable,
                false,
                "The Agent refused the scheduled task",
            )),
            (TurnOutcome::Cancelled, None) => RuntimePoll::Failed(contract_error(
                "runtime_turn_cancelled_unsolicited",
                ErrorCategory::Cancelled,
                false,
                "The Agent cancelled the turn without a durable cancellation request",
            )),
            (TurnOutcome::Failed { error }, _) => RuntimePoll::Failed(error),
        };
        match poll {
            RuntimePoll::Succeeded => match self.close() {
                Ok(()) => poll,
                Err(error) => RuntimePoll::Failed(error),
            },
            RuntimePoll::Failed(_) | RuntimePoll::Cancelled { .. } => {
                // Preserve the known turn result; close diagnostics are
                // separately governed and cannot make that result less known.
                let _ = self.close();
                poll
            }
            RuntimePoll::Pending
            | RuntimePoll::Observed { .. }
            | RuntimePoll::Update { .. }
            | RuntimePoll::PermissionRequested { .. }
            | RuntimePoll::PermissionAbandoned { .. }
            | RuntimePoll::BrokeredExecutionRequested { .. }
            | RuntimePoll::InputRequested { .. } => {
                unreachable!("turn settlement must be terminal")
            }
        }
    }

    fn close(&mut self) -> Result<(), ContractError> {
        if self.terminal {
            return Ok(());
        }
        self.port
            .dispatch(
                AgentRuntimeCommand::Close {
                    binding: self.binding.clone(),
                },
                deadline(self.command_timeout),
            )
            .map_err(map_port_error)?;
        self.terminal = true;
        Ok(())
    }

    fn fail(&mut self, error: ContractError) -> RuntimePoll {
        let _ = self.close();
        RuntimePoll::Failed(error)
    }
}
