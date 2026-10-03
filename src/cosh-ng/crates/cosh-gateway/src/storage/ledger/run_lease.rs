impl SqliteTaskStore {

    /// Acquires an absent or expired Run lease with a monotonically increasing generation.
    pub fn acquire_run_lease(
        &mut self,
        lease: &LeaseCommand,
    ) -> Result<LedgerOutcome<RunLeaseRecord>, StoreError> {
        validate_command(&lease.command)?;
        integer(lease.expires_at_ms, "lease deadline")?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, &lease.command, "acquire_run_lease")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        require_task_run(
            &transaction,
            &lease.task_id,
            &lease.run_id,
            &lease.command.actor_id,
        )?;
        if lease.expires_at_ms <= lease.command.committed_at_ms {
            return Err(conflict("run lease deadline must be in the future"));
        }
        let existing = load_run_lease_optional(&transaction, &lease.run_id)?;
        if let Some(existing) = &existing {
            if existing.task_id != lease.task_id || existing.actor_id != lease.command.actor_id {
                return Err(conflict(
                    "run lease Task or actor binding cannot be replaced",
                ));
            }
            if existing.expires_at_ms > lease.command.committed_at_ms {
                return Err(conflict("run lease is still held"));
            }
            require_not_before(
                lease.command.committed_at_ms,
                existing.updated_at_ms,
                "run lease takeover",
            )?;
        }
        let generation = match &existing {
            Some(row) => next_integer(row.generation, "lease generation")?,
            None => 1,
        };
        let revision = match &existing {
            Some(row) => next_integer(row.revision, "lease revision")?,
            None => 1,
        };
        let record = RunLeaseRecord {
            task_id: lease.task_id.clone(),
            run_id: lease.run_id.clone(),
            actor_id: lease.command.actor_id.clone(),
            lease_owner: lease.lease_owner.clone(),
            generation,
            revision,
            expires_at_ms: lease.expires_at_ms,
            updated_at_ms: lease.command.committed_at_ms,
        };
        transaction.execute(
            "INSERT INTO run_leases(run_id, task_id, actor_id, lease_owner, generation, revision,
             expires_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(run_id) DO UPDATE SET task_id=excluded.task_id, actor_id=excluded.actor_id,
             lease_owner=excluded.lease_owner, generation=excluded.generation,
             revision=excluded.revision, expires_at_ms=excluded.expires_at_ms,
             updated_at_ms=excluded.updated_at_ms",
            params![
                record.run_id.as_str(),
                record.task_id.as_str(),
                record.actor_id.as_str(),
                record.lease_owner.as_str(),
                integer(record.generation, "lease generation")?,
                integer(record.revision, "lease revision")?,
                integer(record.expires_at_ms, "lease deadline")?,
                integer(record.updated_at_ms, "lease timestamp")?
            ],
        )?;
        insert_receipt(&transaction, &lease.command, "acquire_run_lease", &record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }

    /// Renews an active Run lease without changing its fencing generation.
    pub fn renew_run_lease(
        &mut self,
        lease: &LeaseCommand,
        expected_generation: u64,
        expected_revision: u64,
    ) -> Result<LedgerOutcome<RunLeaseRecord>, StoreError> {
        validate_command(&lease.command)?;
        integer(expected_generation, "lease generation")?;
        integer(expected_revision, "lease expected revision")?;
        integer(lease.expires_at_ms, "lease deadline")?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, &lease.command, "renew_run_lease")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        require_task_run(
            &transaction,
            &lease.task_id,
            &lease.run_id,
            &lease.command.actor_id,
        )?;
        let existing = load_run_lease_optional(&transaction, &lease.run_id)?
            .ok_or_else(|| not_found("run lease", lease.run_id.as_str()))?;
        if existing.task_id != lease.task_id
            || existing.actor_id != lease.command.actor_id
            || existing.lease_owner != lease.lease_owner
            || existing.generation != expected_generation
            || existing.revision != expected_revision
            || existing.expires_at_ms <= lease.command.committed_at_ms
            || lease.expires_at_ms <= lease.command.committed_at_ms
        {
            return Err(conflict(
                "run lease renewal binding, revision, or deadline is stale",
            ));
        }
        require_not_before(
            lease.command.committed_at_ms,
            existing.updated_at_ms,
            "run lease renewal",
        )?;
        let next_revision = next_integer(existing.revision, "lease revision")?;
        let record = RunLeaseRecord {
            revision: next_revision,
            expires_at_ms: lease.expires_at_ms,
            updated_at_ms: lease.command.committed_at_ms,
            ..existing
        };
        let changed = transaction.execute(
            "UPDATE run_leases SET revision=?2, expires_at_ms=?3, updated_at_ms=?4
             WHERE run_id=?1 AND generation=?5 AND revision=?6 AND lease_owner=?7",
            params![
                record.run_id.as_str(),
                integer(record.revision, "lease revision")?,
                integer(record.expires_at_ms, "lease deadline")?,
                integer(record.updated_at_ms, "lease update")?,
                integer(expected_generation, "lease generation")?,
                integer(expected_revision, "lease expected revision")?,
                record.lease_owner.as_str()
            ],
        )?;
        if changed != 1 {
            return Err(conflict("run lease renewal lost its fencing precondition"));
        }
        insert_receipt(&transaction, &lease.command, "renew_run_lease", &record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }

    /// Releases an active Run lease while retaining its fencing generation.
    pub fn release_run_lease(
        &mut self,
        command: &LedgerCommand,
        claim: &LeaseClaim,
    ) -> Result<LedgerOutcome<RunLeaseRecord>, StoreError> {
        validate_command(command)?;
        integer(claim.generation, "lease generation")?;
        integer(claim.revision, "lease expected revision")?;
        let transaction = immediate(self)?;
        if let Some(replayed) = replay(&transaction, command, "release_run_lease")? {
            transaction.commit()?;
            return Ok(LedgerOutcome::Replayed(replayed));
        }
        require_task_run(
            &transaction,
            &claim.task_id,
            &claim.run_id,
            &command.actor_id,
        )?;
        let existing = load_run_lease_optional(&transaction, &claim.run_id)?
            .ok_or_else(|| not_found("run lease", claim.run_id.as_str()))?;
        if existing.task_id != claim.task_id
            || existing.actor_id != command.actor_id
            || existing.lease_owner != claim.lease_owner
            || existing.generation != claim.generation
            || existing.revision != claim.revision
            || existing.expires_at_ms <= command.committed_at_ms
        {
            return Err(conflict(
                "run lease release binding, revision, or deadline is stale",
            ));
        }
        require_not_before(
            command.committed_at_ms,
            existing.updated_at_ms,
            "run lease release",
        )?;
        let next_revision = next_integer(existing.revision, "lease revision")?;
        let record = RunLeaseRecord {
            revision: next_revision,
            expires_at_ms: command.committed_at_ms,
            updated_at_ms: command.committed_at_ms,
            ..existing
        };
        let changed = transaction.execute(
            "UPDATE run_leases SET revision=?2, expires_at_ms=?3, updated_at_ms=?3
             WHERE run_id=?1 AND generation=?4 AND revision=?5 AND lease_owner=?6",
            params![
                record.run_id.as_str(),
                integer(record.revision, "lease revision")?,
                integer(record.updated_at_ms, "lease release timestamp")?,
                integer(claim.generation, "lease generation")?,
                integer(claim.revision, "lease expected revision")?,
                record.lease_owner.as_str()
            ],
        )?;
        if changed != 1 {
            return Err(conflict("run lease release lost its fencing precondition"));
        }
        insert_receipt(&transaction, command, "release_run_lease", &record)?;
        transaction.commit()?;
        Ok(LedgerOutcome::Applied(record))
    }
}
