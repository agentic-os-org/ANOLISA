impl SqliteTaskStore {

    /// Persists a new runtime generation and fences older active bindings for the Run.
    pub fn bind_runtime(
        &mut self,
        command: &LedgerCommand,
        binding: &RuntimeBindingRef,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<RuntimeBindingRecord>, StoreError> {
        validate_command(command)?;
        integer(binding.runtime_generation, "runtime generation")?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "bind_runtime")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        require_task_run(
            &transaction,
            &binding.task_id,
            &binding.run_id,
            &command.actor_id,
        )?;
        if lease.task_id != binding.task_id || lease.run_id != binding.run_id {
            return Err(conflict(
                "runtime binding lease does not bind the Runtime Task and Run",
            ));
        }
        require_current_lease(
            &transaction,
            lease,
            &command.actor_id,
            command.committed_at_ms,
        )?;
        if binding.runtime_generation != lease.generation {
            return Err(StoreError::GenerationFenced {
                expected: lease.generation,
                actual: binding.runtime_generation,
            });
        }
        let highest = transaction.query_row(
            "SELECT COALESCE(MAX(runtime_generation), 0) FROM runtime_bindings WHERE run_id = ?1",
            params![binding.run_id.as_str()],
            |row| row.get::<_, i64>(0),
        )?;
        let highest = unsigned(highest, "runtime generation")?;
        let minimum = next_integer(highest, "runtime generation")?;
        if binding.runtime_generation < minimum {
            return Err(StoreError::GenerationFenced {
                expected: minimum,
                actual: binding.runtime_generation,
            });
        }
        let now = integer(command.committed_at_ms, "runtime binding timestamp")?;
        let latest_update = transaction.query_row(
            "SELECT COALESCE(MAX(updated_at_ms), 0) FROM runtime_bindings WHERE run_id=?1",
            params![binding.run_id.as_str()],
            |row| row.get::<_, i64>(0),
        )?;
        require_not_before(
            command.committed_at_ms,
            unsigned(latest_update, "runtime binding update")?,
            "runtime binding",
        )?;
        transaction.execute(
            "UPDATE runtime_bindings SET state = 'lost', updated_at_ms = ?2
             WHERE run_id = ?1 AND state = 'active'",
            params![binding.run_id.as_str(), now],
        )?;
        transaction.execute(
            "INSERT INTO runtime_bindings(binding_id, actor_id, task_id, run_id,
             runtime_instance_id, runtime_generation, binding_json, state, last_sequence,
             created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 0, ?8, ?8)",
            params![binding.binding_id.as_str(), command.actor_id.as_str(), binding.task_id.as_str(),
                binding.run_id.as_str(), binding.runtime_instance_id.as_str(),
                integer(binding.runtime_generation, "runtime generation")?,
                serde_json::to_string(binding)?, now],
        )?;
        let record = RuntimeBindingRecord {
            binding: binding.clone(),
            actor_id: command.actor_id.clone(),
            state: RuntimeBindingState::Active,
            last_sequence: 0,
            created_at_ms: command.committed_at_ms,
            updated_at_ms: command.committed_at_ms,
        };
        insert_receipt(&transaction, command, "bind_runtime", &record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }

    /// Advances a binding's sequence only for its exact active process generation.
    pub fn record_runtime_sequence(
        &mut self,
        binding_id: &RuntimeBindingId,
        runtime_instance_id: &RuntimeInstanceId,
        runtime_generation: u64,
        sequence: u64,
        updated_at_ms: u64,
        lease: &LeaseClaim,
    ) -> Result<RuntimeBindingRecord, StoreError> {
        integer(runtime_generation, "runtime generation")?;
        integer(sequence, "runtime sequence")?;
        integer(updated_at_ms, "runtime event timestamp")?;
        let transaction = immediate(self)?;
        let record = load_runtime_binding(&transaction, binding_id)?;
        if lease.task_id != record.binding.task_id || lease.run_id != record.binding.run_id {
            return Err(conflict(
                "runtime event lease does not bind the runtime Task and Run",
            ));
        }
        require_current_lease(&transaction, lease, &record.actor_id, updated_at_ms)?;
        require_not_before(
            updated_at_ms,
            record.updated_at_ms,
            "runtime event acceptance",
        )?;
        if record.binding.runtime_generation != runtime_generation {
            return Err(StoreError::GenerationFenced {
                expected: record.binding.runtime_generation,
                actual: runtime_generation,
            });
        }
        let expected_sequence = next_integer(record.last_sequence, "runtime sequence")?;
        if record.state != RuntimeBindingState::Active
            || &record.binding.runtime_instance_id != runtime_instance_id
            || sequence != expected_sequence
        {
            return Err(conflict(
                "runtime instance, state, or event sequence is stale",
            ));
        }
        let changed = transaction.execute(
            "UPDATE runtime_bindings SET last_sequence = ?2, updated_at_ms = ?3
             WHERE binding_id = ?1 AND state = 'active' AND runtime_generation = ?4
             AND last_sequence = ?5",
            params![
                binding_id.as_str(),
                integer(sequence, "runtime sequence")?,
                integer(updated_at_ms, "runtime event timestamp")?,
                integer(runtime_generation, "runtime generation")?,
                integer(record.last_sequence, "runtime prior sequence")?
            ],
        )?;
        if changed != 1 {
            return Err(conflict(
                "runtime sequence lost its active-generation precondition",
            ));
        }
        let updated = load_runtime_binding(&transaction, binding_id)?;
        transaction.commit()?;
        Ok(updated)
    }

    /// Closes an active runtime binding only for its exact fenced generation.
    pub fn close_runtime_binding(
        &mut self,
        command: &LedgerCommand,
        binding_id: &RuntimeBindingId,
        runtime_generation: u64,
        lease: &LeaseClaim,
    ) -> Result<LedgerOutcome<RuntimeBindingRecord>, StoreError> {
        validate_command(command)?;
        integer(runtime_generation, "runtime generation")?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "close_runtime_binding")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        let mut record = load_runtime_binding(&transaction, binding_id)?;
        require_task_owner(&transaction, &record.binding.task_id, &command.actor_id)?;
        if lease.task_id != record.binding.task_id || lease.run_id != record.binding.run_id {
            return Err(conflict(
                "runtime close lease does not bind the Runtime Task and Run",
            ));
        }
        require_current_lease(
            &transaction,
            lease,
            &command.actor_id,
            command.committed_at_ms,
        )?;
        if record.actor_id != command.actor_id {
            return Err(conflict("runtime binding actor does not match"));
        }
        if record.binding.runtime_generation != runtime_generation {
            return Err(StoreError::GenerationFenced {
                expected: record.binding.runtime_generation,
                actual: runtime_generation,
            });
        }
        if record.state != RuntimeBindingState::Active {
            return Err(conflict("runtime binding is not active"));
        }
        require_not_before(
            command.committed_at_ms,
            record.updated_at_ms,
            "runtime close",
        )?;
        record.state = RuntimeBindingState::Closed;
        record.updated_at_ms = command.committed_at_ms;
        let changed = transaction.execute(
            "UPDATE runtime_bindings SET state='closed', updated_at_ms=?2
             WHERE binding_id=?1 AND state='active' AND runtime_generation=?3",
            params![
                binding_id.as_str(),
                integer(command.committed_at_ms, "runtime close timestamp")?,
                integer(runtime_generation, "runtime generation")?
            ],
        )?;
        if changed != 1 {
            return Err(conflict(
                "runtime close lost its active-generation precondition",
            ));
        }
        insert_receipt(&transaction, command, "close_runtime_binding", &record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }
}
