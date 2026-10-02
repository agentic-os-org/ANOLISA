impl SqliteTaskStore {
    /// Loads started effects eligible for exact read-only reconciliation.
    ///
    /// The supplied lease must be the current unexpired takeover generation.
    /// Returned requests retain the provider binding captured before approval.
    pub fn load_started_brokered_recovery_candidates(
        &self,
        lease: &LeaseClaim,
        now_ms: u64,
    ) -> Result<Vec<BrokeredExecutionRecoveryCandidate>, StoreError> {
        let current = load_run_lease_optional(self.connection(), &lease.run_id)?
            .ok_or_else(|| not_found("run lease", lease.run_id.as_str()))?;
        if current.task_id != lease.task_id
            || current.run_id != lease.run_id
            || current.lease_owner != lease.lease_owner
            || current.generation != lease.generation
            || current.revision != lease.revision
            || current.expires_at_ms <= now_ms
        {
            return Err(conflict(
                "brokered evidence recovery requires the current unexpired takeover lease",
            ));
        }
        let executions = load_brokered_recovery_candidates_for_run(
            self.connection(),
            &lease.run_id,
            "started",
            ExecutionState::Started,
            lease.generation,
        )?;
        executions
            .into_iter()
            .map(|execution| {
                let request_id = self
                    .connection()
                    .query_row(
                        "SELECT request_id FROM permits WHERE execution_id=?1",
                        params![execution.execution_id.as_str()],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        not_found("execution permit", execution.execution_id.as_str())
                    })?;
                let request_id = RequestId::parse(request_id)
                    .map_err(|_| corrupt("invalid recovery request identity"))?;
                let request = load_brokered_request(self.connection(), &request_id)?;
                if execution.actor_id != request.request.actor.actor_id
                    || execution.task_id != request.request.task_id
                    || execution.run_id != request.request.run_id
                    || execution.target != request.request.target
                    || execution.target_identity_digest.as_ref()
                        != Some(&request.target_identity_digest)
                    || execution.runtime_fence.as_ref() != Some(&request.runtime_fence)
                    || execution.operation_digest != request.request.operation_digest
                    || execution.input_digest != request.request.input_digest
                {
                    return Err(corrupt(
                        "started recovery execution does not match its brokered request",
                    ));
                }
                Ok(BrokeredExecutionRecoveryCandidate { execution, request })
            })
            .collect()
    }

    /// Recovers stale brokered executions after a Run-lease generation takeover.
    ///
    /// The current unexpired lease must have a newer generation than every
    /// recovered execution. Repeated calls are read-only because only exact
    /// `Claimed` and `Started` states are eligible.
    pub fn recover_brokered_executions_for_run(
        &mut self,
        run_id: &RunId,
        now_ms: u64,
    ) -> Result<BrokeredExecutionRecoveryReport, StoreError> {
        let transaction = immediate(self)?;
        let current = load_run_lease_optional(&transaction, run_id)?
            .ok_or_else(|| not_found("run lease", run_id.as_str()))?;
        if current.expires_at_ms <= now_ms {
            return Err(conflict(
                "brokered execution recovery requires an unexpired takeover lease",
            ));
        }
        require_task_run(
            &transaction,
            &current.task_id,
            &current.run_id,
            &current.actor_id,
        )?;
        let claimed = load_brokered_recovery_candidates_for_run(
            &transaction,
            run_id,
            "claimed",
            ExecutionState::Planned,
            current.generation,
        )?;
        let started = load_brokered_recovery_candidates_for_run(
            &transaction,
            run_id,
            "started",
            ExecutionState::Started,
            current.generation,
        )?;
        let report = apply_brokered_execution_recovery(&transaction, &claimed, &started, now_ms)?;
        transaction.commit()?;
        Ok(report)
    }

    /// Recovers durable state conservatively without retrying side effects.
    pub fn recover_gateway(&mut self, now_ms: u64) -> Result<RecoveryReport, StoreError> {
        let transaction = immediate(self)?;
        let now = integer(now_ms, "recovery timestamp")?;
        validate_all_execution_receipts(&transaction)?;
        let claimed =
            load_brokered_recovery_candidates(&transaction, "claimed", ExecutionState::Planned)?;
        let started =
            load_brokered_recovery_candidates(&transaction, "started", ExecutionState::Started)?;
        let execution_recovery =
            apply_brokered_execution_recovery(&transaction, &claimed, &started, now_ms)?;
        let (runtime_input_requests_cancelled, runtime_input_dispatches_unknown) =
            recover_runtime_inputs_after_restart(&transaction, now_ms)?;
        let approvals_expired = transaction.execute(
            "UPDATE approvals SET state='expired', revision=revision+1, updated_at_ms=?1
             WHERE state='pending' AND expires_at_ms <= ?1",
            params![now],
        )?;
        let approvals_cancelled = transaction.execute(
            "UPDATE approvals SET state='cancelled', revision=revision+1, updated_at_ms=?1
             WHERE state='pending'",
            params![now],
        )?;
        let permission_dispatches_abandoned = transaction.execute(
            "UPDATE provider_permission_dispatches
             SET state='abandoned', revision=revision+1, updated_at_ms=?1
             WHERE state='prepared'
               AND EXISTS (
                   SELECT 1 FROM tasks
                   WHERE tasks.task_id=provider_permission_dispatches.task_id
                     AND tasks.state IN (
                         'running', 'waiting_approval', 'waiting_input', 'suspended'
                     )
               )",
            params![now],
        )?;
        let permission_dispatches_unknown = transaction.execute(
            "UPDATE provider_permission_dispatches
             SET state='unknown', revision=revision+1, updated_at_ms=?1
             WHERE state IN ('write_started', 'written')
               AND EXISTS (
                   SELECT 1 FROM tasks
                   WHERE tasks.task_id=provider_permission_dispatches.task_id
                     AND tasks.state IN (
                         'running', 'waiting_approval', 'waiting_input', 'suspended'
                     )
               )",
            params![now],
        )?;
        let brokered_dispatches_unknown = transaction.execute(
            "UPDATE brokered_runtime_dispatches
             SET state='unknown', revision=revision+1, updated_at_ms=?1
             WHERE state='started'",
            params![now],
        )?;
        let permits_expired = transaction.execute(
            "UPDATE permits SET state='expired' WHERE state='issued' AND valid_until_ms <= ?1",
            params![now],
        )?;
        let legacy_executions_uncertain = transaction.execute(
            "UPDATE executions SET state='uncertain', revision=revision+1, completed_at_ms=?1,
             updated_at_ms=?1 WHERE state='started' AND broker_state IS NULL",
            params![now],
        )?;
        let runtime_bindings_lost = transaction.execute(
            "UPDATE runtime_bindings SET state='lost', updated_at_ms=?1 WHERE state='active'",
            params![now],
        )?;
        transaction.commit()?;
        Ok(RecoveryReport {
            approvals_expired: approvals_expired as u64,
            approvals_cancelled: approvals_cancelled as u64,
            permission_dispatches_abandoned: permission_dispatches_abandoned as u64,
            permission_dispatches_unknown: permission_dispatches_unknown as u64,
            brokered_dispatches_unknown: brokered_dispatches_unknown as u64,
            runtime_input_requests_cancelled,
            runtime_input_dispatches_unknown,
            permits_expired: permits_expired as u64,
            executions_uncertain: execution_recovery.executions_uncertain
                + legacy_executions_uncertain as u64,
            executions_known_no_effect: execution_recovery.executions_known_no_effect,
            runtime_bindings_lost: runtime_bindings_lost as u64,
        })
    }
}
