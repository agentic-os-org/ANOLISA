
fn prepare_brokered_runtime_dispatch(
    store: &mut SqliteTaskStore,
    command: &LedgerCommand,
    kind: BrokeredRuntimeDispatchKind,
    source: BrokeredRuntimeDispatchSource,
    brokered: &BrokeredExecutionRef,
    supplied_digest: Option<&Digest>,
    delivery: Option<&BrokeredExecutionDelivery>,
    lease: &LeaseClaim,
) -> Result<LedgerOutcome<BrokeredRuntimeDispatchRecord>, StoreError> {
    validate_command(command)?;
    let transaction = immediate(store)?;
    let canonical_digest = delivery.map(brokered_delivery_digest).transpose()?;
    let payload_digest = match (supplied_digest, canonical_digest.as_ref(), kind) {
        (Some(digest), None, BrokeredRuntimeDispatchKind::Acknowledgement) => digest,
        (None, Some(digest), BrokeredRuntimeDispatchKind::Result) => digest,
        _ => return Err(conflict("brokered dispatch payload kind is invalid")),
    };
    if let Some(replayed) = replay::<BrokeredRuntimeDispatchRecord>(
        &transaction,
        command,
        "prepare_brokered_runtime_dispatch",
    )? {
        if replayed.kind != kind || replayed.source != source {
            return Err(StoreError::IdempotencyConflict);
        }
        require_brokered_dispatch_context(
            &transaction,
            &command.actor_id,
            &replayed,
            brokered,
            payload_digest,
            lease,
            command.committed_at_ms,
        )?;
        transaction.commit()?;
        return Ok(LedgerOutcome::Replayed(replayed));
    }
    let request = load_brokered_request(&transaction, &brokered.request_id)?;
    let record = BrokeredRuntimeDispatchRecord {
        brokered: brokered.clone(),
        actor_id: request.request.actor.actor_id.clone(),
        task_id: request.request.task_id.clone(),
        kind,
        payload_digest: payload_digest.clone(),
        source,
        state: BrokeredRuntimeDispatchState::Prepared,
        revision: 1,
        created_at_ms: command.committed_at_ms,
        updated_at_ms: command.committed_at_ms,
    };
    require_brokered_dispatch_context(
        &transaction,
        &command.actor_id,
        &record,
        brokered,
        payload_digest,
        lease,
        command.committed_at_ms,
    )?;
    require_brokered_dispatch_ready(&transaction, &request, &record, delivery)?;
    let (source_kind, source_id) = brokered_dispatch_source_columns(&record.source);
    transaction.execute(
        "INSERT INTO brokered_runtime_dispatches(
             request_id, dispatch_kind, actor_id, task_id, run_id, brokered_ref_json,
             payload_digest, source_kind, source_id, state, revision, created_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'prepared', 1, ?10, ?10)",
        params![
            brokered.request_id.as_str(),
            state_name(kind)?,
            record.actor_id.as_str(),
            record.task_id.as_str(),
            brokered.run_id.as_str(),
            serde_json::to_string(brokered)?,
            payload_digest.as_str(),
            source_kind,
            source_id,
            integer(command.committed_at_ms, "dispatch preparation timestamp")?,
        ],
    )?;
    insert_receipt(
        &transaction,
        command,
        "prepare_brokered_runtime_dispatch",
        &record,
    )?;
    transaction.commit()?;
    Ok(LedgerOutcome::Applied(record))
}

// Keeping every expected binding explicit prevents a partial dispatch transition.
#[allow(clippy::too_many_arguments)]
fn transition_brokered_runtime_dispatch(
    store: &mut SqliteTaskStore,
    command: &LedgerCommand,
    kind: BrokeredRuntimeDispatchKind,
    brokered: &BrokeredExecutionRef,
    payload_digest: &Digest,
    expected_revision: u64,
    lease: &LeaseClaim,
    expected_state: BrokeredRuntimeDispatchState,
    next_state: BrokeredRuntimeDispatchState,
    operation: &str,
) -> Result<LedgerOutcome<BrokeredRuntimeDispatchRecord>, StoreError> {
    validate_command(command)?;
    integer(expected_revision, "brokered dispatch expected revision")?;
    let transaction = immediate(store)?;
    if let Some(replayed) =
        replay::<BrokeredRuntimeDispatchRecord>(&transaction, command, operation)?
    {
        if replayed.kind != kind {
            return Err(StoreError::IdempotencyConflict);
        }
        require_brokered_dispatch_context(
            &transaction,
            &command.actor_id,
            &replayed,
            brokered,
            payload_digest,
            lease,
            command.committed_at_ms,
        )?;
        transaction.commit()?;
        return Ok(LedgerOutcome::Replayed(replayed));
    }
    let mut record = load_brokered_runtime_dispatch(&transaction, &brokered.request_id, kind)?;
    require_brokered_dispatch_context(
        &transaction,
        &command.actor_id,
        &record,
        brokered,
        payload_digest,
        lease,
        command.committed_at_ms,
    )?;
    if record.state != expected_state || record.revision != expected_revision {
        return Err(conflict(
            "brokered Runtime dispatch is not at the expected state and revision",
        ));
    }
    require_not_before(
        command.committed_at_ms,
        record.updated_at_ms,
        "brokered Runtime dispatch transition",
    )?;
    let prior_state = state_name(expected_state)?;
    record.state = next_state;
    record.revision = next_integer(record.revision, "brokered dispatch revision")?;
    record.updated_at_ms = command.committed_at_ms;
    let changed = transaction.execute(
        "UPDATE brokered_runtime_dispatches
         SET state=?3, revision=?4, updated_at_ms=?5
         WHERE request_id=?1 AND dispatch_kind=?2 AND state=?6 AND revision=?7",
        params![
            brokered.request_id.as_str(),
            state_name(kind)?,
            state_name(next_state)?,
            integer(record.revision, "brokered dispatch revision")?,
            integer(command.committed_at_ms, "brokered dispatch timestamp")?,
            prior_state,
            integer(expected_revision, "brokered dispatch expected revision")?,
        ],
    )?;
    if changed != 1 {
        return Err(conflict(
            "brokered Runtime dispatch lost its state or revision precondition",
        ));
    }
    insert_receipt(&transaction, command, operation, &record)?;
    transaction.commit()?;
    Ok(LedgerOutcome::Applied(record))
}

fn require_brokered_dispatch_context(
    connection: &rusqlite::Connection,
    actor_id: &ActorId,
    record: &BrokeredRuntimeDispatchRecord,
    brokered: &BrokeredExecutionRef,
    payload_digest: &Digest,
    lease: &LeaseClaim,
    now_ms: u64,
) -> Result<(), StoreError> {
    if record.actor_id != *actor_id
        || record.brokered != *brokered
        || record.payload_digest != *payload_digest
        || record.task_id != lease.task_id
        || brokered.run_id != lease.run_id
        || brokered.runtime_generation != lease.generation
        || brokered.event_sequence == 0
    {
        return Err(conflict(
            "brokered dispatch actor, payload, callback, or lease binding differs",
        ));
    }
    require_current_lease(connection, lease, actor_id, now_ms)?;
    let request = load_brokered_request(connection, &brokered.request_id)?;
    if request.request.actor.actor_id != *actor_id
        || request.request.task_id != record.task_id
        || request.request.run_id != brokered.run_id
        || request.operation != brokered.operation
        || request.runtime_fence.binding_id != brokered.binding_id
        || request.runtime_fence.runtime_generation != brokered.runtime_generation
        || request.runtime_fence.lease_generation != lease.generation
    {
        return Err(conflict(
            "brokered dispatch diverges from its durable request authority",
        ));
    }
    let binding = load_runtime_binding(connection, &brokered.binding_id)?;
    if binding.actor_id != *actor_id
        || binding.state != RuntimeBindingState::Active
        || binding.binding.task_id != record.task_id
        || binding.binding.run_id != brokered.run_id
        || binding.binding.runtime_generation != brokered.runtime_generation
        || binding.last_sequence < brokered.event_sequence
    {
        return Err(conflict(
            "brokered dispatch Runtime binding is stale or has not observed the event",
        ));
    }
    Ok(())
}

fn require_brokered_dispatch_ready(
    connection: &rusqlite::Connection,
    request: &BrokeredRequestRecord,
    record: &BrokeredRuntimeDispatchRecord,
    delivery: Option<&BrokeredExecutionDelivery>,
) -> Result<(), StoreError> {
    let task = load_authoritative_task(connection, &record.task_id)?;
    match &record.source {
        BrokeredRuntimeDispatchSource::ApprovalPending { approval_id } => {
            let approval = load_approval(connection, approval_id)?;
            if record.kind != BrokeredRuntimeDispatchKind::Acknowledgement
                || request.approval_id.as_ref() != Some(approval_id)
                || !brokered_approval_matches_request(&approval, request)
                || approval.state != ApprovalState::Pending
                || task.state() != TaskState::WaitingApproval
                || delivery.is_some()
            {
                return Err(conflict(
                    "brokered acknowledgement prerequisites are not durable",
                ));
            }
        }
        BrokeredRuntimeDispatchSource::ApprovalDenied { approval_id } => {
            let approval = load_approval(connection, approval_id)?;
            let valid_delivery = matches!(
                delivery,
                Some(BrokeredExecutionDelivery {
                    request_id,
                    outcome: BrokeredExecutionOutcome::Denied {
                        code: DenialCode::ApprovalDenied,
                        ..
                    },
                }) if request_id == &request.request.request_id
            );
            if record.kind != BrokeredRuntimeDispatchKind::Result
                || request.approval_id.as_ref() != Some(approval_id)
                || !brokered_approval_matches_request(&approval, request)
                || approval.state != ApprovalState::Denied
                || task.state() != TaskState::Running
                || !valid_delivery
            {
                return Err(conflict("brokered denial is not durable"));
            }
        }
        BrokeredRuntimeDispatchSource::Execution { execution_id } => {
            let execution = load_execution(connection, execution_id)?;
            let permit_request = connection
                .query_row(
                    "SELECT request_id FROM permits WHERE execution_id=?1",
                    params![execution_id.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| not_found("execution permit", execution_id.as_str()))?;
            let ready = matches!(
                execution.state,
                ExecutionState::Succeeded | ExecutionState::Failed | ExecutionState::Uncertain
            ) || execution.broker_state == Some(BrokerExecutionState::KnownNoEffect);
            let expected_task_state = if execution.state == ExecutionState::Uncertain {
                TaskState::Suspended
            } else {
                TaskState::Running
            };
            let valid_delivery = match (delivery, execution.state, execution.broker_state) {
                (
                    Some(BrokeredExecutionDelivery {
                        request_id,
                        outcome:
                            BrokeredExecutionOutcome::Succeeded {
                                execution_id: delivered_execution,
                                result,
                            },
                    }),
                    ExecutionState::Succeeded,
                    _,
                ) => {
                    request_id == &request.request.request_id
                        && delivered_execution == execution_id
                        && load_brokered_execution_result(connection, execution_id)
                            .map(|durable| durable.result == *result)?
                }
                (
                    Some(BrokeredExecutionDelivery {
                        request_id,
                        outcome:
                            BrokeredExecutionOutcome::Failed {
                                execution_id: delivered_execution,
                                ..
                            },
                    }),
                    ExecutionState::Failed,
                    _,
                )
                | (
                    Some(BrokeredExecutionDelivery {
                        request_id,
                        outcome:
                            BrokeredExecutionOutcome::Failed {
                                execution_id: delivered_execution,
                                ..
                            },
                    }),
                    ExecutionState::Planned,
                    Some(BrokerExecutionState::KnownNoEffect),
                ) => {
                    request_id == &request.request.request_id && delivered_execution == execution_id
                }
                (
                    Some(BrokeredExecutionDelivery {
                        request_id,
                        outcome:
                            BrokeredExecutionOutcome::Uncertain {
                                execution_id: delivered_execution,
                                ..
                            },
                    }),
                    ExecutionState::Uncertain,
                    _,
                ) => {
                    request_id == &request.request.request_id && delivered_execution == execution_id
                }
                _ => false,
            };
            if record.kind != BrokeredRuntimeDispatchKind::Result
                || permit_request != record.brokered.request_id.as_str()
                || execution.actor_id != record.actor_id
                || execution.task_id != record.task_id
                || execution.run_id != record.brokered.run_id
                || execution.target != request.request.target
                || execution.target_identity_digest.as_ref()
                    != Some(&request.target_identity_digest)
                || execution.runtime_fence.as_ref() != Some(&request.runtime_fence)
                || execution.operation_digest != request.request.operation_digest
                || execution.input_digest != request.request.input_digest
                || !ready
                || task.state() != expected_task_state
                || !valid_delivery
            {
                return Err(conflict(
                    "brokered execution result prerequisites are not durable",
                ));
            }
        }
    }
    Ok(())
}

fn brokered_approval_matches_request(
    approval: &ApprovalRecord,
    request: &BrokeredRequestRecord,
) -> bool {
    approval.request_id == request.request.request_id
        && approval.actor_id == request.request.actor.actor_id
        && approval.task_id == request.request.task_id
        && approval.run_id == request.request.run_id
        && approval.target == request.request.target
        && approval.target_identity_digest.as_ref() == Some(&request.target_identity_digest)
        && approval.runtime_fence.as_ref() == Some(&request.runtime_fence)
        && approval.operation_digest == request.request.operation_digest
        && approval.input_digest == request.request.input_digest
}

fn brokered_dispatch_source_columns(
    source: &BrokeredRuntimeDispatchSource,
) -> (&'static str, &str) {
    match source {
        BrokeredRuntimeDispatchSource::ApprovalPending { approval_id } => {
            ("approval_pending", approval_id.as_str())
        }
        BrokeredRuntimeDispatchSource::ApprovalDenied { approval_id } => {
            ("approval_denied", approval_id.as_str())
        }
        BrokeredRuntimeDispatchSource::Execution { execution_id } => {
            ("execution", execution_id.as_str())
        }
    }
}

fn load_brokered_recovery_candidates(
    transaction: &rusqlite::Connection,
    broker_state: &str,
    execution_state: ExecutionState,
) -> Result<Vec<ExecutionRecord>, StoreError> {
    let state = state_name(execution_state)?;
    let ids = {
        let mut statement = transaction.prepare(
            "SELECT execution_id FROM executions
             WHERE broker_state=?1 AND state=?2 ORDER BY created_at_ms, execution_id",
        )?;
        let rows = statement
            .query_map(params![broker_state, state], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    ids.into_iter()
        .map(|value| {
            let id = ExecutionId::parse(value)
                .map_err(|_| corrupt("invalid brokered recovery execution identity"))?;
            load_execution(transaction, &id)
        })
        .collect()
}

fn load_brokered_recovery_candidates_for_run(
    connection: &rusqlite::Connection,
    run_id: &RunId,
    broker_state: &str,
    execution_state: ExecutionState,
    current_generation: u64,
) -> Result<Vec<ExecutionRecord>, StoreError> {
    let state = state_name(execution_state)?;
    let ids = {
        let mut statement = connection.prepare(
            "SELECT execution_id FROM executions
             WHERE run_id=?1 AND broker_state=?2 AND state=?3
             ORDER BY created_at_ms, execution_id",
        )?;
        let rows = statement
            .query_map(params![run_id.as_str(), broker_state, state], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    ids.into_iter()
        .map(|value| {
            let id = ExecutionId::parse(value)
                .map_err(|_| corrupt("invalid brokered recovery execution identity"))?;
            load_execution(connection, &id)
        })
        .map(|result| match result {
            Ok(execution) => match execution.runtime_fence.as_ref() {
                Some(fence) if fence.lease_generation < current_generation => Ok(execution),
                Some(_) => Err(conflict(
                    "brokered execution recovery requires a newer lease generation",
                )),
                None => Err(corrupt(
                    "brokered recovery execution is missing its Runtime fence",
                )),
            },
            Err(error) => Err(error),
        })
        .collect()
}

fn apply_brokered_execution_recovery(
    transaction: &Transaction<'_>,
    claimed: &[ExecutionRecord],
    started: &[ExecutionRecord],
    now_ms: u64,
) -> Result<BrokeredExecutionRecoveryReport, StoreError> {
    let now = integer(now_ms, "execution recovery timestamp")?;
    for execution in claimed {
        append_internal_task_event(
            transaction,
            &execution.task_id,
            &execution.actor_id,
            now_ms,
            TaskEvent::ExecutionResultRecorded {
                execution_id: execution.execution_id.clone(),
                outcome: ExecutionOutcome::Failed {
                    error: ContractError::new(
                        "executor_restarted_before_effect",
                        ErrorCategory::RuntimeUnavailable,
                        false,
                        "Executor restarted before the external effect began",
                    )
                    .map_err(|_| corrupt("static recovery error is invalid"))?,
                },
            },
            None,
        )?;
        let changed = transaction.execute(
            "UPDATE executions SET broker_state='known_no_effect', revision=revision+1,
                 updated_at_ms=?2
             WHERE execution_id=?1 AND state='planned' AND broker_state='claimed'",
            params![execution.execution_id.as_str(), now],
        )?;
        if changed != 1 {
            return Err(corrupt(
                "claimed recovery lost its known-no-effect precondition",
            ));
        }
    }
    for execution in started {
        append_internal_task_event(
            transaction,
            &execution.task_id,
            &execution.actor_id,
            now_ms,
            TaskEvent::ExecutionUncertain {
                execution_id: execution.execution_id.clone(),
                reason: UncertaintyCode::ExecutorRestarted,
            },
            None,
        )?;
        let changed = transaction.execute(
            "UPDATE executions SET state='uncertain', revision=revision+1,
                 completed_at_ms=?2, updated_at_ms=?2
             WHERE execution_id=?1 AND state='started' AND broker_state='started'",
            params![execution.execution_id.as_str(), now],
        )?;
        if changed != 1 {
            return Err(corrupt(
                "started recovery lost its uncertainty precondition",
            ));
        }
    }
    Ok(BrokeredExecutionRecoveryReport {
        executions_known_no_effect: claimed.len() as u64,
        executions_uncertain: started.len() as u64,
    })
}
