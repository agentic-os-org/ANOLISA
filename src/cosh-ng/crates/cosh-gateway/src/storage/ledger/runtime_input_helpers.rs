
#[allow(clippy::too_many_arguments)]
fn transition_pending_runtime_input_request(
    store: &mut SqliteTaskStore,
    command: &LedgerCommand,
    request_id: &InputRequestId,
    expected_revision: u64,
    next_state: RuntimeInputRequestState,
    require_expired: bool,
    operation: &str,
) -> Result<LedgerOutcome<RuntimeInputRequestRecord>, StoreError> {
    validate_command(command)?;
    integer(expected_revision, "runtime input request expected revision")?;
    let transaction = immediate(store)?;
    if let Some(replayed) = replay::<RuntimeInputRequestRecord>(&transaction, command, operation)? {
        if replayed.request.request_id() != request_id || replayed.state != next_state {
            return Err(StoreError::IdempotencyConflict);
        }
        transaction.commit()?;
        return Ok(LedgerOutcome::Replayed(replayed));
    }
    let mut record = load_runtime_input_request(&transaction, request_id)?;
    if record.actor_id != command.actor_id
        || record.state != RuntimeInputRequestState::Pending
        || record.revision != expected_revision
        || (require_expired && command.committed_at_ms < record.expires_at_ms)
    {
        return Err(conflict(
            "runtime input request state, revision, actor, or deadline is stale",
        ));
    }
    if require_expired {
        append_internal_task_event(
            &transaction,
            &record.task_id,
            &record.actor_id,
            command.committed_at_ms,
            TaskEvent::RunSuspended {
                run_id: record.run_id.clone(),
                reason: cosh_gateway_contracts::task::SuspensionCode::OperatorRequired,
            },
            None,
        )?;
    } else {
        let task = load_verified_projection(&transaction, &record.task_id)?
            .ok_or(StoreError::TaskNotFound)?;
        if !task.cancellation_requested() {
            return Err(conflict(
                "runtime input cancellation requires durable Task cancellation",
            ));
        }
    }
    record.state = next_state;
    record.revision = next_integer(record.revision, "runtime input request revision")?;
    record.updated_at_ms = command.committed_at_ms;
    let changed = transaction.execute(
        "UPDATE runtime_input_requests SET state=?2, revision=?3, updated_at_ms=?4
         WHERE request_id=?1 AND state='pending' AND revision=?5",
        params![
            request_id.as_str(),
            state_name(next_state)?,
            integer(record.revision, "runtime input request revision")?,
            integer(command.committed_at_ms, "runtime input request transition")?,
            integer(expected_revision, "runtime input request expected revision")?,
        ],
    )?;
    if changed != 1 {
        return Err(conflict(
            "runtime input request lost its pending revision precondition",
        ));
    }
    insert_receipt(&transaction, command, operation, &record)?;
    transaction.commit()?;
    Ok(LedgerOutcome::Applied(record))
}

#[allow(clippy::too_many_arguments)]
fn transition_runtime_input_dispatch(
    store: &mut SqliteTaskStore,
    command: &LedgerCommand,
    request_id: &InputRequestId,
    response_digest: &Digest,
    expected_revision: u64,
    lease: &LeaseClaim,
    expected_state: RuntimeInputDispatchState,
    next_state: RuntimeInputDispatchState,
    operation: &str,
) -> Result<LedgerOutcome<RuntimeInputDispatchRecord>, StoreError> {
    validate_command(command)?;
    integer(
        expected_revision,
        "runtime input dispatch expected revision",
    )?;
    let transaction = immediate(store)?;
    if let Some(replayed) = replay_runtime_input_dispatch(
        &transaction,
        command,
        operation,
        request_id,
        response_digest,
    )? {
        transaction.commit()?;
        return Ok(LedgerOutcome::Replayed(replayed));
    }
    let request = load_runtime_input_request(&transaction, request_id)?;
    let mut dispatch = load_runtime_input_dispatch(&transaction, request_id)?;
    require_runtime_input_dispatch_context(
        &transaction,
        command,
        &request,
        &dispatch,
        response_digest,
        lease,
    )?;
    if dispatch.state != expected_state || dispatch.revision != expected_revision {
        return Err(conflict(
            "runtime input dispatch is not at the expected state and revision",
        ));
    }
    if expected_state == RuntimeInputDispatchState::Prepared {
        let task = load_verified_projection(&transaction, &dispatch.task_id)?
            .ok_or_else(|| corrupt("Runtime input dispatch references a missing Task"))?;
        if task.owner_actor_id() != &dispatch.actor_id
            || !task.active_run_is_running(&dispatch.run_id)
            || task.cancellation_requested()
        {
            return Err(conflict(
                "runtime input dispatch cannot start after its Task stopped running",
            ));
        }
    }
    require_not_before(
        command.committed_at_ms,
        dispatch.updated_at_ms,
        "runtime input dispatch",
    )?;
    dispatch.state = next_state;
    dispatch.revision = next_integer(dispatch.revision, "runtime input dispatch revision")?;
    dispatch.updated_at_ms = command.committed_at_ms;
    let changed = transaction.execute(
        "UPDATE runtime_input_dispatches SET state=?2, revision=?3, updated_at_ms=?4
         WHERE request_id=?1 AND state=?5 AND revision=?6",
        params![
            request_id.as_str(),
            state_name(next_state)?,
            integer(dispatch.revision, "runtime input dispatch revision")?,
            integer(command.committed_at_ms, "runtime input dispatch timestamp")?,
            state_name(expected_state)?,
            integer(
                expected_revision,
                "runtime input dispatch expected revision"
            )?,
        ],
    )?;
    if changed != 1 {
        return Err(conflict(
            "runtime input dispatch lost its state or revision precondition",
        ));
    }
    insert_runtime_input_dispatch_receipt(&transaction, command, operation, &dispatch)?;
    transaction.commit()?;
    Ok(LedgerOutcome::Applied(dispatch))
}

#[allow(clippy::too_many_arguments)]
fn mark_runtime_input_dispatch_unknown_atomic(
    store: &mut SqliteTaskStore,
    command: &LedgerCommand,
    request_id: &InputRequestId,
    response_digest: &Digest,
    expected_revision: u64,
    lease: &LeaseClaim,
    operation: &str,
) -> Result<LedgerOutcome<RuntimeInputDispatchRecord>, StoreError> {
    validate_command(command)?;
    integer(
        expected_revision,
        "runtime input dispatch expected revision",
    )?;
    let transaction = immediate(store)?;
    if let Some(replayed) = replay_runtime_input_dispatch(
        &transaction,
        command,
        operation,
        request_id,
        response_digest,
    )? {
        transaction.commit()?;
        return Ok(LedgerOutcome::Replayed(replayed));
    }
    let request = load_runtime_input_request(&transaction, request_id)?;
    let mut dispatch = load_runtime_input_dispatch(&transaction, request_id)?;
    require_runtime_input_dispatch_context(
        &transaction,
        command,
        &request,
        &dispatch,
        response_digest,
        lease,
    )?;
    if dispatch.state != RuntimeInputDispatchState::Started
        || dispatch.revision != expected_revision
    {
        return Err(conflict(
            "runtime input dispatch is not at the started state and revision",
        ));
    }
    require_not_before(
        command.committed_at_ms,
        dispatch.updated_at_ms,
        "runtime input uncertainty",
    )?;
    dispatch.state = RuntimeInputDispatchState::Unknown;
    dispatch.revision = next_integer(dispatch.revision, "runtime input dispatch revision")?;
    dispatch.updated_at_ms = command.committed_at_ms;
    let changed = transaction.execute(
        "UPDATE runtime_input_dispatches SET state='unknown', revision=?2, updated_at_ms=?3
         WHERE request_id=?1 AND state='started' AND revision=?4",
        params![
            request_id.as_str(),
            integer(dispatch.revision, "runtime input dispatch revision")?,
            integer(
                command.committed_at_ms,
                "runtime input uncertainty timestamp"
            )?,
            integer(
                expected_revision,
                "runtime input dispatch expected revision"
            )?,
        ],
    )?;
    if changed != 1 {
        return Err(conflict(
            "runtime input uncertainty lost its started revision",
        ));
    }
    append_internal_task_event(
        &transaction,
        &dispatch.task_id,
        &dispatch.actor_id,
        command.committed_at_ms,
        TaskEvent::RunSuspended {
            run_id: dispatch.run_id.clone(),
            reason: cosh_gateway_contracts::task::SuspensionCode::OperatorRequired,
        },
        None,
    )?;
    insert_runtime_input_dispatch_receipt(&transaction, command, operation, &dispatch)?;
    transaction.commit()?;
    Ok(LedgerOutcome::Applied(dispatch))
}

fn require_runtime_input_dispatch_context(
    transaction: &rusqlite::Connection,
    command: &LedgerCommand,
    request: &RuntimeInputRequestRecord,
    dispatch: &RuntimeInputDispatchRecord,
    response_digest: &Digest,
    lease: &LeaseClaim,
) -> Result<(), StoreError> {
    if request.state != RuntimeInputRequestState::Resolved
        || request.response_digest.as_ref() != Some(response_digest)
        || dispatch.request_id != *request.request.request_id()
        || dispatch.actor_id != command.actor_id
        || dispatch.actor_id != request.actor_id
        || dispatch.task_id != request.task_id
        || dispatch.run_id != request.run_id
        || dispatch.response_digest != *response_digest
        || lease.task_id != request.task_id
        || lease.run_id != request.run_id
        || lease.generation != request.lease_generation
    {
        return Err(conflict("runtime input dispatch binding is stale"));
    }
    if runtime_input_response_digest(&dispatch.response)? != *response_digest {
        return Err(corrupt(
            "runtime input dispatch response diverges from its digest",
        ));
    }
    require_current_lease(
        transaction,
        lease,
        &command.actor_id,
        command.committed_at_ms,
    )?;
    let binding = load_runtime_binding(transaction, &request.binding_id)?;
    if binding.state != RuntimeBindingState::Active
        || binding.actor_id != command.actor_id
        || binding.binding.task_id != request.task_id
        || binding.binding.run_id != request.run_id
        || binding.binding.runtime_instance_id != request.runtime_instance_id
        || binding.binding.runtime_generation != request.runtime_generation
    {
        return Err(conflict("runtime input dispatch Runtime binding is stale"));
    }
    Ok(())
}

fn validate_runtime_input_response(
    request: &RuntimeInputRequest,
    response: &RuntimeInputResponse,
) -> Result<(), StoreError> {
    match response {
        RuntimeInputResponse::Text { .. } if !request.allows_free_text() => {
            Err(conflict("runtime input request does not allow free text"))
        }
        RuntimeInputResponse::Options { selections } => {
            if (!request.allows_multiple() && selections.as_slice().len() != 1)
                || selections
                    .as_slice()
                    .iter()
                    .any(|index| usize::from(*index) >= request.options().len())
            {
                return Err(conflict(
                    "runtime input response selections do not match the request",
                ));
            }
            Ok(())
        }
        RuntimeInputResponse::Text { .. } => Ok(()),
    }
}

fn validate_json_bound<T: Serialize>(
    value: &T,
    maximum: usize,
    field: &str,
) -> Result<(), StoreError> {
    if serde_json::to_vec(value)?.len() > maximum {
        return Err(conflict(&format!(
            "{field} exceeds {maximum} serialized bytes"
        )));
    }
    Ok(())
}

fn runtime_input_response_digest(response: &RuntimeInputResponse) -> Result<Digest, StoreError> {
    let encoded = serde_json::to_vec(response)?;
    let digest = Sha256::digest(&encoded);
    Digest::parse(format!("{digest:x}")).map_err(|error| {
        corrupt(&format!(
            "runtime input response digest is invalid: {error}"
        ))
    })
}

fn insert_runtime_input_request(
    transaction: &rusqlite::Connection,
    record: &RuntimeInputRequestRecord,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO runtime_input_requests(
             request_id, actor_id, task_id, run_id, binding_id, runtime_instance_id,
             runtime_generation, runtime_sequence, lease_generation, lease_revision,
             request_json, state, response_digest, revision, expires_at_ms,
             created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending', NULL,
                 1, ?12, ?13, ?13)",
        params![
            record.request.request_id().as_str(),
            record.actor_id.as_str(),
            record.task_id.as_str(),
            record.run_id.as_str(),
            record.binding_id.as_str(),
            record.runtime_instance_id.as_str(),
            integer(record.runtime_generation, "runtime input generation")?,
            integer(record.runtime_sequence, "runtime input sequence")?,
            integer(record.lease_generation, "runtime input lease generation")?,
            integer(record.lease_revision, "runtime input lease revision")?,
            serde_json::to_string(&record.request)?,
            integer(record.expires_at_ms, "runtime input deadline")?,
            integer(record.created_at_ms, "runtime input creation timestamp")?,
        ],
    )?;
    Ok(())
}

fn insert_runtime_input_dispatch(
    transaction: &rusqlite::Connection,
    record: &RuntimeInputDispatchRecord,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO runtime_input_dispatches(
             request_id, actor_id, task_id, run_id, response_json, response_digest,
             state, revision, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'prepared', 1, ?7, ?7)",
        params![
            record.request_id.as_str(),
            record.actor_id.as_str(),
            record.task_id.as_str(),
            record.run_id.as_str(),
            serde_json::to_string(&record.response)?,
            record.response_digest.as_str(),
            integer(record.created_at_ms, "runtime input dispatch creation")?,
        ],
    )?;
    Ok(())
}

fn insert_runtime_input_dispatch_receipt(
    transaction: &Transaction<'_>,
    command: &LedgerCommand,
    operation: &str,
    record: &RuntimeInputDispatchRecord,
) -> Result<(), StoreError> {
    insert_receipt(
        transaction,
        command,
        operation,
        &RuntimeInputDispatchReceipt {
            request_id: record.request_id.clone(),
            response_digest: record.response_digest.clone(),
            state: record.state,
            revision: record.revision,
        },
    )
}

fn replay_runtime_input_dispatch(
    transaction: &Transaction<'_>,
    command: &LedgerCommand,
    operation: &str,
    request_id: &InputRequestId,
    response_digest: &Digest,
) -> Result<Option<RuntimeInputDispatchRecord>, StoreError> {
    let Some(receipt) = replay::<RuntimeInputDispatchReceipt>(transaction, command, operation)?
    else {
        return Ok(None);
    };
    if receipt.request_id != *request_id || receipt.response_digest != *response_digest {
        return Err(StoreError::IdempotencyConflict);
    }
    let record = load_runtime_input_dispatch(transaction, request_id)?;
    let forward_state = match receipt.state {
        RuntimeInputDispatchState::Prepared => true,
        RuntimeInputDispatchState::Started => matches!(
            record.state,
            RuntimeInputDispatchState::Started
                | RuntimeInputDispatchState::Delivered
                | RuntimeInputDispatchState::Unknown
        ),
        RuntimeInputDispatchState::Delivered => {
            record.state == RuntimeInputDispatchState::Delivered
        }
        RuntimeInputDispatchState::Unknown => record.state == RuntimeInputDispatchState::Unknown,
    };
    if record.response_digest != receipt.response_digest
        || record.revision < receipt.revision
        || !forward_state
    {
        return Err(corrupt(
            "runtime input dispatch diverges from its command receipt",
        ));
    }
    Ok(Some(record))
}

fn load_recoverable_runtime_input_dispatches(
    transaction: &rusqlite::Connection,
    run_id: &RunId,
) -> Result<Vec<RuntimeInputDispatchRecord>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT request_id FROM runtime_input_dispatches
         WHERE run_id=?1 AND state IN ('prepared', 'started') ORDER BY request_id LIMIT 2",
    )?;
    let ids = statement
        .query_map(params![run_id.as_str()], |row| row.get::<_, String>(0))?
        .map(|row| {
            let raw = row?;
            InputRequestId::parse(raw)
                .map_err(|error| corrupt(&format!("invalid input request identity: {error}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    ids.iter()
        .map(|id| load_runtime_input_dispatch(transaction, id))
        .collect()
}

fn load_pending_runtime_input_requests(
    transaction: &rusqlite::Connection,
    run_id: &RunId,
) -> Result<Vec<RuntimeInputRequestRecord>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT request_id FROM runtime_input_requests
         WHERE run_id=?1 AND state='pending' ORDER BY request_id LIMIT 2",
    )?;
    let ids = statement
        .query_map(params![run_id.as_str()], |row| row.get::<_, String>(0))?
        .map(|row| {
            let raw = row?;
            InputRequestId::parse(raw)
                .map_err(|error| corrupt(&format!("invalid input request identity: {error}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    ids.iter()
        .map(|id| load_runtime_input_request(transaction, id))
        .collect()
}

fn recover_runtime_inputs_after_restart(
    transaction: &Transaction<'_>,
    now_ms: u64,
) -> Result<(u64, u64), StoreError> {
    let now = integer(now_ms, "runtime input restart recovery timestamp")?;
    let mut dispatches_unknown = 0u64;
    loop {
        let request_id = transaction
            .query_row(
                "SELECT request_id FROM runtime_input_dispatches
                 WHERE state IN ('prepared', 'started') ORDER BY updated_at_ms, request_id LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(request_id) = request_id else {
            break;
        };
        let request_id = InputRequestId::parse(request_id)
            .map_err(|error| corrupt(&format!("invalid input request identity: {error}")))?;
        let dispatch = load_runtime_input_dispatch(transaction, &request_id)?;
        let suspend_task = runtime_input_recovery_requires_suspension(
            transaction,
            &dispatch.task_id,
            &dispatch.actor_id,
            &dispatch.run_id,
            TaskState::Running,
        )?;
        let changed = transaction.execute(
            "UPDATE runtime_input_dispatches
             SET state='unknown', revision=revision+1, updated_at_ms=?2
             WHERE request_id=?1 AND state IN ('prepared', 'started') AND revision=?3",
            params![
                request_id.as_str(),
                now,
                integer(dispatch.revision, "runtime input dispatch revision")?,
            ],
        )?;
        if changed != 1 {
            return Err(conflict(
                "runtime input restart recovery lost its dispatch revision",
            ));
        }
        if suspend_task {
            append_internal_task_event(
                transaction,
                &dispatch.task_id,
                &dispatch.actor_id,
                now_ms,
                TaskEvent::RunSuspended {
                    run_id: dispatch.run_id,
                    reason: cosh_gateway_contracts::task::SuspensionCode::OperatorRequired,
                },
                None,
            )?;
        }
        dispatches_unknown = dispatches_unknown
            .checked_add(1)
            .ok_or_else(|| corrupt("runtime input dispatch recovery count overflow"))?;
    }

    let mut requests_cancelled = 0u64;
    loop {
        let request_id = transaction
            .query_row(
                "SELECT request_id FROM runtime_input_requests
                 WHERE state='pending' ORDER BY updated_at_ms, request_id LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(request_id) = request_id else {
            break;
        };
        let request_id = InputRequestId::parse(request_id)
            .map_err(|error| corrupt(&format!("invalid input request identity: {error}")))?;
        let request = load_runtime_input_request(transaction, &request_id)?;
        let suspend_task = runtime_input_recovery_requires_suspension(
            transaction,
            &request.task_id,
            &request.actor_id,
            &request.run_id,
            TaskState::WaitingInput,
        )?;
        let changed = transaction.execute(
            "UPDATE runtime_input_requests
             SET state='cancelled', revision=revision+1, updated_at_ms=?2
             WHERE request_id=?1 AND state='pending' AND revision=?3",
            params![
                request_id.as_str(),
                now,
                integer(request.revision, "runtime input request revision")?,
            ],
        )?;
        if changed != 1 {
            return Err(conflict(
                "runtime input restart recovery lost its request revision",
            ));
        }
        if suspend_task {
            append_internal_task_event(
                transaction,
                &request.task_id,
                &request.actor_id,
                now_ms,
                TaskEvent::RunSuspended {
                    run_id: request.run_id,
                    reason: cosh_gateway_contracts::task::SuspensionCode::OperatorRequired,
                },
                None,
            )?;
        }
        requests_cancelled = requests_cancelled
            .checked_add(1)
            .ok_or_else(|| corrupt("runtime input request recovery count overflow"))?;
    }
    Ok((requests_cancelled, dispatches_unknown))
}

fn runtime_input_recovery_requires_suspension(
    transaction: &Transaction<'_>,
    task_id: &TaskId,
    actor_id: &ActorId,
    run_id: &RunId,
    recoverable_state: TaskState,
) -> Result<bool, StoreError> {
    let task = load_verified_projection(transaction, task_id)?
        .ok_or_else(|| corrupt("Runtime input recovery references a missing Task"))?;
    if task.owner_actor_id() != actor_id || task.active_run_id() != Some(run_id) {
        return Err(corrupt(
            "Runtime input recovery identity diverges from its Task",
        ));
    }
    if task.state() == recoverable_state {
        return Ok(true);
    }
    if matches!(
        task.state(),
        TaskState::Suspended | TaskState::Failed | TaskState::Cancelled
    ) {
        return Ok(false);
    }
    Err(corrupt(
        "Runtime input recovery Task is neither active nor safely terminal",
    ))
}
