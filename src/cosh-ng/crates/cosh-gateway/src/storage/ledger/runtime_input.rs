impl SqliteTaskStore {
    /// Loads one durable bounded Runtime input request.
    pub(crate) fn load_runtime_input_request(
        &self,
        request_id: &InputRequestId,
    ) -> Result<RuntimeInputRequestRecord, StoreError> {
        load_runtime_input_request(self.connection(), request_id)
    }

    /// Loads one private typed response dispatch.
    pub(crate) fn load_runtime_input_dispatch(
        &self,
        request_id: &InputRequestId,
    ) -> Result<RuntimeInputDispatchRecord, StoreError> {
        load_runtime_input_dispatch(self.connection(), request_id)
    }

    /// Atomically consumes one Runtime sequence and records its pending input Task fact.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_runtime_input_request(
        &mut self,
        command: &LedgerCommand,
        request: &RuntimeInputRequest,
        expires_at_ms: u64,
        binding_id: &RuntimeBindingId,
        runtime_instance_id: &RuntimeInstanceId,
        runtime_generation: u64,
        sequence: u64,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<RuntimeInputRequestRecord>, StoreError> {
        validate_command(command)?;
        integer(expires_at_ms, "runtime input deadline")?;
        integer(runtime_generation, "runtime input generation")?;
        integer(sequence, "runtime input sequence")?;
        validate_json_bound(
            request,
            MAX_RUNTIME_INPUT_REQUEST_BYTES,
            "runtime input request",
        )?;
        if expires_at_ms <= command.committed_at_ms || request.run_id() != &lease.run_id {
            return Err(conflict("runtime input request Run or deadline is invalid"));
        }
        let transaction = immediate(self)?;
        if let Some(replayed) = replay::<RuntimeInputRequestRecord>(
            &transaction,
            command,
            "record_runtime_input_request",
        )? {
            if replayed.request != *request
                || replayed.binding_id != *binding_id
                || replayed.runtime_instance_id != *runtime_instance_id
                || replayed.runtime_generation != runtime_generation
                || replayed.runtime_sequence != sequence
                || replayed.expires_at_ms != expires_at_ms
            {
                return Err(StoreError::IdempotencyConflict);
            }
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let binding = load_runtime_binding(&transaction, binding_id)?;
        if binding.actor_id != command.actor_id
            || binding.binding.task_id != lease.task_id
            || binding.binding.run_id != lease.run_id
            || binding.binding.runtime_instance_id != *runtime_instance_id
            || binding.binding.runtime_generation != runtime_generation
            || binding.state != RuntimeBindingState::Active
        {
            return Err(conflict("runtime input request binding is stale"));
        }
        require_current_lease(
            &transaction,
            lease,
            &command.actor_id,
            command.committed_at_ms,
        )?;
        require_not_before(
            command.committed_at_ms,
            binding.updated_at_ms,
            "runtime input request",
        )?;
        let expected_sequence = next_integer(binding.last_sequence, "runtime input sequence")?;
        if sequence != expected_sequence {
            return Err(conflict("runtime input request sequence is stale"));
        }
        let changed = transaction.execute(
            "UPDATE runtime_bindings SET last_sequence=?2, updated_at_ms=?3
             WHERE binding_id=?1 AND state='active' AND runtime_instance_id=?4
               AND runtime_generation=?5 AND last_sequence=?6",
            params![
                binding_id.as_str(),
                integer(sequence, "runtime input sequence")?,
                integer(command.committed_at_ms, "runtime input timestamp")?,
                runtime_instance_id.as_str(),
                integer(runtime_generation, "runtime input generation")?,
                integer(binding.last_sequence, "runtime input prior sequence")?,
            ],
        )?;
        if changed != 1 {
            return Err(conflict("runtime input request lost its sequence fence"));
        }
        append_internal_task_event(
            &transaction,
            &lease.task_id,
            &command.actor_id,
            command.committed_at_ms,
            TaskEvent::InputRequested {
                request: request.clone(),
            },
            None,
        )?;
        let record = RuntimeInputRequestRecord {
            request: request.clone(),
            actor_id: command.actor_id.clone(),
            task_id: lease.task_id.clone(),
            run_id: lease.run_id.clone(),
            binding_id: binding_id.clone(),
            runtime_instance_id: runtime_instance_id.clone(),
            runtime_generation,
            runtime_sequence: sequence,
            lease_generation: lease.generation,
            lease_revision: lease.revision,
            state: RuntimeInputRequestState::Pending,
            response_digest: None,
            revision: 1,
            expires_at_ms,
            created_at_ms: command.committed_at_ms,
            updated_at_ms: command.committed_at_ms,
        };
        insert_runtime_input_request(&transaction, &record)?;
        insert_receipt(
            &transaction,
            command,
            "record_runtime_input_request",
            &record,
        )?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }

    /// Atomically records a private typed response, digest-only Task fact, and Prepared dispatch.
    pub(crate) fn resolve_runtime_input(
        &mut self,
        command: &LedgerCommand,
        request_id: &InputRequestId,
        expected_task_revision: u64,
        response: &RuntimeInputResponse,
    ) -> Result<LedgerOutcome<RuntimeInputDispatchRecord>, StoreError> {
        validate_command(command)?;
        integer(
            expected_task_revision,
            "runtime input expected Task revision",
        )?;
        validate_json_bound(
            response,
            MAX_RUNTIME_INPUT_RESPONSE_BYTES,
            "runtime input response",
        )?;
        let response_digest = runtime_input_response_digest(response)?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay_runtime_input_dispatch(
            &transaction,
            command,
            "resolve_runtime_input",
            request_id,
            &response_digest,
        )? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let mut request = load_runtime_input_request(&transaction, request_id)?;
        if request.actor_id != command.actor_id
            || request.state != RuntimeInputRequestState::Pending
            || request.expires_at_ms <= command.committed_at_ms
        {
            return Err(conflict(
                "runtime input request actor, state, or deadline is stale",
            ));
        }
        validate_runtime_input_response(&request.request, response)?;
        let task_revision = transaction.query_row(
            "SELECT revision FROM tasks WHERE task_id=?1 AND owner_actor_id=?2",
            params![request.task_id.as_str(), command.actor_id.as_str()],
            |row| row.get::<_, i64>(0),
        )?;
        if unsigned(task_revision, "runtime input Task revision")? != expected_task_revision {
            return Err(conflict("runtime input Task revision is stale"));
        }
        append_internal_task_event(
            &transaction,
            &request.task_id,
            &command.actor_id,
            command.committed_at_ms,
            TaskEvent::InputSubmitted {
                request_id: request_id.clone(),
                run_id: request.request.run_id().clone(),
                response_digest: response_digest.clone(),
            },
            None,
        )?;
        request.state = RuntimeInputRequestState::Resolved;
        request.response_digest = Some(response_digest.clone());
        request.revision = next_integer(request.revision, "runtime input request revision")?;
        request.updated_at_ms = command.committed_at_ms;
        let changed = transaction.execute(
            "UPDATE runtime_input_requests
             SET state='resolved', response_digest=?2, revision=?3, updated_at_ms=?4
             WHERE request_id=?1 AND state='pending' AND revision=1",
            params![
                request_id.as_str(),
                response_digest.as_str(),
                integer(request.revision, "runtime input request revision")?,
                integer(
                    command.committed_at_ms,
                    "runtime input resolution timestamp"
                )?,
            ],
        )?;
        if changed != 1 {
            return Err(conflict("runtime input request lost its pending revision"));
        }
        let dispatch = RuntimeInputDispatchRecord {
            request_id: request_id.clone(),
            actor_id: command.actor_id.clone(),
            task_id: request.task_id.clone(),
            run_id: request.run_id.clone(),
            response: response.clone(),
            response_digest,
            state: RuntimeInputDispatchState::Prepared,
            revision: 1,
            created_at_ms: command.committed_at_ms,
            updated_at_ms: command.committed_at_ms,
        };
        insert_runtime_input_dispatch(&transaction, &dispatch)?;
        insert_runtime_input_dispatch_receipt(
            &transaction,
            command,
            "resolve_runtime_input",
            &dispatch,
        )?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(dispatch))
    }

    /// Expires one pending input request at or after its durable deadline.
    pub(crate) fn expire_runtime_input_request(
        &mut self,
        command: &LedgerCommand,
        request_id: &InputRequestId,
        expected_revision: u64,
    ) -> Result<LedgerOutcome<RuntimeInputRequestRecord>, StoreError> {
        transition_pending_runtime_input_request(
            self,
            command,
            request_id,
            expected_revision,
            RuntimeInputRequestState::Expired,
            true,
            "expire_runtime_input_request",
        )
    }

    /// Cancels one pending request while converging its Task to Suspended.
    pub(crate) fn cancel_runtime_input_request(
        &mut self,
        command: &LedgerCommand,
        request_id: &InputRequestId,
        expected_revision: u64,
    ) -> Result<LedgerOutcome<RuntimeInputRequestRecord>, StoreError> {
        transition_pending_runtime_input_request(
            self,
            command,
            request_id,
            expected_revision,
            RuntimeInputRequestState::Cancelled,
            false,
            "cancel_runtime_input_request",
        )
    }

    /// Commits the non-replayable boundary before writing one input response.
    pub(crate) fn start_runtime_input_dispatch(
        &mut self,
        command: &LedgerCommand,
        request_id: &InputRequestId,
        response_digest: &Digest,
        expected_revision: u64,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<RuntimeInputDispatchRecord>, StoreError> {
        transition_runtime_input_dispatch(
            self,
            command,
            request_id,
            response_digest,
            expected_revision,
            lease,
            RuntimeInputDispatchState::Prepared,
            RuntimeInputDispatchState::Started,
            "start_runtime_input_dispatch",
        )
    }

    /// Records that Runtime transport accepted the one-shot input response.
    pub(crate) fn complete_runtime_input_dispatch(
        &mut self,
        command: &LedgerCommand,
        request_id: &InputRequestId,
        response_digest: &Digest,
        expected_revision: u64,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<RuntimeInputDispatchRecord>, StoreError> {
        transition_runtime_input_dispatch(
            self,
            command,
            request_id,
            response_digest,
            expected_revision,
            lease,
            RuntimeInputDispatchState::Started,
            RuntimeInputDispatchState::Delivered,
            "complete_runtime_input_dispatch",
        )
    }

    /// Marks a started input response permanently indeterminate.
    pub(crate) fn mark_runtime_input_dispatch_unknown(
        &mut self,
        command: &LedgerCommand,
        request_id: &InputRequestId,
        response_digest: &Digest,
        expected_revision: u64,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<RuntimeInputDispatchRecord>, StoreError> {
        mark_runtime_input_dispatch_unknown_atomic(
            self,
            command,
            request_id,
            response_digest,
            expected_revision,
            lease,
            "mark_runtime_input_dispatch_unknown",
        )
    }

    /// Atomically converges one abandoned input request or dispatch after Run takeover.
    pub fn recover_runtime_input_dispatch_for_run(
        &mut self,
        command: &LedgerCommand,
        run_id: &RunId,
        takeover_lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<u64>, StoreError> {
        validate_command(command)?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay::<u64>(
            &transaction,
            command,
            "recover_runtime_input_dispatch_for_run",
        )? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let dispatches = load_recoverable_runtime_input_dispatches(&transaction, run_id)?;
        let requests = load_pending_runtime_input_requests(&transaction, run_id)?;
        if dispatches.len() + requests.len() > 1 {
            return Err(corrupt(
                "one Run has multiple recoverable Runtime input records",
            ));
        }
        let changed = if let Some(dispatch) = dispatches.first() {
            if dispatch.actor_id != command.actor_id {
                return Err(conflict(
                    "runtime input recovery actor does not own the dispatch",
                ));
            }
            let request = load_runtime_input_request(&transaction, &dispatch.request_id)?;
            if takeover_lease.task_id != dispatch.task_id
                || takeover_lease.run_id != dispatch.run_id
                || takeover_lease.generation <= request.lease_generation
            {
                return Err(conflict(
                    "runtime input recovery requires a newer takeover lease",
                ));
            }
            require_current_lease(
                &transaction,
                takeover_lease,
                &command.actor_id,
                command.committed_at_ms,
            )?;
            let changed = transaction.execute(
                "UPDATE runtime_input_dispatches
                 SET state='unknown', revision=revision+1, updated_at_ms=?2
                 WHERE request_id=?1 AND state IN ('prepared', 'started') AND revision=?3",
                params![
                    dispatch.request_id.as_str(),
                    integer(command.committed_at_ms, "runtime input recovery timestamp")?,
                    integer(dispatch.revision, "runtime input dispatch revision")?,
                ],
            )?;
            if changed != 1 {
                return Err(conflict("runtime input recovery lost its started revision"));
            }
            if runtime_input_recovery_requires_suspension(
                &transaction,
                &dispatch.task_id,
                &dispatch.actor_id,
                &dispatch.run_id,
                TaskState::Running,
            )? {
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
            }
            1u64
        } else if let Some(request) = requests.first() {
            if request.actor_id != command.actor_id
                || takeover_lease.task_id != request.task_id
                || takeover_lease.run_id != request.run_id
                || takeover_lease.generation <= request.lease_generation
            {
                return Err(conflict(
                    "runtime input recovery requires an owned newer takeover lease",
                ));
            }
            require_current_lease(
                &transaction,
                takeover_lease,
                &command.actor_id,
                command.committed_at_ms,
            )?;
            let changed = transaction.execute(
                "UPDATE runtime_input_requests
                 SET state='cancelled', revision=revision+1, updated_at_ms=?2
                 WHERE request_id=?1 AND state='pending' AND revision=?3",
                params![
                    request.request.request_id().as_str(),
                    integer(command.committed_at_ms, "runtime input recovery timestamp")?,
                    integer(request.revision, "runtime input request revision")?,
                ],
            )?;
            if changed != 1 {
                return Err(conflict("runtime input recovery lost its pending revision"));
            }
            if runtime_input_recovery_requires_suspension(
                &transaction,
                &request.task_id,
                &request.actor_id,
                &request.run_id,
                TaskState::WaitingInput,
            )? {
                append_internal_task_event(
                    &transaction,
                    &request.task_id,
                    &request.actor_id,
                    command.committed_at_ms,
                    TaskEvent::RunSuspended {
                        run_id: request.run_id.clone(),
                        reason: cosh_gateway_contracts::task::SuspensionCode::OperatorRequired,
                    },
                    None,
                )?;
            }
            1u64
        } else {
            0u64
        };
        insert_receipt(
            &transaction,
            command,
            "recover_runtime_input_dispatch_for_run",
            &changed,
        )?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(changed))
    }
}
