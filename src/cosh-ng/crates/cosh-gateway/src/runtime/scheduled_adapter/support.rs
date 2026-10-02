fn validate_event(
    run: &ScheduledRun,
    binding_id: &RuntimeBindingId,
    expected_sequence: u64,
    event: &RuntimeEventEnvelope,
) -> Result<(), ContractError> {
    validate_event_ids(
        &run.actor.actor_id,
        &run.task_id,
        &run.run_id,
        binding_id,
        expected_sequence,
        event,
    )
}

fn validate_event_ids(
    actor_id: &cosh_gateway_contracts::ids::ActorId,
    task_id: &cosh_gateway_contracts::ids::TaskId,
    run_id: &cosh_gateway_contracts::ids::RunId,
    binding_id: &RuntimeBindingId,
    expected_sequence: u64,
    event: &RuntimeEventEnvelope,
) -> Result<(), ContractError> {
    let correlation = &event.header.correlation;
    if event.header.validate_version().is_err()
        || event
            .header
            .validate_schema(cosh_gateway_contracts::common::ContractSchema::RuntimeEvent)
            .is_err()
        || &event.binding_id != binding_id
        || event.sequence != expected_sequence
        || correlation.actor_id.as_ref() != Some(actor_id)
        || correlation.task_id.as_ref() != Some(task_id)
        || correlation.run_id.as_ref() != Some(run_id)
        || correlation.runtime_binding_id.as_ref() != Some(binding_id)
    {
        return Err(contract_error(
            "runtime_event_identity_invalid",
            ErrorCategory::Internal,
            false,
            "The Runtime emitted an event with invalid identity or ordering",
        ));
    }
    Ok(())
}

fn deadline(timeout: Duration) -> Instant {
    Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now)
}

fn map_port_error(error: AgentRuntimePortError) -> ContractError {
    match error {
        AgentRuntimePortError::Deadline { .. } => contract_error(
            "runtime_deadline_exceeded",
            ErrorCategory::Transport,
            true,
            "The Runtime operation exceeded its deadline",
        ),
        AgentRuntimePortError::Transport => contract_error(
            "runtime_transport_failed",
            ErrorCategory::Transport,
            true,
            "The Runtime transport failed",
        ),
        AgentRuntimePortError::Unsupported { .. } => contract_error(
            "runtime_operation_unsupported",
            ErrorCategory::RuntimeUnavailable,
            false,
            "The Runtime does not support a required operation",
        ),
        AgentRuntimePortError::InvalidState { .. }
        | AgentRuntimePortError::IdentityMismatch
        | AgentRuntimePortError::WorkspaceMismatch
        | AgentRuntimePortError::Protocol => contract_error(
            "runtime_protocol_failed",
            ErrorCategory::Internal,
            false,
            "The Runtime violated its lifecycle contract",
        ),
        AgentRuntimePortError::Terminal => contract_error(
            "runtime_terminal_unexpected",
            ErrorCategory::RuntimeUnavailable,
            false,
            "The Runtime became terminal before completing the task",
        ),
    }
}

fn contract_error(
    code: &'static str,
    category: ErrorCategory,
    retryable: bool,
    message: &'static str,
) -> ContractError {
    ContractError::new(code, category, retryable, message)
        .unwrap_or_else(|_| unreachable!("static Runtime error must remain valid"))
}
