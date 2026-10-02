fn internal_ledger_command(
    actor_id: &ActorId,
    operation: &str,
    claim: &OutboxClaim,
    committed_at_ms: u64,
    digest_value: &impl Serialize,
) -> Result<LedgerCommand, GatewayDaemonError> {
    Ok(LedgerCommand {
        actor_id: actor_id.clone(),
        idempotency_key: internal_key(operation, claim),
        command_digest: digest_json(digest_value)?,
        committed_at_ms,
    })
}

fn provider_dispatch_command(
    actor_id: &ActorId,
    operation: &str,
    approval_id: &ApprovalId,
    expected_revision: u64,
    committed_at_ms: u64,
) -> Result<LedgerCommand, GatewayDaemonError> {
    Ok(LedgerCommand {
        actor_id: actor_id.clone(),
        idempotency_key: IdempotencyKey::new(format!(
            "scheduler-provider-{operation}-{}-{expected_revision}",
            approval_id.as_str()
        ))
        .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
        command_digest: digest_json(&(operation, approval_id, expected_revision))?,
        committed_at_ms,
    })
}

fn runtime_input_command(
    actor_id: &ActorId,
    operation: &str,
    request_id: &InputRequestId,
    expected_revision: u64,
    committed_at_ms: u64,
) -> Result<LedgerCommand, GatewayDaemonError> {
    Ok(LedgerCommand {
        actor_id: actor_id.clone(),
        idempotency_key: IdempotencyKey::new(format!(
            "scheduler-input-{operation}-{}-{expected_revision}",
            request_id.as_str()
        ))
        .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
        command_digest: digest_json(&(operation, request_id, expected_revision))?,
        committed_at_ms,
    })
}

fn approval_resolution_revision(
    approval: &crate::storage::ApprovalRecord,
    decision: ApprovalDecision,
) -> Result<u64, GatewayDaemonError> {
    match (approval.state, decision) {
        (ApprovalState::Pending, _) => Ok(approval.revision),
        (ApprovalState::Approved, ApprovalDecision::Approve)
        | (ApprovalState::Denied, ApprovalDecision::Deny) => {
            approval.revision.checked_sub(1).ok_or_else(|| {
                GatewayDaemonError::Protocol(
                    "resolved approval has an invalid durable revision".to_owned(),
                )
            })
        }
        (ApprovalState::Approved, ApprovalDecision::Deny)
        | (ApprovalState::Denied, ApprovalDecision::Approve) => Err(GatewayDaemonError::Protocol(
            "approval was already resolved with a different decision".to_owned(),
        )),
        (ApprovalState::Expired | ApprovalState::Cancelled, _) => Err(
            GatewayDaemonError::Protocol("approval is no longer resolvable".to_owned()),
        ),
    }
}

fn provider_dispatch_decision(decision: ApprovalDecision) -> ProviderPermissionDispatchDecision {
    match decision {
        ApprovalDecision::Approve => ProviderPermissionDispatchDecision::AllowOnce,
        ApprovalDecision::Deny => ProviderPermissionDispatchDecision::Deny,
    }
}

fn internal_key(operation: &str, claim: &OutboxClaim) -> IdempotencyKey {
    IdempotencyKey::new(format!(
        "scheduler-{operation}-{}-{}",
        claim.delivery_id.as_str(),
        claim.attempt
    ))
    .unwrap_or_else(|_| unreachable!())
}

fn deadline(now_ms: u64, duration_ms: u64) -> Result<u64, GatewayDaemonError> {
    now_ms
        .checked_add(duration_ms)
        .ok_or_else(|| GatewayDaemonError::Protocol("scheduler lease deadline overflow".to_owned()))
}

fn duration_ms(duration: Duration, label: &str) -> Result<u64, GatewayDaemonError> {
    u64::try_from(duration.as_millis())
        .map_err(|_| GatewayDaemonError::Protocol(format!("{label} exceeds the supported range")))
}

fn refreshed_now_ms(previous_ms: u64) -> Result<u64, GatewayDaemonError> {
    Ok(super::now_ms()?.max(previous_ms))
}

fn renew_for_operation(
    coordinator: &mut TaskCoordinator,
    actor_id: &ActorId,
    claim: &mut LeaseClaim,
    lease_expires_at_ms: &mut u64,
    config: ValidatedSchedulerConfig,
    now_ms: u64,
) -> Result<(), GatewayDaemonError> {
    if now_ms >= *lease_expires_at_ms {
        return Err(stale_operation_error());
    }
    let required_remaining = config
        .runtime_operation_timeout_ms
        .checked_add(config.lease_renewal_margin_ms)
        .ok_or_else(|| {
            GatewayDaemonError::Protocol("scheduler Runtime operation budget overflow".to_owned())
        })?;
    if lease_expires_at_ms.saturating_sub(now_ms) <= required_remaining {
        let renewed_until = deadline(now_ms, config.lease_duration_ms)?;
        *claim = coordinator.renew_lease(actor_id, claim, renewed_until, now_ms)?;
        *lease_expires_at_ms = renewed_until;
    }
    Ok(())
}

fn stale_operation_error() -> GatewayDaemonError {
    StoreError::GenerationFenced {
        expected: 0,
        actual: 0,
    }
    .into()
}

fn no_active_run() -> GatewayDaemonError {
    GatewayDaemonError::Protocol("scheduler has no active Runtime".to_owned())
}

fn runtime_lost_error(code: &str, message: &str) -> Result<ContractError, GatewayDaemonError> {
    ContractError::new(code, ErrorCategory::RuntimeUnavailable, false, message)
        .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))
}
