impl SqliteTaskStore {
    /// Creates a pending approval bound to an actor, Task, Run, target, and digests.
    pub fn create_approval(
        &mut self,
        command: &LedgerCommand,
        approval: &ApprovalRecord,
    ) -> Result<LedgerOutcome<ApprovalRecord>, StoreError> {
        validate_command(command)?;
        integer(approval.expires_at_ms, "approval deadline")?;
        if approval.permission.is_some() {
            return Err(conflict(
                "provider approvals require a fenced Runtime and Run lease",
            ));
        }
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "create_approval")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        require_task_run(
            &transaction,
            &approval.task_id,
            &approval.run_id,
            &command.actor_id,
        )?;
        validate_initial_approval(command, approval)?;
        insert_approval(&transaction, approval, command.committed_at_ms)?;
        insert_receipt(&transaction, command, "create_approval", approval)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(approval.clone()))
    }

    /// Creates a provider-native approval only for a live fenced callback.
    pub fn create_provider_approval(
        &mut self,
        command: &LedgerCommand,
        approval_request: &ApprovalRequest,
        approval: &ApprovalRecord,
        binding: &RuntimeBindingRef,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<ApprovalRecord>, StoreError> {
        validate_command(command)?;
        integer(approval.expires_at_ms, "approval deadline")?;
        let permission = approval
            .permission
            .as_ref()
            .ok_or_else(|| conflict("provider approval is missing its Runtime permission"))?;
        if approval_request.approval_id != approval.approval_id
            || approval_request.request_id != approval.request_id
            || approval_request.task_id != approval.task_id
            || approval_request.run_id != approval.run_id
            || approval_request.expires_at_ms != approval.expires_at_ms
        {
            return Err(conflict(
                "provider approval request and durable record differ",
            ));
        }
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "create_provider_approval")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        validate_initial_approval(command, approval)?;
        validate_permission_binding(approval, permission)?;
        let callback_operation_digest = match (&permission.callback, &permission.core_callback) {
            (Some(callback), None) => &callback.normalized_operation_digest,
            (None, Some(callback)) => &callback.normalized_operation_digest,
            _ => {
                return Err(conflict(
                    "live Runtime Permission must have exactly one callback",
                ))
            }
        };
        if approval.actor_id != command.actor_id
            || approval.permission.as_ref() != Some(permission)
            || *callback_operation_digest != approval.operation_digest
            || lease.task_id != approval.task_id
            || lease.run_id != approval.run_id
            || lease.generation != permission.runtime_generation
            || binding.binding_id != permission.binding_id
            || binding.task_id != approval.task_id
            || binding.run_id != approval.run_id
            || binding.runtime_generation != permission.runtime_generation
        {
            return Err(conflict(
                "provider permission admission binding does not match",
            ));
        }
        require_current_lease(
            &transaction,
            lease,
            &command.actor_id,
            command.committed_at_ms,
        )?;
        let task = load_authoritative_task(&transaction, &approval.task_id)?;
        if task.state() != TaskState::Running || task.cancellation_requested() {
            return Err(conflict(
                "provider permission Task is not running for admission",
            ));
        }
        let runtime = load_runtime_binding(&transaction, &permission.binding_id)?;
        let expected_sequence = next_integer(runtime.last_sequence, "Runtime sequence")?;
        if runtime.binding != *binding
            || runtime.actor_id != command.actor_id
            || runtime.state != RuntimeBindingState::Active
            || permission.event_sequence != expected_sequence
        {
            return Err(conflict(
                "provider permission callback is not the next live Runtime event",
            ));
        }
        require_not_before(
            command.committed_at_ms,
            runtime.updated_at_ms,
            "provider permission admission",
        )?;
        let sequence_changed = transaction.execute(
            "UPDATE runtime_bindings SET last_sequence=?2, updated_at_ms=?3
             WHERE binding_id=?1 AND state='active' AND runtime_instance_id=?4
               AND runtime_generation=?5 AND last_sequence=?6",
            params![
                binding.binding_id.as_str(),
                integer(permission.event_sequence, "Runtime permission sequence")?,
                integer(command.committed_at_ms, "provider permission timestamp")?,
                binding.runtime_instance_id.as_str(),
                integer(binding.runtime_generation, "Runtime generation")?,
                integer(runtime.last_sequence, "Runtime prior sequence")?,
            ],
        )?;
        if sequence_changed != 1 {
            return Err(conflict(
                "provider permission admission lost its Runtime sequence precondition",
            ));
        }
        insert_approval(&transaction, approval, command.committed_at_ms)?;
        let delivery_kind = BoundedName::new("provider_approval_request")
            .map_err(|_| corrupt("static provider approval route is invalid"))?;
        append_internal_task_event(
            &transaction,
            &approval.task_id,
            &approval.actor_id,
            command.committed_at_ms,
            TaskEvent::ApprovalRequested {
                approval: approval_request.clone(),
            },
            Some((delivery_kind, serde_json::to_value(approval_request)?)),
        )?;
        insert_receipt(&transaction, command, "create_provider_approval", approval)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(approval.clone()))
    }

    /// Resolves a pending approval with revision, actor, and deadline checks.
    pub fn resolve_approval(
        &mut self,
        command: &LedgerCommand,
        approval_id: &ApprovalId,
        expected_revision: u64,
        resolution: ApprovalResolution,
    ) -> Result<LedgerOutcome<ApprovalRecord>, StoreError> {
        validate_command(command)?;
        integer(expected_revision, "approval expected revision")?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "resolve_approval")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let mut record = load_approval(&transaction, approval_id)?;
        require_task_owner(&transaction, &record.task_id, &command.actor_id)?;
        if record.actor_id != command.actor_id || record.revision != expected_revision {
            return Err(conflict("approval actor or revision does not match"));
        }
        require_not_before(
            command.committed_at_ms,
            record.updated_at_ms,
            "approval resolution",
        )?;
        if record.state != ApprovalState::Pending {
            return Err(conflict("approval is no longer pending"));
        }
        let (state, decided_by) = if command.committed_at_ms >= record.expires_at_ms {
            (ApprovalState::Expired, None)
        } else {
            match resolution {
                ApprovalResolution::Decide(ApprovalDecision::Approve) => {
                    (ApprovalState::Approved, Some(command.actor_id.clone()))
                }
                ApprovalResolution::Decide(ApprovalDecision::Deny) => {
                    (ApprovalState::Denied, Some(command.actor_id.clone()))
                }
                ApprovalResolution::Cancel => (ApprovalState::Cancelled, None),
            }
        };
        let next_revision = next_integer(record.revision, "approval revision")?;
        record.state = state;
        record.revision = next_revision;
        record.decided_by_actor_id = decided_by;
        record.updated_at_ms = command.committed_at_ms;
        let changed = transaction.execute(
            "UPDATE approvals SET state = ?2, revision = ?3, decided_by_actor_id = ?4,
             updated_at_ms = ?5 WHERE approval_id = ?1 AND revision = ?6 AND state = 'pending'",
            params![
                approval_id.as_str(),
                state_name(state)?,
                integer(record.revision, "approval revision")?,
                record.decided_by_actor_id.as_ref().map(ActorId::as_str),
                integer(command.committed_at_ms, "approval timestamp")?,
                integer(expected_revision, "approval expected revision")?
            ],
        )?;
        if changed != 1 {
            return Err(conflict("approval resolution lost its pending revision"));
        }
        if record.permission.is_none()
            && record.target_identity_digest.is_some()
            && record.runtime_fence.is_some()
        {
            if let ApprovalResolution::Decide(decision) = resolution {
                append_internal_task_event(
                    &transaction,
                    &record.task_id,
                    &record.actor_id,
                    command.committed_at_ms,
                    TaskEvent::ApprovalResolved {
                        approval_id: record.approval_id.clone(),
                        decision,
                    },
                    None,
                )?;
            }
        }
        insert_receipt(&transaction, command, "resolve_approval", &record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }

    /// Resolves a provider-native approval and prepares its exact response atomically.
    ///
    /// This path never issues an execution permit. The returned dispatch is
    /// observation-only authority and remains fenced to the active Runtime
    /// generation, Turn, tool, request, Run lease, and authenticated actor.
    pub fn resolve_provider_permission(
        &mut self,
        command: &LedgerCommand,
        approval_id: &ApprovalId,
        expected_revision: u64,
        resolution: ApprovalResolution,
        expected_permission: &RuntimePermissionRef,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<ProviderPermissionDispatchRecord>, StoreError> {
        validate_command(command)?;
        integer(expected_revision, "approval expected revision")?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "resolve_provider_permission")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }

        let mut approval = load_approval(&transaction, approval_id)?;
        require_provider_permission_context(
            &transaction,
            command,
            &approval,
            expected_permission,
            lease,
            TaskState::WaitingApproval,
        )?;
        if approval.revision != expected_revision || approval.state != ApprovalState::Pending {
            return Err(conflict(
                "provider approval is no longer at its pending revision",
            ));
        }
        require_not_before(
            command.committed_at_ms,
            approval.updated_at_ms,
            "provider approval resolution",
        )?;
        if command.committed_at_ms >= approval.expires_at_ms {
            return Err(conflict(
                "provider approval deadline elapsed; use fenced expiry",
            ));
        }

        let (approval_state, decided_by, decision, dispatch_decision) = match resolution {
            ApprovalResolution::Decide(ApprovalDecision::Approve) => (
                ApprovalState::Approved,
                Some(command.actor_id.clone()),
                ApprovalDecision::Approve,
                ProviderPermissionDispatchDecision::AllowOnce,
            ),
            ApprovalResolution::Decide(ApprovalDecision::Deny) => (
                ApprovalState::Denied,
                Some(command.actor_id.clone()),
                ApprovalDecision::Deny,
                ProviderPermissionDispatchDecision::Deny,
            ),
            ApprovalResolution::Cancel => {
                return Err(conflict(
                    "provider permission cancellation requires exact abandonment",
                ));
            }
        };
        approval.state = approval_state;
        approval.revision = next_integer(approval.revision, "approval revision")?;
        approval.decided_by_actor_id = decided_by;
        approval.updated_at_ms = command.committed_at_ms;
        let changed = transaction.execute(
            "UPDATE approvals SET state=?2, revision=?3, decided_by_actor_id=?4,
             updated_at_ms=?5 WHERE approval_id=?1 AND state='pending' AND revision=?6",
            params![
                approval_id.as_str(),
                state_name(approval_state)?,
                integer(approval.revision, "approval revision")?,
                approval.decided_by_actor_id.as_ref().map(ActorId::as_str),
                integer(command.committed_at_ms, "approval timestamp")?,
                integer(expected_revision, "approval expected revision")?,
            ],
        )?;
        if changed != 1 {
            return Err(conflict(
                "provider approval resolution lost its pending revision",
            ));
        }

        append_internal_task_event(
            &transaction,
            &approval.task_id,
            &approval.actor_id,
            command.committed_at_ms,
            TaskEvent::ApprovalResolved {
                approval_id: approval_id.clone(),
                decision,
            },
            None,
        )?;

        let dispatch = ProviderPermissionDispatchRecord {
            approval_id: approval_id.clone(),
            actor_id: approval.actor_id.clone(),
            task_id: approval.task_id.clone(),
            run_id: approval.run_id.clone(),
            permission: expected_permission.clone(),
            decision: dispatch_decision,
            state: ProviderPermissionDispatchState::Prepared,
            revision: 1,
            created_at_ms: command.committed_at_ms,
            updated_at_ms: command.committed_at_ms,
        };
        transaction.execute(
            "INSERT INTO provider_permission_dispatches(
                 approval_id, actor_id, task_id, run_id, permission_ref_json,
                 decision, state, revision, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'prepared', 1, ?7, ?7)",
            params![
                approval_id.as_str(),
                dispatch.actor_id.as_str(),
                dispatch.task_id.as_str(),
                dispatch.run_id.as_str(),
                serde_json::to_string(&dispatch.permission)?,
                state_name(dispatch.decision)?,
                integer(command.committed_at_ms, "dispatch timestamp")?,
            ],
        )?;
        insert_receipt(
            &transaction,
            command,
            "resolve_provider_permission",
            &dispatch,
        )?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(dispatch))
    }

    /// Expires a pending provider-native approval without dispatching a response.
    ///
    /// The callback must still belong to the current live Runtime generation;
    /// losing that fence requires Run recovery instead of rewriting history as
    /// a normal deadline expiry.
    pub fn expire_provider_approval(
        &mut self,
        command: &LedgerCommand,
        approval_id: &ApprovalId,
        expected_permission: &RuntimePermissionRef,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<ApprovalRecord>, StoreError> {
        validate_command(command)?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "expire_provider_approval")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let mut approval = load_approval(&transaction, approval_id)?;
        require_provider_permission_context(
            &transaction,
            command,
            &approval,
            expected_permission,
            lease,
            TaskState::WaitingApproval,
        )?;
        if approval.state != ApprovalState::Pending {
            return Err(conflict("provider approval is no longer pending"));
        }
        require_not_before(
            command.committed_at_ms,
            approval.updated_at_ms,
            "provider approval expiry",
        )?;
        if command.committed_at_ms < approval.expires_at_ms {
            return Err(conflict("provider approval deadline has not elapsed"));
        }
        let prior_revision = approval.revision;
        approval.state = ApprovalState::Expired;
        approval.revision = next_integer(prior_revision, "approval revision")?;
        approval.decided_by_actor_id = None;
        approval.updated_at_ms = command.committed_at_ms;
        let changed = transaction.execute(
            "UPDATE approvals SET state='expired', revision=?2, decided_by_actor_id=NULL,
             updated_at_ms=?3 WHERE approval_id=?1 AND state='pending' AND revision=?4",
            params![
                approval_id.as_str(),
                integer(approval.revision, "approval revision")?,
                integer(command.committed_at_ms, "approval expiry timestamp")?,
                integer(prior_revision, "approval prior revision")?,
            ],
        )?;
        if changed != 1 {
            return Err(conflict(
                "provider approval expiry lost its pending revision",
            ));
        }
        insert_receipt(&transaction, command, "expire_provider_approval", &approval)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(approval))
    }

    /// Atomically abandons one exact provider callback and advances its Runtime sequence.
    ///
    /// No provider response is prepared. The callback's live binding must
    /// still be current, and the abandonment event must be the next sequence
    /// emitted by that exact Runtime instance.
    pub fn abandon_provider_permission(
        &mut self,
        command: &LedgerCommand,
        approval_id: &ApprovalId,
        expected_permission: &RuntimePermissionRef,
        binding: &RuntimeBindingRef,
        abandoned_event_sequence: u64,
        lease: &LeaseClaim,
        cause: ApprovalAbandonCause,
    ) -> Result<LedgerOutcome<ApprovalRecord>, StoreError> {
        validate_command(command)?;
        integer(abandoned_event_sequence, "provider abandonment sequence")?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "abandon_provider_permission")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let mut approval = load_approval(&transaction, approval_id)?;
        require_provider_permission_context(
            &transaction,
            command,
            &approval,
            expected_permission,
            lease,
            TaskState::WaitingApproval,
        )?;
        if approval.state != ApprovalState::Pending {
            return Err(conflict("provider approval is no longer pending"));
        }
        let runtime = load_runtime_binding(&transaction, &expected_permission.binding_id)?;
        let next_sequence = next_integer(runtime.last_sequence, "Runtime sequence")?;
        if runtime.binding != *binding
            || runtime.actor_id != command.actor_id
            || runtime.state != RuntimeBindingState::Active
            || binding.binding_id != expected_permission.binding_id
            || binding.task_id != approval.task_id
            || binding.run_id != approval.run_id
            || binding.runtime_generation != expected_permission.runtime_generation
            || abandoned_event_sequence != next_sequence
        {
            return Err(conflict(
                "provider abandonment does not match the next live Runtime event",
            ));
        }
        require_not_before(
            command.committed_at_ms,
            runtime.updated_at_ms,
            "provider abandonment",
        )?;
        let sequence_changed = transaction.execute(
            "UPDATE runtime_bindings SET last_sequence=?2, updated_at_ms=?3
             WHERE binding_id=?1 AND state='active' AND runtime_instance_id=?4
               AND runtime_generation=?5 AND last_sequence=?6",
            params![
                binding.binding_id.as_str(),
                integer(abandoned_event_sequence, "provider abandonment sequence")?,
                integer(command.committed_at_ms, "provider abandonment timestamp")?,
                binding.runtime_instance_id.as_str(),
                integer(binding.runtime_generation, "Runtime generation")?,
                integer(runtime.last_sequence, "Runtime prior sequence")?,
            ],
        )?;
        if sequence_changed != 1 {
            return Err(conflict(
                "provider abandonment lost its Runtime sequence precondition",
            ));
        }
        let prior_revision = approval.revision;
        approval.state = ApprovalState::Cancelled;
        approval.revision = next_integer(prior_revision, "approval revision")?;
        approval.decided_by_actor_id = None;
        approval.updated_at_ms = command.committed_at_ms;
        let approval_changed = transaction.execute(
            "UPDATE approvals SET state='cancelled', revision=?2,
             decided_by_actor_id=NULL, updated_at_ms=?3
             WHERE approval_id=?1 AND state='pending' AND revision=?4",
            params![
                approval_id.as_str(),
                integer(approval.revision, "approval revision")?,
                integer(command.committed_at_ms, "provider abandonment timestamp")?,
                integer(prior_revision, "approval prior revision")?,
            ],
        )?;
        if approval_changed != 1 {
            return Err(conflict(
                "provider abandonment lost its pending approval precondition",
            ));
        }
        append_internal_task_event(
            &transaction,
            &approval.task_id,
            &approval.actor_id,
            command.committed_at_ms,
            TaskEvent::ApprovalAbandoned {
                approval_id: approval_id.clone(),
                cause,
            },
            None,
        )?;
        insert_receipt(
            &transaction,
            command,
            "abandon_provider_permission",
            &approval,
        )?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(approval))
    }

    /// Commits the non-replayable boundary before writing a provider response.
    ///
    /// Callers must write to the provider only for [`LedgerOutcome::Applied`].
    /// A replayed result proves that dispatch may already have crossed the
    /// provider boundary and must never trigger another write.
    pub fn start_provider_permission_dispatch(
        &mut self,
        command: &LedgerCommand,
        approval_id: &ApprovalId,
        expected_revision: u64,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<ProviderPermissionDispatchRecord>, StoreError> {
        validate_command(command)?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "start_provider_permission")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let approval = load_approval(&transaction, approval_id)?;
        let mut dispatch = load_provider_permission_dispatch(&transaction, approval_id)?;
        require_provider_permission_context(
            &transaction,
            command,
            &approval,
            &dispatch.permission,
            lease,
            TaskState::Running,
        )?;
        if dispatch.state != ProviderPermissionDispatchState::Prepared
            || dispatch.revision != expected_revision
        {
            return Err(conflict(
                "provider permission dispatch is not prepared at the expected revision",
            ));
        }
        require_not_before(
            command.committed_at_ms,
            dispatch.updated_at_ms,
            "provider permission dispatch start",
        )?;
        dispatch.state = ProviderPermissionDispatchState::WriteStarted;
        dispatch.revision = next_integer(dispatch.revision, "dispatch revision")?;
        dispatch.updated_at_ms = command.committed_at_ms;
        update_provider_permission_dispatch(
            &transaction,
            &dispatch,
            expected_revision,
            "prepared",
        )?;
        insert_receipt(
            &transaction,
            command,
            "start_provider_permission",
            &dispatch,
        )?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(dispatch))
    }

    /// Records that the live provider transport write returned.
    ///
    /// This is not a provider acknowledgement and remains non-replayable after
    /// the live Runtime session is lost.
    pub fn complete_provider_permission_dispatch(
        &mut self,
        command: &LedgerCommand,
        approval_id: &ApprovalId,
        expected_revision: u64,
    ) -> Result<LedgerOutcome<ProviderPermissionDispatchRecord>, StoreError> {
        validate_command(command)?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "complete_provider_permission")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let mut dispatch = load_provider_permission_dispatch(&transaction, approval_id)?;
        require_task_owner(&transaction, &dispatch.task_id, &command.actor_id)?;
        if dispatch.actor_id != command.actor_id
            || dispatch.state != ProviderPermissionDispatchState::WriteStarted
            || dispatch.revision != expected_revision
        {
            return Err(conflict(
                "provider permission dispatch is not started at the expected revision",
            ));
        }
        require_not_before(
            command.committed_at_ms,
            dispatch.updated_at_ms,
            "provider permission dispatch completion",
        )?;
        dispatch.state = ProviderPermissionDispatchState::Written;
        dispatch.revision = next_integer(dispatch.revision, "dispatch revision")?;
        dispatch.updated_at_ms = command.committed_at_ms;
        update_provider_permission_dispatch(
            &transaction,
            &dispatch,
            expected_revision,
            "write_started",
        )?;
        insert_receipt(
            &transaction,
            command,
            "complete_provider_permission",
            &dispatch,
        )?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(dispatch))
    }
}
