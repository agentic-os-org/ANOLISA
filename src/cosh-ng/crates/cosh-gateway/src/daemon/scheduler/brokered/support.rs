fn brokered_dispatch_command(
    actor_id: &ActorId,
    operation: &str,
    kind: BrokeredRuntimeDispatchKind,
    brokered: &BrokeredExecutionRef,
    expected_revision: u64,
    committed_at_ms: u64,
) -> Result<LedgerCommand, GatewayDaemonError> {
    let kind_label = match kind {
        BrokeredRuntimeDispatchKind::Acknowledgement => "ack",
        BrokeredRuntimeDispatchKind::Result => "result",
    };
    Ok(LedgerCommand {
        actor_id: actor_id.clone(),
        idempotency_key: IdempotencyKey::new(format!(
            "scheduler-brokered-{operation}-{kind_label}-{}-{expected_revision}",
            brokered.request_id.as_str()
        ))
        .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
        command_digest: digest_json(&(operation, kind, brokered, expected_revision))?,
        committed_at_ms,
    })
}

fn validate_resolution(
    approval: &ApprovalRecord,
    decision: ApprovalDecision,
    resolution: &BrokeredResolution,
) -> Result<(), GatewayDaemonError> {
    use cosh_gateway_contracts::runtime::BrokeredExecutionOutcome;

    if resolution.delivery.request_id != approval.request_id {
        return Err(GatewayDaemonError::Protocol(
            "brokered result request identity does not match its approval".to_owned(),
        ));
    }
    let valid = match (&resolution.source, &resolution.delivery.outcome, decision) {
        (
            BrokeredResolutionSource::ApprovalDenied { approval_id },
            BrokeredExecutionOutcome::Denied { .. },
            ApprovalDecision::Deny,
        ) => approval_id == &approval.approval_id,
        (
            BrokeredResolutionSource::Execution { execution_id },
            BrokeredExecutionOutcome::Succeeded {
                execution_id: outcome_id,
                ..
            }
            | BrokeredExecutionOutcome::Failed {
                execution_id: outcome_id,
                ..
            }
            | BrokeredExecutionOutcome::Uncertain {
                execution_id: outcome_id,
                ..
            },
            ApprovalDecision::Approve,
        ) => execution_id == outcome_id,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(GatewayDaemonError::Protocol(
            "brokered driver result does not match the actor decision or durable source".to_owned(),
        ))
    }
}

fn validate_replayed_dispatch(
    actor_id: &ActorId,
    approval: &ApprovalRecord,
    request: &BrokeredRequestRecord,
    decision: ApprovalDecision,
    dispatch: &BrokeredRuntimeDispatchRecord,
    store: &SqliteTaskStore,
) -> Result<(), GatewayDaemonError> {
    let brokered = &dispatch.brokered;
    if dispatch.actor_id != *actor_id
        || dispatch.task_id != approval.task_id
        || dispatch.kind != BrokeredRuntimeDispatchKind::Result
        || brokered.request_id != request.request.request_id
        || brokered.run_id != request.request.run_id
        || brokered.operation != request.operation
        || brokered.binding_id != request.runtime_fence.binding_id
        || brokered.runtime_generation != request.runtime_fence.runtime_generation
    {
        return Err(GatewayDaemonError::Unauthorized);
    }
    match (&dispatch.source, decision) {
        (
            crate::storage::BrokeredRuntimeDispatchSource::ApprovalDenied { approval_id },
            ApprovalDecision::Deny,
        ) if approval.state == ApprovalState::Denied && approval_id == &approval.approval_id => {
            Ok(())
        }
        (
            crate::storage::BrokeredRuntimeDispatchSource::Execution { execution_id },
            ApprovalDecision::Approve,
        ) if approval.state == ApprovalState::Approved => {
            let execution = store.load_execution_record(execution_id)?;
            if execution.actor_id != *actor_id
                || execution.task_id != approval.task_id
                || execution.run_id != approval.run_id
                || execution.target_identity_digest.as_ref()
                    != approval.target_identity_digest.as_ref()
                || execution.runtime_fence.as_ref() != approval.runtime_fence.as_ref()
                || !matches!(
                    execution.state,
                    crate::storage::ExecutionState::Succeeded
                        | crate::storage::ExecutionState::Failed
                        | crate::storage::ExecutionState::Uncertain
                )
            {
                return Err(GatewayDaemonError::Unauthorized);
            }
            Ok(())
        }
        _ => Err(GatewayDaemonError::Unauthorized),
    }
}

fn indeterminate_replay_error() -> GatewayDaemonError {
    GatewayDaemonError::Protocol(
        "brokered result delivery is indeterminate and cannot be replayed".to_owned(),
    )
}
