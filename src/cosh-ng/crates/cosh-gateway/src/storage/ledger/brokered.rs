impl SqliteTaskStore {

    /// Loads and validates one typed COSH-brokered request.
    pub fn load_brokered_request(
        &self,
        request_id: &RequestId,
    ) -> Result<BrokeredRequestRecord, StoreError> {
        load_brokered_request(self.connection(), request_id)
    }

    /// Loads one durable brokered callback without requiring a live Runtime lease.
    ///
    /// This recovery-oriented read validates the stored row's typed encoding and
    /// primary-key columns only. Callers must still compare actor, Task, Run,
    /// callback reference, payload digest, and source with their request context
    /// before deciding whether a lost API response can be replayed safely.
    pub fn load_brokered_runtime_dispatch_record(
        &self,
        request_id: &RequestId,
        kind: BrokeredRuntimeDispatchKind,
    ) -> Result<BrokeredRuntimeDispatchRecord, StoreError> {
        load_brokered_runtime_dispatch(self.connection(), request_id, kind)
    }

    /// Loads an exact brokered callback only while its Runtime and lease remain authoritative.
    pub fn load_brokered_runtime_dispatch(
        &self,
        actor_id: &ActorId,
        kind: BrokeredRuntimeDispatchKind,
        brokered: &BrokeredExecutionRef,
        payload_digest: &Digest,
        lease: &LeaseClaim,
        now_ms: u64,
    ) -> Result<BrokeredRuntimeDispatchRecord, StoreError> {
        let record = load_brokered_runtime_dispatch(self.connection(), &brokered.request_id, kind)?;
        require_brokered_dispatch_context(
            self.connection(),
            actor_id,
            &record,
            brokered,
            payload_digest,
            lease,
            now_ms,
        )?;
        Ok(record)
    }

    /// Prepares an acknowledgement after request, approval, and WaitingApproval are durable.
    pub fn prepare_brokered_acknowledgement_dispatch(
        &mut self,
        command: &LedgerCommand,
        approval_id: &ApprovalId,
        brokered: &BrokeredExecutionRef,
        payload_digest: &Digest,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<BrokeredRuntimeDispatchRecord>, StoreError> {
        prepare_brokered_runtime_dispatch(
            self,
            command,
            BrokeredRuntimeDispatchKind::Acknowledgement,
            BrokeredRuntimeDispatchSource::ApprovalPending {
                approval_id: approval_id.clone(),
            },
            brokered,
            Some(payload_digest),
            None,
            lease,
        )
    }

    /// Prepares a typed denial only after the bound approval denial is durable.
    ///
    /// Canonicalizes the complete delivery inside the write transaction and
    /// rejects request identity or denial-classification substitution.
    pub fn prepare_brokered_denied_result_dispatch(
        &mut self,
        command: &LedgerCommand,
        approval_id: &ApprovalId,
        brokered: &BrokeredExecutionRef,
        delivery: &BrokeredExecutionDelivery,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<BrokeredRuntimeDispatchRecord>, StoreError> {
        prepare_brokered_runtime_dispatch(
            self,
            command,
            BrokeredRuntimeDispatchKind::Result,
            BrokeredRuntimeDispatchSource::ApprovalDenied {
                approval_id: approval_id.clone(),
            },
            brokered,
            None,
            Some(delivery),
            lease,
        )
    }

    /// Prepares a typed result only after its exact durable execution outcome.
    ///
    /// Canonicalizes the complete delivery inside the write transaction. A
    /// successful delivery must equal the result persisted by
    /// [`Self::complete_execution`]; failure and uncertainty variants must
    /// match the durable execution lifecycle.
    pub fn prepare_brokered_execution_result_dispatch(
        &mut self,
        command: &LedgerCommand,
        execution_id: &ExecutionId,
        brokered: &BrokeredExecutionRef,
        delivery: &BrokeredExecutionDelivery,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<BrokeredRuntimeDispatchRecord>, StoreError> {
        prepare_brokered_runtime_dispatch(
            self,
            command,
            BrokeredRuntimeDispatchKind::Result,
            BrokeredRuntimeDispatchSource::Execution {
                execution_id: execution_id.clone(),
            },
            brokered,
            None,
            Some(delivery),
            lease,
        )
    }

    /// Commits the non-replayable boundary before writing a brokered callback.
    ///
    /// Callers may write to the Runtime only for [`LedgerOutcome::Applied`].
    /// `Replayed`, `Started`, and `Unknown` must never cause another write.
    pub fn start_brokered_runtime_dispatch(
        &mut self,
        command: &LedgerCommand,
        kind: BrokeredRuntimeDispatchKind,
        brokered: &BrokeredExecutionRef,
        payload_digest: &Digest,
        expected_revision: u64,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<BrokeredRuntimeDispatchRecord>, StoreError> {
        transition_brokered_runtime_dispatch(
            self,
            command,
            kind,
            brokered,
            payload_digest,
            expected_revision,
            lease,
            BrokeredRuntimeDispatchState::Prepared,
            BrokeredRuntimeDispatchState::Started,
            "start_brokered_runtime_dispatch",
        )
    }

    /// Records that the live Runtime accepted a previously started callback.
    pub fn complete_brokered_runtime_dispatch(
        &mut self,
        command: &LedgerCommand,
        kind: BrokeredRuntimeDispatchKind,
        brokered: &BrokeredExecutionRef,
        payload_digest: &Digest,
        expected_revision: u64,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<BrokeredRuntimeDispatchRecord>, StoreError> {
        transition_brokered_runtime_dispatch(
            self,
            command,
            kind,
            brokered,
            payload_digest,
            expected_revision,
            lease,
            BrokeredRuntimeDispatchState::Started,
            BrokeredRuntimeDispatchState::Delivered,
            "complete_brokered_runtime_dispatch",
        )
    }

    /// Marks a started callback indeterminate after a live transport failure.
    pub fn mark_brokered_runtime_dispatch_unknown(
        &mut self,
        command: &LedgerCommand,
        kind: BrokeredRuntimeDispatchKind,
        brokered: &BrokeredExecutionRef,
        payload_digest: &Digest,
        expected_revision: u64,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<BrokeredRuntimeDispatchRecord>, StoreError> {
        transition_brokered_runtime_dispatch(
            self,
            command,
            kind,
            brokered,
            payload_digest,
            expected_revision,
            lease,
            BrokeredRuntimeDispatchState::Started,
            BrokeredRuntimeDispatchState::Unknown,
            "mark_brokered_runtime_dispatch_unknown",
        )
    }

    /// Records a typed brokered request that policy may permit without approval.
    pub fn create_brokered_request(
        &mut self,
        command: &LedgerCommand,
        request: &CapabilityRequest,
        operation: &BrokeredOperation,
        target_identity_digest: &Digest,
        runtime_fence: &RuntimeExecutionFence,
        provider_binding: Option<&BoundedOpaque>,
    ) -> Result<LedgerOutcome<BrokeredRequestRecord>, StoreError> {
        validate_command(command)?;
        if request.actor.actor_id != command.actor_id
            || request.expires_at_ms <= command.committed_at_ms
        {
            return Err(conflict("brokered request actor or deadline is invalid"));
        }
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "create_brokered_request")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        require_task_run(
            &transaction,
            &request.task_id,
            &request.run_id,
            &command.actor_id,
        )?;
        require_runtime_fence(
            &transaction,
            &command.actor_id,
            &request.task_id,
            &request.run_id,
            runtime_fence,
            command.committed_at_ms,
        )?;
        let record = BrokeredRequestRecord {
            request: request.clone(),
            operation: operation.clone(),
            typed_operation_digest: brokered_operation_digest(operation)?,
            target_identity_digest: target_identity_digest.clone(),
            runtime_fence: runtime_fence.clone(),
            provider_binding: provider_binding.cloned(),
            approval_id: None,
            created_at_ms: command.committed_at_ms,
        };
        insert_brokered_request(&transaction, &record)?;
        insert_receipt(&transaction, command, "create_brokered_request", &record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }

    /// Atomically records a typed COSH-brokered request, approval, Task fact, and Outbox intent.
    pub fn create_brokered_approval(
        &mut self,
        command: &LedgerCommand,
        request: &CapabilityRequest,
        approval: &cosh_gateway_contracts::capability::ApprovalRequest,
        operation: &BrokeredOperation,
        provider_binding: Option<&BoundedOpaque>,
        record: &ApprovalRecord,
    ) -> Result<LedgerOutcome<ApprovalRecord>, StoreError> {
        validate_command(command)?;
        integer(record.expires_at_ms, "approval deadline")?;
        let target_identity_digest = record
            .target_identity_digest
            .as_ref()
            .ok_or_else(|| conflict("brokered approval is missing target identity"))?;
        let runtime_fence = record
            .runtime_fence
            .as_ref()
            .ok_or_else(|| conflict("brokered approval is missing Runtime fence"))?;
        if request.request_id != record.request_id
            || request.actor.actor_id != record.actor_id
            || request.task_id != record.task_id
            || request.run_id != record.run_id
            || request.target != record.target
            || request.operation_digest != record.operation_digest
            || request.input_digest != record.input_digest
            || approval.approval_id != record.approval_id
            || approval.request_id != request.request_id
        {
            return Err(conflict("brokered request, approval, and authority differ"));
        }
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "create_brokered_approval")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        require_task_run(
            &transaction,
            &record.task_id,
            &record.run_id,
            &command.actor_id,
        )?;
        validate_initial_approval(command, record)?;
        require_runtime_fence(
            &transaction,
            &command.actor_id,
            &record.task_id,
            &record.run_id,
            runtime_fence,
            command.committed_at_ms,
        )?;
        insert_approval(&transaction, record, command.committed_at_ms)?;
        insert_brokered_request(
            &transaction,
            &BrokeredRequestRecord {
                request: request.clone(),
                operation: operation.clone(),
                typed_operation_digest: brokered_operation_digest(operation)?,
                target_identity_digest: target_identity_digest.clone(),
            runtime_fence: runtime_fence.clone(),
            provider_binding: provider_binding.cloned(),
                approval_id: Some(approval.approval_id.clone()),
                created_at_ms: command.committed_at_ms,
            },
        )?;
        let delivery_kind = BoundedName::new("brokered_approval_request")
            .map_err(|_| corrupt("static brokered approval route is invalid"))?;
        append_internal_task_event(
            &transaction,
            &record.task_id,
            &record.actor_id,
            command.committed_at_ms,
            TaskEvent::ApprovalRequested {
                approval: approval.clone(),
            },
            Some((delivery_kind, serde_json::to_value(approval)?)),
        )?;
        insert_receipt(&transaction, command, "create_brokered_approval", record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record.clone()))
    }
}
