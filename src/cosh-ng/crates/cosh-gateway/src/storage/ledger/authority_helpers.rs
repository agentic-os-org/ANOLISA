fn require_task_owner(
    transaction: &rusqlite::Connection,
    task_id: &TaskId,
    actor_id: &ActorId,
) -> Result<(), StoreError> {
    let task = load_authoritative_task(transaction, task_id)?;
    if task.owner_actor_id() == actor_id {
        Ok(())
    } else {
        Err(conflict("actor does not own the bound Task"))
    }
}

fn require_task_run(
    transaction: &rusqlite::Connection,
    task_id: &TaskId,
    run_id: &RunId,
    actor_id: &ActorId,
) -> Result<(), StoreError> {
    let task = load_authoritative_task(transaction, task_id)?;
    if task.owner_actor_id() != actor_id {
        return Err(conflict("actor does not own the bound Task"));
    }
    if task.active_run_id() != Some(run_id) {
        return Err(conflict(
            "Run is not the authoritative active Run for the bound Task",
        ));
    }
    Ok(())
}

fn load_authoritative_task(
    transaction: &rusqlite::Connection,
    task_id: &TaskId,
) -> Result<TaskAggregate, StoreError> {
    let snapshot_json = transaction
        .query_row(
            "SELECT snapshot_json FROM tasks WHERE task_id=?1",
            params![task_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::TaskNotFound)?;
    let snapshot = serde_json::from_str::<TaskAggregate>(&snapshot_json)
        .map_err(|error| corrupt(&format!("Task snapshot cannot be decoded: {error}")))?;
    let mut statement = transaction
        .prepare("SELECT payload_json FROM task_events WHERE task_id=?1 ORDER BY revision ASC")?;
    let payloads = statement
        .query_map(params![task_id.as_str()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if payloads.is_empty() {
        return Err(corrupt("Task projection has no event stream"));
    }
    let events = payloads
        .into_iter()
        .map(|payload| {
            serde_json::from_str(&payload)
                .map_err(|error| corrupt(&format!("Task event cannot be decoded: {error}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let recovered = TaskAggregate::replay(&events)?;
    if recovered != snapshot || recovered.task_id() != task_id {
        return Err(corrupt(
            "Task projection diverges from its authoritative event stream",
        ));
    }
    Ok(recovered)
}

fn require_current_lease(
    transaction: &rusqlite::Connection,
    claim: &LeaseClaim,
    actor_id: &ActorId,
    now_ms: u64,
) -> Result<(), StoreError> {
    integer(claim.generation, "lease generation")?;
    integer(claim.revision, "lease revision")?;
    let current = load_run_lease_optional(transaction, &claim.run_id)?
        .ok_or_else(|| not_found("run lease", claim.run_id.as_str()))?;
    require_task_run(transaction, &claim.task_id, &claim.run_id, actor_id)?;
    if current.task_id != claim.task_id
        || current.actor_id != *actor_id
        || current.lease_owner != claim.lease_owner
        || current.generation != claim.generation
        || current.revision != claim.revision
        || current.expires_at_ms <= now_ms
    {
        return Err(conflict("run lease fencing claim is stale or expired"));
    }
    Ok(())
}

fn require_runtime_fence(
    transaction: &rusqlite::Connection,
    actor_id: &ActorId,
    task_id: &TaskId,
    run_id: &RunId,
    fence: &RuntimeExecutionFence,
    now_ms: u64,
) -> Result<(), StoreError> {
    integer(fence.runtime_generation, "brokered Runtime generation")?;
    integer(fence.lease_generation, "brokered lease generation")?;
    integer(fence.lease_revision, "brokered lease revision")?;
    let binding = load_runtime_binding(transaction, &fence.binding_id)?;
    let lease = load_run_lease_optional(transaction, run_id)?
        .ok_or_else(|| not_found("run lease", run_id.as_str()))?;
    if binding.actor_id != *actor_id
        || binding.state != RuntimeBindingState::Active
        || binding.binding.task_id != *task_id
        || binding.binding.run_id != *run_id
        || binding.binding.runtime_generation != fence.runtime_generation
        || lease.actor_id != *actor_id
        || lease.task_id != *task_id
        || lease.run_id != *run_id
        || lease.generation != fence.lease_generation
        || lease.expires_at_ms <= now_ms
    {
        return Err(conflict(
            "brokered Runtime binding or Run lease fence is stale",
        ));
    }
    Ok(())
}

fn require_execution_runtime_context(
    connection: &rusqlite::Connection,
    command: &LedgerCommand,
    execution: &ExecutionRecord,
    lease: &LeaseClaim,
) -> Result<(), StoreError> {
    let runtime_fence = execution
        .runtime_fence
        .as_ref()
        .ok_or_else(|| conflict("brokered execution is missing a Runtime fence"))?;
    if execution.actor_id != command.actor_id
        || lease.task_id != execution.task_id
        || lease.run_id != execution.run_id
        || lease.generation != runtime_fence.lease_generation
    {
        return Err(conflict(
            "execution actor, lease, or Runtime generation differs",
        ));
    }
    require_current_lease(
        connection,
        lease,
        &command.actor_id,
        command.committed_at_ms,
    )?;
    require_runtime_fence(
        connection,
        &execution.actor_id,
        &execution.task_id,
        &execution.run_id,
        runtime_fence,
        command.committed_at_ms,
    )
}

fn validate_initial_approval(
    command: &LedgerCommand,
    approval: &ApprovalRecord,
) -> Result<(), StoreError> {
    if approval.actor_id != command.actor_id
        || approval.state != ApprovalState::Pending
        || approval.revision != 1
        || approval.decided_by_actor_id.is_some()
        || approval.created_at_ms != command.committed_at_ms
        || approval.updated_at_ms != command.committed_at_ms
        || approval.expires_at_ms <= command.committed_at_ms
    {
        return Err(conflict("invalid initial approval bindings or lifecycle"));
    }
    Ok(())
}

fn insert_approval(
    transaction: &Transaction<'_>,
    approval: &ApprovalRecord,
    committed_at_ms: u64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO approvals(approval_id, request_id, actor_id, task_id, run_id,
         target_json, operation_digest, input_digest, state, revision, expires_at_ms,
         created_at_ms, updated_at_ms, permission_ref_json, target_identity_digest,
         runtime_fence_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', 1, ?9, ?10, ?10, ?11,
                 ?12, ?13)",
        params![
            approval.approval_id.as_str(),
            approval.request_id.as_str(),
            approval.actor_id.as_str(),
            approval.task_id.as_str(),
            approval.run_id.as_str(),
            serde_json::to_string(&approval.target)?,
            approval.operation_digest.as_str(),
            approval.input_digest.as_str(),
            integer(approval.expires_at_ms, "approval deadline")?,
            integer(committed_at_ms, "approval timestamp")?,
            approval
                .permission
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            approval.target_identity_digest.as_ref().map(Digest::as_str),
            approval
                .runtime_fence
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        ],
    )?;
    Ok(())
}

fn validate_permission_binding(
    approval: &ApprovalRecord,
    permission: &RuntimePermissionRef,
) -> Result<(), StoreError> {
    if permission.runtime_generation == 0
        || permission.event_sequence == 0
        || permission.request_id != approval.request_id
        || permission.run_id != approval.run_id
    {
        return Err(conflict(
            "Runtime permission does not match the approval request and Run",
        ));
    }
    integer(
        permission.runtime_generation,
        "Runtime permission generation",
    )?;
    integer(
        permission.event_sequence,
        "Runtime permission event sequence",
    )?;
    Ok(())
}

fn require_provider_permission_context(
    transaction: &Transaction<'_>,
    command: &LedgerCommand,
    approval: &ApprovalRecord,
    expected_permission: &RuntimePermissionRef,
    lease: &LeaseClaim,
    expected_task_state: TaskState,
) -> Result<(), StoreError> {
    validate_permission_binding(approval, expected_permission)?;
    let callback_operation_digest = match (
        &expected_permission.callback,
        &expected_permission.core_callback,
    ) {
        (Some(callback), None) => &callback.normalized_operation_digest,
        (None, Some(callback)) => &callback.normalized_operation_digest,
        _ => {
            return Err(conflict(
                "live Runtime Permission must have exactly one callback",
            ))
        }
    };
    if approval.actor_id != command.actor_id
        || approval.permission.as_ref() != Some(expected_permission)
        || *callback_operation_digest != approval.operation_digest
        || lease.task_id != approval.task_id
        || lease.run_id != approval.run_id
        || lease.generation != expected_permission.runtime_generation
    {
        return Err(conflict(
            "provider permission actor, lease, authority, or callback binding does not match",
        ));
    }
    require_current_lease(
        transaction,
        lease,
        &command.actor_id,
        command.committed_at_ms,
    )?;
    let task = load_authoritative_task(transaction, &approval.task_id)?;
    if task.state() != expected_task_state || task.cancellation_requested() {
        return Err(conflict(
            "provider permission Task is not active in the required approval state",
        ));
    }
    let binding = load_runtime_binding(transaction, &expected_permission.binding_id)?;
    if binding.actor_id != command.actor_id
        || binding.state != RuntimeBindingState::Active
        || binding.binding.task_id != approval.task_id
        || binding.binding.run_id != approval.run_id
        || binding.binding.runtime_generation != expected_permission.runtime_generation
        || binding.last_sequence < expected_permission.event_sequence
    {
        return Err(conflict(
            "provider permission Runtime binding is stale or does not match",
        ));
    }
    Ok(())
}

fn update_provider_permission_dispatch(
    transaction: &Transaction<'_>,
    dispatch: &ProviderPermissionDispatchRecord,
    expected_revision: u64,
    expected_state: &str,
) -> Result<(), StoreError> {
    let changed = transaction.execute(
        "UPDATE provider_permission_dispatches
         SET state=?2, revision=?3, updated_at_ms=?4
         WHERE approval_id=?1 AND state=?5 AND revision=?6",
        params![
            dispatch.approval_id.as_str(),
            state_name(dispatch.state)?,
            integer(dispatch.revision, "dispatch revision")?,
            integer(dispatch.updated_at_ms, "dispatch timestamp")?,
            expected_state,
            integer(expected_revision, "dispatch expected revision")?,
        ],
    )?;
    if changed != 1 {
        return Err(conflict(
            "provider permission dispatch lost its state or revision precondition",
        ));
    }
    Ok(())
}
