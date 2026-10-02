impl SqliteTaskStore {

    /// Persists a single-use permit and its planned execution atomically.
    pub fn issue_permit(
        &mut self,
        command: &LedgerCommand,
        permit: &ExecutionPermit,
    ) -> Result<LedgerOutcome<PermitRecord>, StoreError> {
        validate_command(command)?;
        integer(permit.policy_revision, "policy revision")?;
        integer(permit.valid_until_ms, "permit deadline")?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "issue_permit")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        require_task_run(
            &transaction,
            &permit.task_id,
            &permit.run_id,
            &command.actor_id,
        )?;
        require_runtime_fence(
            &transaction,
            &command.actor_id,
            &permit.task_id,
            &permit.run_id,
            &permit.runtime_fence,
            command.committed_at_ms,
        )?;
        if permit.actor_id != command.actor_id
            || !permit.single_use
            || permit.valid_until_ms <= command.committed_at_ms
        {
            return Err(conflict(
                "permit actor, single-use flag, or deadline is invalid",
            ));
        }
        let brokered = load_brokered_request(&transaction, &permit.request_id)?;
        if brokered.request.actor.actor_id != permit.actor_id
            || brokered.request.task_id != permit.task_id
            || brokered.request.run_id != permit.run_id
            || brokered.request.target != permit.target
            || brokered.request.operation_digest != permit.operation_digest
            || brokered.request.input_digest != permit.input_digest
            || brokered.target_identity_digest != permit.target_identity_digest
            || brokered.runtime_fence != permit.runtime_fence
            || brokered.approval_id != permit.approval_id
        {
            return Err(conflict(
                "typed brokered request does not exactly bind the permit",
            ));
        }
        if let Some(approval_id) = &permit.approval_id {
            let approval = load_approval(&transaction, approval_id)?;
            if approval.state != ApprovalState::Approved
                || approval.request_id != permit.request_id
                || approval.actor_id != permit.actor_id
                || approval.task_id != permit.task_id
                || approval.run_id != permit.run_id
                || approval.target != permit.target
                || approval.target_identity_digest.as_ref() != Some(&permit.target_identity_digest)
                || approval.runtime_fence.as_ref() != Some(&permit.runtime_fence)
                || approval.operation_digest != permit.operation_digest
                || approval.input_digest != permit.input_digest
                || approval.expires_at_ms <= command.committed_at_ms
                || permit.valid_until_ms > approval.expires_at_ms
                || command.committed_at_ms < approval.updated_at_ms
            {
                return Err(conflict(
                    "approved request does not exactly bind the permit",
                ));
            }
        }
        let active_brokered: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM executions
                 WHERE task_id=?1 AND run_id=?2
                   AND broker_state IN ('planned', 'claimed', 'started')
                   AND state IN ('planned', 'started')
             )",
            params![permit.task_id.as_str(), permit.run_id.as_str()],
            |row| row.get(0),
        )?;
        if active_brokered {
            return Err(conflict(
                "only one brokered execution may cross the effect boundary per Run",
            ));
        }
        let target_json = serde_json::to_string(&permit.target)?;
        let now = integer(command.committed_at_ms, "permit timestamp")?;
        transaction.execute(
            "INSERT INTO executions(execution_id, actor_id, task_id, run_id, target_json,
             operation_digest, input_digest, state, revision, created_at_ms, updated_at_ms,
             target_identity_digest, runtime_fence_json, broker_state, typed_result_state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'planned', 1, ?8, ?8, ?9, ?10, 'planned',
                     'not_applicable')",
            params![
                permit.execution_id.as_str(),
                permit.actor_id.as_str(),
                permit.task_id.as_str(),
                permit.run_id.as_str(),
                target_json,
                permit.operation_digest.as_str(),
                permit.input_digest.as_str(),
                now,
                permit.target_identity_digest.as_str(),
                serde_json::to_string(&permit.runtime_fence)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO permits(permit_id, request_id, approval_id, actor_id, task_id, run_id,
             execution_id, target_json, operation_digest, input_digest, policy_revision, state,
             single_use, valid_until_ms, created_at_ms, target_identity_digest, runtime_fence_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'issued', 1, ?12, ?13,
                     ?14, ?15)",
            params![
                permit.permit_id.as_str(),
                permit.request_id.as_str(),
                permit.approval_id.as_ref().map(ApprovalId::as_str),
                permit.actor_id.as_str(),
                permit.task_id.as_str(),
                permit.run_id.as_str(),
                permit.execution_id.as_str(),
                serde_json::to_string(&permit.target)?,
                permit.operation_digest.as_str(),
                permit.input_digest.as_str(),
                integer(permit.policy_revision, "policy revision")?,
                integer(permit.valid_until_ms, "permit deadline")?,
                now,
                permit.target_identity_digest.as_str(),
                serde_json::to_string(&permit.runtime_fence)?
            ],
        )?;
        let record = PermitRecord {
            permit: permit.clone(),
            state: PermitState::Issued,
            consumed_at_ms: None,
            created_at_ms: command.committed_at_ms,
        };
        append_internal_task_event(
            &transaction,
            &permit.task_id,
            &permit.actor_id,
            command.committed_at_ms,
            TaskEvent::ExecutionPlanned {
                execution_id: permit.execution_id.clone(),
                permit_id: permit.permit_id.clone(),
            },
            None,
        )?;
        insert_receipt(&transaction, command, "issue_permit", &record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }

    /// Revokes an issued permit before execution starts.
    pub fn revoke_permit(
        &mut self,
        command: &LedgerCommand,
        permit_id: &PermitId,
    ) -> Result<LedgerOutcome<PermitRecord>, StoreError> {
        validate_command(command)?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "revoke_permit")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let mut record = load_permit(&transaction, permit_id)?;
        require_task_owner(&transaction, &record.permit.task_id, &command.actor_id)?;
        require_not_before(
            command.committed_at_ms,
            record.created_at_ms,
            "permit revocation",
        )?;
        if record.permit.actor_id != command.actor_id || record.state != PermitState::Issued {
            return Err(conflict("only the bound actor may revoke an issued permit"));
        }
        let changed = transaction.execute(
            "UPDATE permits SET state='revoked' WHERE permit_id=?1 AND state='issued'",
            params![permit_id.as_str()],
        )?;
        if changed != 1 {
            return Err(conflict(
                "permit revocation lost its issued-state precondition",
            ));
        }
        record.state = PermitState::Revoked;
        insert_receipt(&transaction, command, "revoke_permit", &record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }
}
