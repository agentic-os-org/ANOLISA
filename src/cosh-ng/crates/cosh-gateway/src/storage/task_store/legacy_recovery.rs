impl SqliteTaskStore {
    pub(super) fn settle_legacy_runtime_start_recoveries(&mut self) -> Result<(), StoreError> {
        let candidates = self.legacy_runtime_start_recovery_candidates()?;
        for (task_id, run_id) in candidates {
            self.settle_legacy_runtime_start_recovery(&task_id, &run_id)?;
        }
        Ok(())
    }

    fn legacy_runtime_start_recovery_candidates(&self) -> Result<Vec<(TaskId, RunId)>, StoreError> {
        let mut statement = self.connection().prepare(
            "SELECT task_id, run_id
             FROM legacy_runtime_start_recoveries
             WHERE state = 'pending'
             ORDER BY detected_at_ms, task_id",
        )?;
        let candidates = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (task_id, run_id) = row?;
                Ok((
                    TaskId::parse(task_id).map_err(|error| {
                        corrupt(&format!("invalid legacy recovery Task identity: {error}"))
                    })?,
                    RunId::parse(run_id).map_err(|error| {
                        corrupt(&format!("invalid legacy recovery Run identity: {error}"))
                    })?,
                ))
            })
            .collect();
        candidates
    }

    fn settle_legacy_runtime_start_recovery(
        &mut self,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<(), StoreError> {
        let now_ms = current_time_ms()?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let marker_state = transaction
            .query_row(
                "SELECT state FROM legacy_runtime_start_recoveries
                 WHERE task_id=?1 AND run_id=?2",
                params![task_id.as_str(), run_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if marker_state.as_deref() != Some("pending") {
            return Err(corrupt(
                "legacy Runtime recovery marker lost its pending precondition",
            ));
        }
        let task =
            load_verified_projection(&transaction, task_id)?.ok_or(StoreError::TaskNotFound)?;
        if task.state() != TaskState::Queued || task.active_run_id() != Some(run_id) {
            return Err(corrupt(
                "legacy Runtime recovery marker no longer matches its queued Task",
            ));
        }

        let existing_events = load_events(&transaction, task_id)?;
        let previous = existing_events
            .last()
            .ok_or_else(|| corrupt("legacy queued Task has no immutable history"))?;
        let mut correlation = Correlation::new(previous.header.correlation.installation_id.clone());
        correlation.actor_id = Some(task.owner_actor_id().clone());
        correlation.task_id = Some(task_id.clone());
        correlation.run_id = Some(run_id.clone());
        correlation.causation_message_id = Some(previous.header.message_id.clone());
        let cancellation_revision = task
            .revision()
            .checked_add(1)
            .ok_or_else(|| corrupt("legacy recovery Task revision overflow"))?;
        let run_cancelled_revision = task
            .revision()
            .checked_add(2)
            .ok_or_else(|| corrupt("legacy recovery Task revision overflow"))?;
        let task_cancelled_revision = task
            .revision()
            .checked_add(3)
            .ok_or_else(|| corrupt("legacy recovery Task revision overflow"))?;

        let cancellation_requested = migration_event(
            task_id,
            cancellation_revision,
            now_ms,
            correlation.clone(),
            TaskEvent::CancellationRequested {
                run_id: run_id.clone(),
                cause: CancelReason::RuntimeShutdown,
            },
        );
        correlation.causation_message_id = Some(cancellation_requested.header.message_id.clone());
        let run_cancelled = migration_event(
            task_id,
            run_cancelled_revision,
            now_ms,
            correlation.clone(),
            TaskEvent::RunCancelled {
                run_id: run_id.clone(),
                stage: CancellationStage::BeforeRuntime,
            },
        );
        correlation.causation_message_id = Some(run_cancelled.header.message_id.clone());
        let task_cancelled = migration_event(
            task_id,
            task_cancelled_revision,
            now_ms,
            correlation,
            TaskEvent::TaskCancelled,
        );
        let events = vec![cancellation_requested, run_cancelled, task_cancelled];
        let aggregate = reduce_commit(Some(task.clone()), &events)?;
        persist_projection(&transaction, &aggregate, task.revision(), now_ms)?;
        append_events(&transaction, &events)?;
        let settlement_digest = legacy_recovery_digest(task_id, run_id)?;
        let event_ids = events
            .iter()
            .map(|event| event.header.message_id.clone())
            .collect::<Vec<_>>();
        let changed = transaction.execute(
            "UPDATE legacy_runtime_start_recoveries
             SET state='settled', settled_revision=?3,
                 settled_at_ms=MAX(?4, detected_at_ms), settlement_digest=?5,
                 settlement_event_ids_json=?6
             WHERE task_id=?1 AND run_id=?2 AND state='pending'",
            params![
                task_id.as_str(),
                run_id.as_str(),
                sqlite_integer(aggregate.revision(), "legacy recovery revision")?,
                sqlite_integer(now_ms, "legacy recovery timestamp")?,
                settlement_digest.as_str(),
                serde_json::to_string(&event_ids)?,
            ],
        )?;
        if changed != 1 {
            return Err(corrupt(
                "legacy Runtime recovery marker lost its settlement precondition",
            ));
        }
        transaction.commit()?;
        Ok(())
    }
}
