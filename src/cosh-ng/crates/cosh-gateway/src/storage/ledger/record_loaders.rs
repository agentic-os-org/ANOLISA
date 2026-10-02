
fn load_approval(
    transaction: &rusqlite::Connection,
    id: &ApprovalId,
) -> Result<ApprovalRecord, StoreError> {
    transaction
        .query_row(
            "SELECT request_id, actor_id, task_id, run_id, target_json, operation_digest,
         input_digest, state, revision, expires_at_ms, decided_by_actor_id, created_at_ms,
         updated_at_ms, permission_ref_json, target_identity_digest, runtime_fence_json
         FROM approvals WHERE approval_id=?1",
            params![id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<String>>(15)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| not_found("approval", id.as_str()))
        .and_then(|row| {
            let record = ApprovalRecord {
                approval_id: id.clone(),
                request_id: parse_id(&row.0)?,
                actor_id: parse_id(&row.1)?,
                task_id: parse_id(&row.2)?,
                run_id: parse_id(&row.3)?,
                target: serde_json::from_str(&row.4)?,
                target_identity_digest: row
                    .14
                    .map(Digest::parse)
                    .transpose()
                    .map_err(|_| corrupt("invalid approval target identity digest"))?,
                runtime_fence: row
                    .15
                    .map(|value| serde_json::from_str(&value))
                    .transpose()?,
                operation_digest: Digest::parse(row.5)
                    .map_err(|_| corrupt("invalid approval operation digest"))?,
                input_digest: Digest::parse(row.6)
                    .map_err(|_| corrupt("invalid approval input digest"))?,
                permission: row
                    .13
                    .map(|value| serde_json::from_str(&value))
                    .transpose()?,
                state: parse_approval_state(&row.7)?,
                revision: unsigned(row.8, "approval revision")?,
                expires_at_ms: unsigned(row.9, "approval deadline")?,
                decided_by_actor_id: row.10.map(|value| parse_id(&value)).transpose()?,
                created_at_ms: unsigned(row.11, "approval creation")?,
                updated_at_ms: unsigned(row.12, "approval update")?,
            };
            if let Some(permission) = &record.permission {
                validate_permission_binding(&record, permission)?;
            }
            Ok(record)
        })
}

fn load_brokered_request(
    connection: &rusqlite::Connection,
    request_id: &RequestId,
) -> Result<BrokeredRequestRecord, StoreError> {
    let row = connection
        .query_row(
            "SELECT approval_id, actor_id, task_id, run_id, request_json, operation_json,
                    typed_operation_digest, operation_digest, input_digest,
                    target_identity_digest, runtime_fence_json, provider_binding, created_at_ms
             FROM brokered_requests WHERE request_id=?1",
            params![request_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, i64>(12)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| not_found("brokered request", request_id.as_str()))?;
    let request = serde_json::from_str::<CapabilityRequest>(&row.4)?;
    let operation = serde_json::from_str::<BrokeredOperation>(&row.5)?;
    let typed_operation_digest =
        Digest::parse(row.6).map_err(|_| corrupt("invalid brokered typed operation digest"))?;
    let target_identity_digest = Digest::parse(row.9)
        .map_err(|_| corrupt("invalid brokered request target identity digest"))?;
    let runtime_fence = serde_json::from_str::<RuntimeExecutionFence>(&row.10)?;
    let provider_binding = row
        .11
        .map(BoundedOpaque::new)
        .transpose()
        .map_err(|_| corrupt("invalid brokered provider binding"))?;
    if request.request_id != *request_id
        || request.actor.actor_id.as_str() != row.1
        || request.task_id.as_str() != row.2
        || request.run_id.as_str() != row.3
        || request.operation_digest.as_str() != row.7
        || request.input_digest.as_str() != row.8
        || brokered_operation_digest(&operation)? != typed_operation_digest
    {
        return Err(corrupt(
            "brokered request columns diverge from the typed request",
        ));
    }
    Ok(BrokeredRequestRecord {
        request,
        operation,
        typed_operation_digest,
        target_identity_digest,
        runtime_fence,
        provider_binding,
        approval_id: row.0.map(|value| parse_id(&value)).transpose()?,
        created_at_ms: unsigned(row.12, "brokered request creation")?,
    })
}

fn insert_brokered_request(
    transaction: &Transaction<'_>,
    record: &BrokeredRequestRecord,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO brokered_requests(
             request_id, approval_id, actor_id, task_id, run_id, request_json,
             operation_json, typed_operation_digest, operation_digest, input_digest,
             target_identity_digest, runtime_fence_json, provider_binding, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            record.request.request_id.as_str(),
            record.approval_id.as_ref().map(ApprovalId::as_str),
            record.request.actor.actor_id.as_str(),
            record.request.task_id.as_str(),
            record.request.run_id.as_str(),
            serde_json::to_string(&record.request)?,
            serde_json::to_string(&record.operation)?,
            record.typed_operation_digest.as_str(),
            record.request.operation_digest.as_str(),
            record.request.input_digest.as_str(),
            record.target_identity_digest.as_str(),
            serde_json::to_string(&record.runtime_fence)?,
            record.provider_binding.as_ref().map(BoundedOpaque::as_str),
            integer(record.created_at_ms, "brokered request timestamp")?,
        ],
    )?;
    Ok(())
}

fn brokered_operation_digest(operation: &BrokeredOperation) -> Result<Digest, StoreError> {
    let encoded = serde_json::to_vec(operation)?;
    let mut hasher = Sha256::new();
    hasher.update(b"cosh.gateway.brokered-operation.v1\0");
    hasher.update(encoded);
    Digest::parse(format!("{:x}", hasher.finalize()))
        .map_err(|_| corrupt("brokered operation digest construction failed"))
}

fn brokered_result_digest(result: &BrokeredOperationResult) -> Result<Digest, StoreError> {
    let encoded = serde_json::to_vec(result)?;
    let mut hasher = Sha256::new();
    hasher.update(b"cosh.gateway.brokered-result.v1\0");
    hasher.update(encoded);
    Digest::parse(format!("{:x}", hasher.finalize()))
        .map_err(|_| corrupt("brokered result digest construction failed"))
}

fn brokered_delivery_digest(delivery: &BrokeredExecutionDelivery) -> Result<Digest, StoreError> {
    let encoded = serde_json::to_vec(delivery)?;
    let mut hasher = Sha256::new();
    hasher.update(encoded);
    Digest::parse(format!("{:x}", hasher.finalize()))
        .map_err(|_| corrupt("brokered delivery digest construction failed"))
}

fn validate_result_shape(
    operation: &BrokeredOperation,
    result: &BrokeredOperationResult,
) -> Result<(), StoreError> {
    match (operation, result) {
        (
            BrokeredOperation::WorkspaceCheckpointCreateV1(operation),
            BrokeredOperationResult::WorkspaceCheckpointCreateV1(result),
        ) if operation.checkpoint_id == result.checkpoint_id => Ok(()),
        _ => Err(conflict(
            "typed result does not match the admitted brokered operation",
        )),
    }
}

fn execution_request_id(
    connection: &rusqlite::Connection,
    execution_id: &ExecutionId,
) -> Result<RequestId, StoreError> {
    let request_id = connection
        .query_row(
            "SELECT request_id FROM permits WHERE execution_id=?1",
            params![execution_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| not_found("execution permit", execution_id.as_str()))?;
    parse_id(&request_id)
}

fn validate_completion_result(
    connection: &rusqlite::Connection,
    execution: &ExecutionRecord,
    result: &BrokeredOperationResult,
    command: &LedgerCommand,
) -> Result<BrokeredExecutionResultRecord, StoreError> {
    let request_id = execution_request_id(connection, &execution.execution_id)?;
    let request = load_brokered_request(connection, &request_id)?;
    let target_identity_digest = execution
        .target_identity_digest
        .as_ref()
        .ok_or_else(|| corrupt("successful brokered execution lacks target identity"))?;
    let runtime_fence = execution
        .runtime_fence
        .as_ref()
        .ok_or_else(|| corrupt("successful brokered execution lacks Runtime fence"))?;
    if execution.broker_state != Some(BrokerExecutionState::Started)
        || request.request.actor.actor_id != execution.actor_id
        || request.request.task_id != execution.task_id
        || request.request.run_id != execution.run_id
        || request.request.target != execution.target
        || request.request.operation_digest != execution.operation_digest
        || request.request.input_digest != execution.input_digest
        || request.target_identity_digest != *target_identity_digest
        || request.runtime_fence != *runtime_fence
    {
        return Err(corrupt(
            "brokered execution authority diverges from its durable request",
        ));
    }
    validate_result_shape(&request.operation, result)?;
    Ok(BrokeredExecutionResultRecord {
        execution_id: execution.execution_id.clone(),
        request_id,
        actor_id: execution.actor_id.clone(),
        task_id: execution.task_id.clone(),
        run_id: execution.run_id.clone(),
        result: result.clone(),
        result_digest: brokered_result_digest(result)?,
        operation: request.operation,
        operation_digest: execution.operation_digest.clone(),
        input_digest: execution.input_digest.clone(),
        target_identity_digest: target_identity_digest.clone(),
        runtime_fence: runtime_fence.clone(),
        committed_at_ms: command.committed_at_ms,
    })
}

fn insert_brokered_execution_result(
    transaction: &Transaction<'_>,
    record: &BrokeredExecutionResultRecord,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO brokered_execution_results(
             execution_id, request_id, actor_id, task_id, run_id, result_json, result_digest,
             operation_json, operation_digest, input_digest, target_identity_digest,
             runtime_fence_json, committed_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            record.execution_id.as_str(),
            record.request_id.as_str(),
            record.actor_id.as_str(),
            record.task_id.as_str(),
            record.run_id.as_str(),
            serde_json::to_string(&record.result)?,
            record.result_digest.as_str(),
            serde_json::to_string(&record.operation)?,
            record.operation_digest.as_str(),
            record.input_digest.as_str(),
            record.target_identity_digest.as_str(),
            serde_json::to_string(&record.runtime_fence)?,
            integer(record.committed_at_ms, "brokered result timestamp")?,
        ],
    )?;
    Ok(())
}

fn load_brokered_runtime_dispatch(
    connection: &rusqlite::Connection,
    request_id: &RequestId,
    kind: BrokeredRuntimeDispatchKind,
) -> Result<BrokeredRuntimeDispatchRecord, StoreError> {
    let row = connection
        .query_row(
            "SELECT actor_id, task_id, run_id, brokered_ref_json, payload_digest,
                    source_kind, source_id, state, revision, created_at_ms, updated_at_ms
             FROM brokered_runtime_dispatches
             WHERE request_id=?1 AND dispatch_kind=?2",
            params![request_id.as_str(), state_name(kind)?],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| not_found("brokered Runtime dispatch", request_id.as_str()))?;
    let brokered = serde_json::from_str::<BrokeredExecutionRef>(&row.3)?;
    let source_id = &row.6;
    let source = match row.5.as_str() {
        "approval_pending" => BrokeredRuntimeDispatchSource::ApprovalPending {
            approval_id: parse_id(source_id)?,
        },
        "approval_denied" => BrokeredRuntimeDispatchSource::ApprovalDenied {
            approval_id: parse_id(source_id)?,
        },
        "execution" => BrokeredRuntimeDispatchSource::Execution {
            execution_id: parse_id(source_id)?,
        },
        _ => return Err(corrupt("invalid brokered Runtime dispatch source")),
    };
    let record = BrokeredRuntimeDispatchRecord {
        brokered,
        actor_id: parse_id(&row.0)?,
        task_id: parse_id(&row.1)?,
        kind,
        payload_digest: Digest::parse(row.4)
            .map_err(|_| corrupt("invalid brokered Runtime dispatch payload digest"))?,
        source,
        state: parse_state(&row.7)?,
        revision: unsigned(row.8, "brokered dispatch revision")?,
        created_at_ms: unsigned(row.9, "brokered dispatch creation")?,
        updated_at_ms: unsigned(row.10, "brokered dispatch update")?,
    };
    if record.brokered.request_id != *request_id
        || record.brokered.run_id.as_str() != row.2
        || brokered_dispatch_source_columns(&record.source) != (row.5.as_str(), row.6.as_str())
    {
        return Err(corrupt(
            "brokered Runtime dispatch columns diverge from its typed binding",
        ));
    }
    Ok(record)
}

fn load_provider_permission_dispatch(
    transaction: &rusqlite::Connection,
    approval_id: &ApprovalId,
) -> Result<ProviderPermissionDispatchRecord, StoreError> {
    transaction
        .query_row(
            "SELECT actor_id, task_id, run_id, permission_ref_json, decision, state,
             revision, created_at_ms, updated_at_ms
             FROM provider_permission_dispatches WHERE approval_id=?1",
            params![approval_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| not_found("provider permission dispatch", approval_id.as_str()))
        .and_then(|row| {
            let permission = serde_json::from_str::<RuntimePermissionRef>(&row.3)?;
            let record = ProviderPermissionDispatchRecord {
                approval_id: approval_id.clone(),
                actor_id: parse_id(&row.0)?,
                task_id: parse_id(&row.1)?,
                run_id: parse_id(&row.2)?,
                permission,
                decision: parse_state(&row.4)?,
                state: parse_state(&row.5)?,
                revision: unsigned(row.6, "dispatch revision")?,
                created_at_ms: unsigned(row.7, "dispatch creation")?,
                updated_at_ms: unsigned(row.8, "dispatch update")?,
            };
            if record.permission.request_id != load_approval(transaction, approval_id)?.request_id
                || record.permission.run_id != record.run_id
            {
                return Err(corrupt(
                    "provider permission dispatch diverges from its approval binding",
                ));
            }
            Ok(record)
        })
}

fn load_permit(
    transaction: &rusqlite::Connection,
    id: &PermitId,
) -> Result<PermitRecord, StoreError> {
    let row = transaction
        .query_row(
            "SELECT request_id, approval_id, actor_id, task_id, run_id, execution_id, target_json,
         operation_digest, input_digest, policy_revision, state, single_use, valid_until_ms,
         consumed_at_ms, created_at_ms, target_identity_digest, runtime_fence_json
         FROM permits WHERE permit_id=?1",
            params![id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, Option<i64>>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, Option<String>>(16)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| not_found("permit", id.as_str()))?;
    if row.11 != 1 {
        return Err(corrupt("durable permit is not single-use"));
    }
    let target_identity_digest = row
        .15
        .ok_or_else(|| corrupt("brokered permit is missing target identity"))
        .and_then(|value| {
            Digest::parse(value).map_err(|_| corrupt("invalid permit target identity digest"))
        })?;
    let runtime_fence = row
        .16
        .ok_or_else(|| corrupt("brokered permit is missing Runtime fence"))
        .and_then(|value| Ok(serde_json::from_str(&value)?))?;
    Ok(PermitRecord {
        permit: ExecutionPermit {
            permit_id: id.clone(),
            request_id: parse_id(&row.0)?,
            approval_id: row.1.map(|value| parse_id(&value)).transpose()?,
            actor_id: parse_id(&row.2)?,
            task_id: parse_id(&row.3)?,
            run_id: parse_id(&row.4)?,
            execution_id: parse_id(&row.5)?,
            target: serde_json::from_str(&row.6)?,
            target_identity_digest,
            runtime_fence,
            operation_digest: Digest::parse(row.7)
                .map_err(|_| corrupt("invalid permit operation digest"))?,
            input_digest: Digest::parse(row.8)
                .map_err(|_| corrupt("invalid permit input digest"))?,
            policy_revision: unsigned(row.9, "policy revision")?,
            valid_until_ms: unsigned(row.12, "permit deadline")?,
            single_use: true,
        },
        state: parse_permit_state(&row.10)?,
        consumed_at_ms: row
            .13
            .map(|value| unsigned(value, "permit consumption"))
            .transpose()?,
        created_at_ms: unsigned(row.14, "permit creation")?,
    })
}
