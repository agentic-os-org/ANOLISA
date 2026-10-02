impl SqliteTaskStore {

    /// Returns a durable task-command receipt for exact replay.
    ///
    /// # Errors
    ///
    /// Returns an idempotency conflict when the key belongs to another
    /// command, or a corruption error for an invalid stored receipt.
    pub fn load_command_receipt(
        &self,
        actor_id: &ActorId,
        idempotency_key: &IdempotencyKey,
        command_digest: &Digest,
    ) -> Result<Option<CommitReceipt>, StoreError> {
        let existing = self
            .connection()
            .query_row(
                "SELECT command_digest, receipt_json FROM command_receipts
                 WHERE actor_id = ?1 AND idempotency_key = ?2",
                params![actor_id.as_str(), idempotency_key.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((stored_digest, receipt_json)) = existing else {
            return Ok(None);
        };
        if stored_digest != command_digest.as_str() {
            return Err(StoreError::IdempotencyConflict);
        }
        Ok(Some(serde_json::from_str::<CommitReceipt>(&receipt_json)?))
    }

    /// Atomically persists an already-authenticated and authorized coordinator
    /// decision. This storage boundary does not replace caller-side ingress
    /// authentication or authorization policy.
    ///
    /// Idempotency replay is checked before the optimistic revision, so a
    /// retried command returns its original receipt after the Task advances.
    ///
    /// # Errors
    ///
    /// Returns a conflict for key or revision reuse, a reducer error for an
    /// illegal transition, or a storage error. No partial rows are committed.
    pub(crate) fn commit_task(&mut self, commit: &TaskCommit) -> Result<CommitOutcome, StoreError> {
        self.commit_task_with_run_guard(commit, None)
    }

    pub(crate) fn commit_retry_task(
        &mut self,
        commit: &TaskCommit,
        previous_run_id: &RunId,
    ) -> Result<CommitOutcome, StoreError> {
        self.commit_task_with_run_guard(commit, Some(RunCommitGuard::Retry(previous_run_id)))
    }

    pub(crate) fn commit_suspended_cancel(
        &mut self,
        commit: &TaskCommit,
        run_id: &RunId,
    ) -> Result<CommitOutcome, StoreError> {
        self.commit_task_with_run_guard(commit, Some(RunCommitGuard::SuspendedCancel(run_id)))
    }

    fn commit_task_with_run_guard(
        &mut self,
        commit: &TaskCommit,
        guard: Option<RunCommitGuard<'_>>,
    ) -> Result<CommitOutcome, StoreError> {
        validate_commit_resource_bounds(commit)?;
        let (task_id, event_ids) = validate_commit_shape(commit)?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(outcome) = replay_receipt(&transaction, commit)? {
            let task_id = match &outcome {
                CommitOutcome::Replayed(receipt) | CommitOutcome::Applied(receipt) => {
                    &receipt.task_id
                }
            };
            load_verified_projection(&transaction, task_id)?
                .ok_or_else(|| corrupt("idempotency receipt references a missing Task"))?;
            transaction.commit()?;
            return Ok(outcome);
        }

        if let Some(guard) = guard {
            match guard {
                RunCommitGuard::Retry(previous_run_id) => {
                    require_retry_run_quiescent(&transaction, commit, previous_run_id)?;
                }
                RunCommitGuard::SuspendedCancel(run_id) => {
                    require_suspended_cancel_quiescent(&transaction, commit, run_id)?;
                }
            }
        }

        let current = load_verified_projection(&transaction, task_id)?;
        if current
            .as_ref()
            .is_some_and(|aggregate| aggregate.owner_actor_id() != &commit.actor_id)
        {
            return Err(invalid("commit actor does not own the existing Task"));
        }
        let actual_revision = current.as_ref().map_or(0, TaskAggregate::revision);
        if let Some(expected) = commit.expected_revision {
            if expected != actual_revision {
                return Err(StoreError::RevisionConflict {
                    expected,
                    actual: actual_revision,
                });
            }
        }

        let aggregate = reduce_commit(current, &commit.events)?;
        if aggregate.owner_actor_id() != &commit.actor_id {
            return Err(invalid("commit actor does not own the created Task"));
        }
        persist_projection(
            &transaction,
            &aggregate,
            actual_revision,
            commit.committed_at_ms,
        )?;
        append_events(&transaction, &commit.events)?;
        append_outbox(&transaction, task_id, commit)?;

        let receipt = CommitReceipt {
            task_id: task_id.clone(),
            revision: aggregate.revision(),
            event_ids,
            delivery_ids: commit
                .outbox
                .iter()
                .map(|intent| intent.delivery_id.clone())
                .collect(),
        };
        insert_receipt(&transaction, commit, &receipt)?;
        transaction.commit()?;
        Ok(CommitOutcome::Applied(receipt))
    }

    /// Exposes the raw Task writer only to debug integration fault fixtures.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn commit_task_for_test(
        &mut self,
        commit: &TaskCommit,
    ) -> Result<CommitOutcome, StoreError> {
        self.commit_task(commit)
    }

    pub(crate) fn load_runtime_start_intent_for_retry(
        &self,
        actor_id: &ActorId,
        task_id: &TaskId,
        previous_run_id: &RunId,
    ) -> Result<serde_json::Value, StoreError> {
        let task = self.load_task(task_id)?;
        if task.owner_actor_id() != actor_id {
            return Err(StoreError::LedgerConflict {
                message: "retry actor does not own the Task".to_owned(),
            });
        }

        let malformed_count = self.connection().query_row(
            "SELECT COUNT(*) FROM outbox
             WHERE task_id=?1 AND delivery_kind='runtime_start'
               AND json_valid(payload_json)=0",
            params![task_id.as_str()],
            |row| row.get::<_, i64>(0),
        )?;
        if malformed_count != 0 {
            return Err(corrupt(
                "runtime start Outbox contains malformed JSON for the retry Task",
            ));
        }

        let mut statement = self.connection().prepare(
            "SELECT state, length(payload_json),
                    CASE WHEN length(payload_json) <= ?3 THEN payload_json ELSE NULL END
             FROM outbox
             WHERE task_id=?1 AND delivery_kind='runtime_start'
               AND json_extract(payload_json, '$.run_id')=?2
             ORDER BY created_at_ms, delivery_id LIMIT 2",
        )?;
        let rows = statement
            .query_map(
                params![
                    task_id.as_str(),
                    previous_run_id.as_str(),
                    i64::try_from(MAX_TASK_PAYLOAD_BYTES)
                        .map_err(|_| corrupt("runtime start payload bound exceeds SQLite range"))?,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let [(state, payload_bytes, bounded_payload_json)] = rows.as_slice() else {
            if rows.is_empty() {
                return Err(StoreError::LedgerNotFound {
                    entity: format!(
                        "delivered runtime start intent for Run {}",
                        previous_run_id.as_str()
                    ),
                });
            }
            return Err(corrupt(
                "retry Run matches multiple runtime start Outbox intents",
            ));
        };
        if state != "delivered" {
            return Err(StoreError::LedgerConflict {
                message: "retry requires a delivered runtime start intent".to_owned(),
            });
        }
        let payload_bytes = usize::try_from(*payload_bytes)
            .map_err(|_| corrupt("runtime start Outbox has a negative payload length"))?;
        if payload_bytes > MAX_TASK_PAYLOAD_BYTES {
            return Err(corrupt(
                "runtime start Outbox payload exceeds the durable payload bound",
            ));
        }
        let payload_json = bounded_payload_json
            .as_ref()
            .ok_or_else(|| corrupt("bounded runtime start Outbox payload was not materialized"))?;
        let payload = serde_json::from_str::<serde_json::Value>(payload_json)
            .map_err(|error| corrupt(&format!("runtime start payload cannot decode: {error}")))?;
        if payload
            .pointer("/actor/actor_id")
            .and_then(|value| value.as_str())
            != Some(actor_id.as_str())
            || payload.get("task_id").and_then(|value| value.as_str()) != Some(task_id.as_str())
            || payload.get("run_id").and_then(|value| value.as_str())
                != Some(previous_run_id.as_str())
        {
            return Err(corrupt(
                "runtime start Outbox payload does not match retry identities",
            ));
        }
        Ok(payload)
    }
}
