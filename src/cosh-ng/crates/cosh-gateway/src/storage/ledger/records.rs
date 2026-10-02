impl SqliteTaskStore {
    /// Loads one durable approval record.
    pub fn load_approval_record(
        &self,
        approval_id: &ApprovalId,
    ) -> Result<ApprovalRecord, StoreError> {
        load_approval(self.connection(), approval_id)
    }

    /// Loads one durable provider-native permission dispatch.
    pub fn load_provider_permission_dispatch_record(
        &self,
        approval_id: &ApprovalId,
    ) -> Result<ProviderPermissionDispatchRecord, StoreError> {
        load_provider_permission_dispatch(self.connection(), approval_id)
    }

    /// Loads one durable permit record.
    pub fn load_permit_record(&self, permit_id: &PermitId) -> Result<PermitRecord, StoreError> {
        load_permit(self.connection(), permit_id)
    }

    /// Loads one durable execution record.
    pub fn load_execution_record(
        &self,
        execution_id: &ExecutionId,
    ) -> Result<ExecutionRecord, StoreError> {
        load_execution(self.connection(), execution_id)
    }

    /// Loads and fully validates one typed successful brokered result.
    pub fn load_brokered_execution_result(
        &self,
        execution_id: &ExecutionId,
    ) -> Result<BrokeredExecutionResultRecord, StoreError> {
        load_brokered_execution_result(self.connection(), execution_id)
    }

    /// Loads one durable runtime binding record.
    pub fn load_runtime_binding_record(
        &self,
        binding_id: &RuntimeBindingId,
    ) -> Result<RuntimeBindingRecord, StoreError> {
        load_runtime_binding(self.connection(), binding_id)
    }

    /// Loads the current durable lease for a Run.
    pub fn load_run_lease(&self, run_id: &RunId) -> Result<RunLeaseRecord, StoreError> {
        load_run_lease_optional(self.connection(), run_id)?
            .ok_or_else(|| not_found("run lease", run_id.as_str()))
    }

    /// Loads one expired lease whose delivered Runtime cannot be reattached.
    ///
    /// Only active Task states with a delivered `runtime_start` Outbox fact are
    /// eligible. A suspended Run remains recoverable until its lease is
    /// explicitly released, which is encoded by equal deadline and update
    /// timestamps.
    pub fn load_expired_active_lease(
        &self,
        now_ms: u64,
    ) -> Result<Option<RunLeaseRecord>, StoreError> {
        let now = integer(now_ms, "recovery timestamp")?;
        let row = self
            .connection()
            .query_row(
                "SELECT r.run_id, r.task_id, r.actor_id, r.lease_owner, r.generation,
                 r.revision, r.expires_at_ms, r.updated_at_ms
                 FROM run_leases r
                 JOIN tasks t ON t.task_id=r.task_id
                 WHERE r.expires_at_ms <= ?1
                   AND t.state IN ('running', 'waiting_approval', 'waiting_input', 'suspended')
                   AND json_extract(t.snapshot_json, '$.active_run_id') = r.run_id
                   AND (t.state != 'suspended' OR r.expires_at_ms > r.updated_at_ms)
                   AND EXISTS (
                       SELECT 1 FROM outbox o
                       WHERE o.task_id=r.task_id
                         AND o.delivery_kind='runtime_start'
                         AND o.state='delivered'
                         AND json_extract(o.payload_json, '$.run_id') = r.run_id
                   )
                 ORDER BY r.expires_at_ms, r.run_id
                 LIMIT 1",
                params![now],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )
            .optional()?;
        row.map(|row| {
            Ok(RunLeaseRecord {
                run_id: parse_id(&row.0)?,
                task_id: parse_id(&row.1)?,
                actor_id: parse_id(&row.2)?,
                lease_owner: BoundedOpaque::new(row.3)
                    .map_err(|_| corrupt("invalid lease owner"))?,
                generation: unsigned(row.4, "lease generation")?,
                revision: unsigned(row.5, "lease revision")?,
                expires_at_ms: unsigned(row.6, "lease deadline")?,
                updated_at_ms: unsigned(row.7, "lease update")?,
            })
        })
        .transpose()
    }

    /// Fences active Runtime bindings for one unrecoverable Run generation.
    pub fn mark_runtime_bindings_lost_for_run(
        &mut self,
        run_id: &RunId,
        now_ms: u64,
    ) -> Result<u64, StoreError> {
        let changed = self.connection_mut().execute(
            "UPDATE runtime_bindings SET state='lost', updated_at_ms=?2
             WHERE run_id=?1 AND state='active'",
            params![
                run_id.as_str(),
                integer(now_ms, "runtime recovery timestamp")?
            ],
        )?;
        Ok(changed as u64)
    }

    /// Cancels pending approvals only for one unrecoverable Run.
    pub fn cancel_pending_approvals_for_run(
        &mut self,
        run_id: &RunId,
        now_ms: u64,
    ) -> Result<u64, StoreError> {
        let changed = self.connection_mut().execute(
            "UPDATE approvals SET state='cancelled', revision=revision+1, updated_at_ms=?2
             WHERE run_id=?1 AND state='pending'",
            params![
                run_id.as_str(),
                integer(now_ms, "approval recovery timestamp")?
            ],
        )?;
        Ok(changed as u64)
    }

    /// Converges provider responses for one lost, non-reattachable Run.
    ///
    /// Prepared records prove that zero bytes were written and become
    /// abandoned. Writes that started or returned become unknown. Neither
    /// class can be replayed into a replacement provider session.
    pub fn mark_provider_dispatches_unknown_for_run(
        &mut self,
        run_id: &RunId,
        now_ms: u64,
    ) -> Result<u64, StoreError> {
        let transaction = immediate(self)?;
        let abandoned = transaction.execute(
            "UPDATE provider_permission_dispatches
             SET state='abandoned', revision=revision+1, updated_at_ms=?2
             WHERE run_id=?1 AND state='prepared'
               AND EXISTS (
                   SELECT 1 FROM tasks
                   WHERE tasks.task_id=provider_permission_dispatches.task_id
                     AND tasks.state IN (
                         'running', 'waiting_approval', 'waiting_input', 'suspended'
                     )
               )",
            params![
                run_id.as_str(),
                integer(now_ms, "dispatch recovery timestamp")?
            ],
        )?;
        let unknown = transaction.execute(
            "UPDATE provider_permission_dispatches
             SET state='unknown', revision=revision+1, updated_at_ms=?2
             WHERE run_id=?1 AND state IN ('write_started', 'written')
               AND EXISTS (
                   SELECT 1 FROM tasks
                   WHERE tasks.task_id=provider_permission_dispatches.task_id
                     AND tasks.state IN (
                         'running', 'waiting_approval', 'waiting_input', 'suspended'
                     )
               )",
            params![
                run_id.as_str(),
                integer(now_ms, "dispatch recovery timestamp")?
            ],
        )?;
        transaction.commit()?;
        Ok((abandoned + unknown) as u64)
    }

    /// Marks started brokered callbacks unknown only for one lost Run.
    ///
    /// Prepared callbacks have not crossed the transport boundary and remain
    /// distinguishable for operator-driven convergence. Started callbacks can
    /// never be retried because their write outcome is indeterminate.
    pub fn mark_brokered_dispatches_unknown_for_run(
        &mut self,
        run_id: &RunId,
        now_ms: u64,
    ) -> Result<u64, StoreError> {
        let changed = self.connection_mut().execute(
            "UPDATE brokered_runtime_dispatches
             SET state='unknown', revision=revision+1, updated_at_ms=?2
             WHERE run_id=?1 AND state='started'",
            params![
                run_id.as_str(),
                integer(now_ms, "brokered dispatch recovery timestamp")?
            ],
        )?;
        Ok(changed as u64)
    }
}
