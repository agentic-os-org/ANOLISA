
fn load_brokered_execution_result(
    connection: &rusqlite::Connection,
    execution_id: &ExecutionId,
) -> Result<BrokeredExecutionResultRecord, StoreError> {
    let execution = load_execution(connection, execution_id)?;
    match execution.typed_result_state {
        TypedExecutionResultState::LegacyUnavailable => {
            return Err(StoreError::LegacyBrokeredResultUnavailable {
                execution_id: execution_id.as_str().to_owned(),
            });
        }
        TypedExecutionResultState::NotApplicable => {
            return Err(not_found(
                "brokered execution result",
                execution_id.as_str(),
            ));
        }
        TypedExecutionResultState::Available => {}
    }
    let row = connection
        .query_row(
            "SELECT request_id, actor_id, task_id, run_id, result_json, result_digest,
                    operation_json, operation_digest, input_digest, target_identity_digest,
                    runtime_fence_json, committed_at_ms
             FROM brokered_execution_results WHERE execution_id=?1",
            params![execution_id.as_str()],
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
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, i64>(11)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| corrupt("available brokered result payload is missing"))?;
    let record = BrokeredExecutionResultRecord {
        execution_id: execution_id.clone(),
        request_id: parse_id(&row.0)?,
        actor_id: parse_id(&row.1)?,
        task_id: parse_id(&row.2)?,
        run_id: parse_id(&row.3)?,
        result: serde_json::from_str(&row.4)?,
        result_digest: Digest::parse(row.5)
            .map_err(|_| corrupt("invalid brokered result digest"))?,
        operation: serde_json::from_str(&row.6)?,
        operation_digest: Digest::parse(row.7)
            .map_err(|_| corrupt("invalid brokered result operation digest"))?,
        input_digest: Digest::parse(row.8)
            .map_err(|_| corrupt("invalid brokered result input digest"))?,
        target_identity_digest: Digest::parse(row.9)
            .map_err(|_| corrupt("invalid brokered result target identity digest"))?,
        runtime_fence: serde_json::from_str(&row.10)?,
        committed_at_ms: unsigned(row.11, "brokered result timestamp")?,
    };
    let request_id = execution_request_id(connection, execution_id)?;
    let request = load_brokered_request(connection, &request_id)?;
    if execution.state != ExecutionState::Succeeded
        || record.request_id != request_id
        || record.actor_id != execution.actor_id
        || record.task_id != execution.task_id
        || record.run_id != execution.run_id
        || record.operation != request.operation
        || record.operation_digest != execution.operation_digest
        || record.input_digest != execution.input_digest
        || Some(&record.target_identity_digest) != execution.target_identity_digest.as_ref()
        || Some(&record.runtime_fence) != execution.runtime_fence.as_ref()
        || record.result_digest != brokered_result_digest(&record.result)?
        || record.committed_at_ms != execution.completed_at_ms.unwrap_or_default()
        || validate_result_shape(&record.operation, &record.result).is_err()
    {
        return Err(corrupt(
            "brokered result payload diverges from its execution authority",
        ));
    }
    Ok(record)
}

fn load_execution(
    transaction: &rusqlite::Connection,
    id: &ExecutionId,
) -> Result<ExecutionRecord, StoreError> {
    let row = transaction
        .query_row(
            "SELECT actor_id, task_id, run_id, target_json, operation_digest, input_digest, state,
         revision, started_at_ms, completed_at_ms, created_at_ms, updated_at_ms,
         target_identity_digest, runtime_fence_json, broker_state, claimed_at_ms,
         start_audit_proof_digest, typed_result_state
         FROM executions WHERE execution_id=?1",
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
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<i64>>(15)?,
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, Option<String>>(17)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| not_found("execution", id.as_str()))?;
    let record = ExecutionRecord {
        execution_id: id.clone(),
        actor_id: parse_id(&row.0)?,
        task_id: parse_id(&row.1)?,
        run_id: parse_id(&row.2)?,
        target: serde_json::from_str(&row.3)?,
        target_identity_digest: row
            .12
            .map(Digest::parse)
            .transpose()
            .map_err(|_| corrupt("invalid execution target identity digest"))?,
        runtime_fence: row
            .13
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
        broker_state: row.14.map(|value| parse_state(&value)).transpose()?,
        claimed_at_ms: row
            .15
            .map(|value| unsigned(value, "execution claim"))
            .transpose()?,
        start_audit_proof_digest: row
            .16
            .map(Digest::parse)
            .transpose()
            .map_err(|_| corrupt("invalid execution start audit proof digest"))?,
        typed_result_state: row
            .17
            .as_deref()
            .ok_or_else(|| corrupt("execution is missing typed result state"))
            .and_then(parse_typed_result_state)?,
        operation_digest: Digest::parse(row.4)
            .map_err(|_| corrupt("invalid execution operation digest"))?,
        input_digest: Digest::parse(row.5)
            .map_err(|_| corrupt("invalid execution input digest"))?,
        state: parse_execution_state(&row.6)?,
        revision: unsigned(row.7, "execution revision")?,
        started_at_ms: row
            .8
            .map(|value| unsigned(value, "execution start"))
            .transpose()?,
        completed_at_ms: row
            .9
            .map(|value| unsigned(value, "execution completion"))
            .transpose()?,
        created_at_ms: unsigned(row.10, "execution creation")?,
        updated_at_ms: unsigned(row.11, "execution update")?,
    };
    validate_execution_receipt(transaction, &record)?;
    Ok(record)
}

fn validate_execution_receipt(
    transaction: &rusqlite::Connection,
    execution: &ExecutionRecord,
) -> Result<(), StoreError> {
    let receipt = transaction
        .query_row(
            "SELECT state FROM execution_receipts WHERE execution_id=?1",
            params![execution.execution_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let expected = match execution.state {
        ExecutionState::Succeeded => Some("succeeded"),
        ExecutionState::Failed => Some("failed"),
        ExecutionState::Planned | ExecutionState::Started | ExecutionState::Uncertain => None,
    };
    if receipt.as_deref() != expected {
        return Err(corrupt(
            "execution terminal state and durable receipt are inconsistent",
        ));
    }
    let has_typed_result: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM brokered_execution_results WHERE execution_id=?1
         )",
        params![execution.execution_id.as_str()],
        |row| row.get(0),
    )?;
    let valid_result_state = match execution.typed_result_state {
        TypedExecutionResultState::Available => {
            execution.state == ExecutionState::Succeeded && has_typed_result
        }
        TypedExecutionResultState::LegacyUnavailable => {
            execution.state == ExecutionState::Succeeded && !has_typed_result
        }
        TypedExecutionResultState::NotApplicable => {
            execution.state != ExecutionState::Succeeded && !has_typed_result
        }
    };
    if !valid_result_state {
        return Err(corrupt(
            "execution typed result state and durable payload are inconsistent",
        ));
    }
    let audit_proof = transaction
        .query_row(
            "SELECT proof_digest FROM security_audit_proofs WHERE execution_id=?1",
            params![execution.execution_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match execution.broker_state {
        None => {
            if execution.target_identity_digest.is_some()
                || execution.runtime_fence.is_some()
                || execution.claimed_at_ms.is_some()
                || execution.start_audit_proof_digest.is_some()
                || audit_proof.is_some()
            {
                return Err(corrupt("legacy execution has partial brokered authority"));
            }
        }
        Some(BrokerExecutionState::Planned) => {
            if execution.state != ExecutionState::Planned
                || execution.claimed_at_ms.is_some()
                || execution.start_audit_proof_digest.is_some()
                || audit_proof.is_some()
            {
                return Err(corrupt("planned brokered execution has started evidence"));
            }
        }
        Some(BrokerExecutionState::Claimed) => {
            if execution.state != ExecutionState::Planned
                || execution.claimed_at_ms.is_none()
                || execution.start_audit_proof_digest.is_some()
                || audit_proof.is_some()
            {
                return Err(corrupt(
                    "claimed brokered execution has invalid effect evidence",
                ));
            }
        }
        Some(BrokerExecutionState::Started) => {
            if execution.state == ExecutionState::Planned
                || execution.claimed_at_ms.is_none()
                || execution
                    .start_audit_proof_digest
                    .as_ref()
                    .map(Digest::as_str)
                    != audit_proof.as_deref()
            {
                return Err(corrupt(
                    "started brokered execution lacks exact audit proof",
                ));
            }
        }
        Some(BrokerExecutionState::KnownNoEffect) => {
            if execution.state != ExecutionState::Planned
                || execution.claimed_at_ms.is_none()
                || execution.start_audit_proof_digest.is_some()
                || audit_proof.is_some()
            {
                return Err(corrupt("known-no-effect execution has effect evidence"));
            }
        }
    }
    if execution.broker_state.is_some()
        && (execution.target_identity_digest.is_none() || execution.runtime_fence.is_none())
    {
        return Err(corrupt("brokered execution is missing immutable authority"));
    }
    Ok(())
}

fn validate_all_execution_receipts(transaction: &rusqlite::Connection) -> Result<(), StoreError> {
    let inconsistent: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM executions e
             LEFT JOIN execution_receipts r ON r.execution_id=e.execution_id
             WHERE (e.state='succeeded' AND (r.state IS NULL OR r.state!='succeeded'))
                OR (e.state='failed' AND (r.state IS NULL OR r.state!='failed'))
                OR (e.state NOT IN ('succeeded', 'failed') AND r.state IS NOT NULL)
                OR (e.state='succeeded' AND e.typed_result_state NOT IN
                    ('available', 'legacy_unavailable'))
                OR (e.state!='succeeded' AND e.typed_result_state!='not_applicable')
                OR (e.typed_result_state='available' AND NOT EXISTS (
                    SELECT 1 FROM brokered_execution_results b
                    WHERE b.execution_id=e.execution_id
                ))
                OR (e.typed_result_state!='available' AND EXISTS (
                    SELECT 1 FROM brokered_execution_results b
                    WHERE b.execution_id=e.execution_id
                ))
         )",
        [],
        |row| row.get(0),
    )?;
    if inconsistent {
        return Err(corrupt(
            "execution ledger contains a terminal receipt inconsistency",
        ));
    }
    Ok(())
}
