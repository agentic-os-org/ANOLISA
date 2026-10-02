impl SqliteTaskStore {

    /// Consumes one exact permit and commits a known-no-effect claimed boundary.
    pub fn claim_execution(
        &mut self,
        command: &LedgerCommand,
        claim: &ExecutionClaim,
    ) -> Result<LedgerOutcome<ExecutionRecord>, StoreError> {
        validate_command(command)?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "claim_execution")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        require_task_run(
            &transaction,
            &claim.task_id,
            &claim.run_id,
            &command.actor_id,
        )?;
        if claim.lease.task_id != claim.task_id || claim.lease.run_id != claim.run_id {
            return Err(conflict(
                "execution lease does not bind the claimed Task and Run",
            ));
        }
        require_current_lease(
            &transaction,
            &claim.lease,
            &command.actor_id,
            command.committed_at_ms,
        )?;
        integer(claim.policy_revision, "execution policy revision")?;
        let permit = load_permit(&transaction, &claim.permit_id)?;
        if permit.state != PermitState::Issued
            || permit.permit.actor_id != command.actor_id
            || permit.permit.execution_id != claim.execution_id
            || permit.permit.task_id != claim.task_id
            || permit.permit.run_id != claim.run_id
            || permit.permit.target != claim.target
            || permit.permit.target_identity_digest != claim.target_identity_digest
            || permit.permit.runtime_fence != claim.runtime_fence
            || permit.permit.operation_digest != claim.operation_digest
            || permit.permit.input_digest != claim.input_digest
            || permit.permit.policy_revision != claim.policy_revision
        {
            return Err(conflict(
                "execution claim does not exactly match an issued permit",
            ));
        }
        if claim.runtime_fence.lease_generation != claim.lease.generation {
            return Err(conflict(
                "execution Runtime fence does not match the current Run lease generation",
            ));
        }
        require_runtime_fence(
            &transaction,
            &command.actor_id,
            &claim.task_id,
            &claim.run_id,
            &claim.runtime_fence,
            command.committed_at_ms,
        )?;
        if command.committed_at_ms >= permit.permit.valid_until_ms {
            return Err(conflict("permit expired before execution start"));
        }
        require_not_before(
            command.committed_at_ms,
            permit.created_at_ms,
            "execution claim",
        )?;
        let now = integer(command.committed_at_ms, "execution claim timestamp")?;
        let changed = transaction.execute(
            "UPDATE permits SET state = 'consumed', consumed_at_ms = ?2
             WHERE permit_id = ?1 AND state = 'issued' AND consumed_at_ms IS NULL",
            params![claim.permit_id.as_str(), now],
        )?;
        let claimed = transaction.execute(
            "UPDATE executions SET broker_state = 'claimed', revision = 2, claimed_at_ms = ?2,
             updated_at_ms = ?2 WHERE execution_id = ?1 AND state = 'planned'
             AND broker_state = 'planned' AND revision = 1",
            params![claim.execution_id.as_str(), now],
        )?;
        if changed != 1 || claimed != 1 {
            return Err(conflict(
                "permit consumption or execution claim lost its precondition",
            ));
        }
        let record = load_execution(&transaction, &claim.execution_id)?;
        insert_receipt(&transaction, command, "claim_execution", &record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }

    /// Persists a security-boundary audit proof before exposing a claimed effect target.
    pub fn start_claimed_execution(
        &mut self,
        command: &LedgerCommand,
        execution_id: &ExecutionId,
        expected_revision: u64,
        proof: &SecurityAuditProof,
    ) -> Result<LedgerOutcome<ExecutionRecord>, StoreError> {
        validate_command(command)?;
        integer(expected_revision, "claimed execution revision")?;
        integer(proof.persisted_at_ms, "security audit proof timestamp")?;
        if proof.persisted_at_ms > command.committed_at_ms {
            return Err(conflict("security audit proof timestamp is in the future"));
        }
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "start_claimed_execution")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let record = load_execution(&transaction, execution_id)?;
        require_task_run(
            &transaction,
            &record.task_id,
            &record.run_id,
            &command.actor_id,
        )?;
        let fence = record
            .runtime_fence
            .as_ref()
            .ok_or_else(|| corrupt("claimed execution is missing its Runtime fence"))?;
        require_runtime_fence(
            &transaction,
            &command.actor_id,
            &record.task_id,
            &record.run_id,
            fence,
            command.committed_at_ms,
        )?;
        if record.actor_id != command.actor_id
            || record.state != ExecutionState::Planned
            || record.broker_state != Some(BrokerExecutionState::Claimed)
            || record.revision != expected_revision
        {
            return Err(conflict(
                "execution is not claimed at the expected revision",
            ));
        }
        require_not_before(
            command.committed_at_ms,
            record.updated_at_ms,
            "execution start",
        )?;
        transaction.execute(
            "INSERT INTO security_audit_proofs(
                 execution_id, proof_digest, durability, persisted_at_ms)
             VALUES (?1, ?2, 'security_boundary', ?3)",
            params![
                execution_id.as_str(),
                proof.proof_digest.as_str(),
                integer(proof.persisted_at_ms, "security audit proof timestamp")?,
            ],
        )?;
        let next_revision = next_integer(record.revision, "execution revision")?;
        let now = integer(command.committed_at_ms, "execution start timestamp")?;
        let changed = transaction.execute(
            "UPDATE executions SET state='started', broker_state='started', revision=?2,
                 started_at_ms=?3, start_audit_proof_digest=?4, updated_at_ms=?3
             WHERE execution_id=?1 AND state='planned' AND broker_state='claimed'
                 AND revision=?5",
            params![
                execution_id.as_str(),
                integer(next_revision, "execution revision")?,
                now,
                proof.proof_digest.as_str(),
                integer(expected_revision, "claimed execution revision")?,
            ],
        )?;
        if changed != 1 {
            return Err(conflict(
                "execution start lost its claimed-state precondition",
            ));
        }
        let started = load_execution(&transaction, execution_id)?;
        insert_receipt(&transaction, command, "start_claimed_execution", &started)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(started))
    }

    /// Commits a conclusive receipt for a started execution.
    pub fn complete_execution(
        &mut self,
        command: &LedgerCommand,
        completion: &ExecutionCompletion,
    ) -> Result<LedgerOutcome<ExecutionRecord>, StoreError> {
        validate_command(command)?;
        integer(completion.expected_revision, "execution expected revision")?;
        if completion.succeeded != completion.typed_result.is_some() {
            return Err(conflict(
                "a successful execution requires exactly one typed result",
            ));
        }
        let transaction = immediate(self)?;
        if let Some(replayed) =
            replay::<ExecutionRecord>(&transaction, command, "complete_execution")?
        {
            let expected_state = if completion.succeeded {
                ExecutionState::Succeeded
            } else {
                ExecutionState::Failed
            };
            let receipt = transaction
                .query_row(
                    "SELECT receipt_digest, safe_detail FROM execution_receipts
                     WHERE execution_id=?1",
                    params![completion.execution_id.as_str()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()?;
            let result_matches = match completion.typed_result.as_ref() {
                Some(expected) => {
                    load_brokered_execution_result(&transaction, &completion.execution_id)?.result
                        == *expected
                }
                None => replayed.typed_result_state == TypedExecutionResultState::NotApplicable,
            };
            if replayed.execution_id != completion.execution_id
                || replayed.state != expected_state
                || receipt
                    != Some((
                        completion.receipt_digest.as_str().to_owned(),
                        completion
                            .safe_detail
                            .as_ref()
                            .map(|detail| detail.as_str().to_owned()),
                    ))
                || !result_matches
            {
                return Err(StoreError::IdempotencyConflict);
            }
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let mut record = load_execution(&transaction, &completion.execution_id)?;
        require_task_owner(&transaction, &record.task_id, &command.actor_id)?;
        if record.actor_id != command.actor_id
            || record.state != ExecutionState::Started
            || record.broker_state != Some(BrokerExecutionState::Started)
            || record.start_audit_proof_digest.is_none()
            || record.revision != completion.expected_revision
        {
            return Err(conflict(
                "execution actor, state, or revision does not match",
            ));
        }
        require_not_before(
            command.committed_at_ms,
            record.updated_at_ms,
            "execution completion",
        )?;
        let typed_result = completion
            .typed_result
            .as_ref()
            .map(|result| validate_completion_result(&transaction, &record, result, command))
            .transpose()?;
        let next_revision = next_integer(record.revision, "execution revision")?;
        record.state = if completion.succeeded {
            ExecutionState::Succeeded
        } else {
            ExecutionState::Failed
        };
        record.revision = next_revision;
        record.typed_result_state = if completion.succeeded {
            TypedExecutionResultState::Available
        } else {
            TypedExecutionResultState::NotApplicable
        };
        record.completed_at_ms = Some(command.committed_at_ms);
        record.updated_at_ms = command.committed_at_ms;
        let state = state_name(record.state)?;
        let now = integer(command.committed_at_ms, "execution completion timestamp")?;
        let changed = transaction.execute(
            "UPDATE executions SET state = ?2, revision = ?3, completed_at_ms = ?4,
             updated_at_ms = ?4, typed_result_state = ?6
             WHERE execution_id = ?1 AND state = 'started' AND revision = ?5",
            params![
                completion.execution_id.as_str(),
                state,
                integer(record.revision, "execution revision")?,
                now,
                integer(completion.expected_revision, "execution expected revision")?,
                state_name(record.typed_result_state)?,
            ],
        )?;
        if changed != 1 {
            return Err(conflict("execution completion lost its started revision"));
        }
        transaction.execute(
            "INSERT INTO execution_receipts(execution_id, state, receipt_digest, safe_detail,
             committed_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                completion.execution_id.as_str(),
                state,
                completion.receipt_digest.as_str(),
                completion.safe_detail.as_ref().map(BoundedText::as_str),
                now
            ],
        )?;
        if let Some(result) = typed_result {
            insert_brokered_execution_result(&transaction, &result)?;
        }
        let outcome = if completion.succeeded {
            ExecutionOutcome::Succeeded {
                evidence_ref: Some(
                    BoundedOpaque::new(completion.receipt_digest.as_str())
                        .map_err(|_| corrupt("receipt digest cannot form an evidence reference"))?,
                ),
            }
        } else {
            ExecutionOutcome::Failed {
                error: ContractError::new(
                    "brokered_execution_failed",
                    ErrorCategory::Internal,
                    false,
                    completion
                        .safe_detail
                        .as_ref()
                        .map_or("Brokered execution failed", BoundedText::as_str),
                )
                .map_err(|_| corrupt("static brokered execution error is invalid"))?,
            }
        };
        append_internal_task_event(
            &transaction,
            &record.task_id,
            &record.actor_id,
            command.committed_at_ms,
            TaskEvent::ExecutionResultRecorded {
                execution_id: record.execution_id.clone(),
                outcome,
            },
            None,
        )?;
        insert_receipt(&transaction, command, "complete_execution", &record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }

    /// Concludes a claimed execution when the pre-effect audit barrier fails.
    ///
    /// This transition is valid only before any start proof exists. It records
    /// a failed Task result in the same transaction, proving the external target
    /// never received control.
    pub fn mark_claimed_execution_known_no_effect(
        &mut self,
        command: &LedgerCommand,
        execution_id: &ExecutionId,
        expected_revision: u64,
        safe_detail: &BoundedText,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<ExecutionRecord>, StoreError> {
        validate_command(command)?;
        integer(expected_revision, "execution expected revision")?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay::<ExecutionRecord>(
            &transaction,
            command,
            "mark_claimed_execution_known_no_effect",
        )? {
            require_execution_runtime_context(&transaction, command, &replayed, lease)?;
            if replayed.execution_id != *execution_id
                || replayed.state != ExecutionState::Planned
                || replayed.broker_state != Some(BrokerExecutionState::KnownNoEffect)
            {
                return Err(StoreError::IdempotencyConflict);
            }
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let mut record = load_execution(&transaction, execution_id)?;
        require_execution_runtime_context(&transaction, command, &record, lease)?;
        if record.state != ExecutionState::Planned
            || record.broker_state != Some(BrokerExecutionState::Claimed)
            || record.claimed_at_ms.is_none()
            || record.start_audit_proof_digest.is_some()
            || record.revision != expected_revision
        {
            return Err(conflict(
                "execution is not a proof-free claim at the expected revision",
            ));
        }
        require_not_before(
            command.committed_at_ms,
            record.updated_at_ms,
            "known-no-effect execution completion",
        )?;
        record.broker_state = Some(BrokerExecutionState::KnownNoEffect);
        record.revision = next_integer(record.revision, "execution revision")?;
        record.updated_at_ms = command.committed_at_ms;
        let changed = transaction.execute(
            "UPDATE executions SET broker_state='known_no_effect', revision=?2,
                 updated_at_ms=?3
             WHERE execution_id=?1 AND state='planned' AND broker_state='claimed'
                 AND start_audit_proof_digest IS NULL AND revision=?4",
            params![
                execution_id.as_str(),
                integer(record.revision, "execution revision")?,
                integer(command.committed_at_ms, "known-no-effect timestamp")?,
                integer(expected_revision, "execution expected revision")?,
            ],
        )?;
        if changed != 1 {
            return Err(conflict(
                "known-no-effect transition lost its claimed revision",
            ));
        }
        append_internal_task_event(
            &transaction,
            &record.task_id,
            &record.actor_id,
            command.committed_at_ms,
            TaskEvent::ExecutionResultRecorded {
                execution_id: record.execution_id.clone(),
                outcome: ExecutionOutcome::Failed {
                    error: ContractError::new(
                        "security_audit_failed_before_effect",
                        ErrorCategory::Storage,
                        false,
                        safe_detail.as_str(),
                    )
                    .map_err(|_| conflict("audit failure detail is not a valid Task error"))?,
                },
            },
            None,
        )?;
        insert_receipt(
            &transaction,
            command,
            "mark_claimed_execution_known_no_effect",
            &record,
        )?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }

    /// Marks a live target response indeterminate without waiting for restart recovery.
    pub fn mark_execution_uncertain(
        &mut self,
        command: &LedgerCommand,
        execution_id: &ExecutionId,
        expected_revision: u64,
        _safe_detail: &BoundedText,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<ExecutionRecord>, StoreError> {
        validate_command(command)?;
        integer(expected_revision, "execution expected revision")?;
        let transaction = immediate(self)?;
        if let Some(replayed) =
            replay::<ExecutionRecord>(&transaction, command, "mark_execution_uncertain")?
        {
            require_execution_runtime_context(&transaction, command, &replayed, lease)?;
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let mut record = load_execution(&transaction, execution_id)?;
        require_execution_runtime_context(&transaction, command, &record, lease)?;
        if record.actor_id != command.actor_id
            || record.state != ExecutionState::Started
            || record.broker_state != Some(BrokerExecutionState::Started)
            || record.start_audit_proof_digest.is_none()
            || record.revision != expected_revision
        {
            return Err(conflict(
                "uncertain execution actor, authority, state, or revision does not match",
            ));
        }
        require_not_before(
            command.committed_at_ms,
            record.updated_at_ms,
            "execution uncertainty",
        )?;
        record.state = ExecutionState::Uncertain;
        record.revision = next_integer(record.revision, "execution revision")?;
        record.completed_at_ms = Some(command.committed_at_ms);
        record.updated_at_ms = command.committed_at_ms;
        let changed = transaction.execute(
            "UPDATE executions SET state='uncertain', revision=?2, completed_at_ms=?3,
             updated_at_ms=?3 WHERE execution_id=?1 AND state='started'
             AND broker_state='started' AND revision=?4",
            params![
                execution_id.as_str(),
                integer(record.revision, "execution revision")?,
                integer(command.committed_at_ms, "execution uncertainty timestamp")?,
                integer(expected_revision, "execution expected revision")?,
            ],
        )?;
        if changed != 1 {
            return Err(conflict("execution uncertainty lost its started revision"));
        }
        append_internal_task_event(
            &transaction,
            &record.task_id,
            &record.actor_id,
            command.committed_at_ms,
            TaskEvent::ExecutionUncertain {
                execution_id: record.execution_id.clone(),
                reason: UncertaintyCode::TransportLost,
            },
            None,
        )?;
        insert_receipt(&transaction, command, "mark_execution_uncertain", &record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }
}
