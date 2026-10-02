impl SqliteTaskStore {
    /// Loads a bounded newest-first list for one authenticated owner.
    ///
    /// # Errors
    ///
    /// Rejects invalid bounds or any corrupt/divergent Task projection.
    pub fn load_tasks_for_owner(
        &self,
        actor_id: &ActorId,
        limit: u16,
    ) -> Result<Vec<TaskAggregate>, StoreError> {
        if limit == 0 || limit > 64 {
            return Err(invalid("Task list limit must be between 1 and 64"));
        }
        let mut statement = self.connection().prepare(
            "SELECT task_id FROM tasks WHERE owner_actor_id = ?1
             ORDER BY updated_at_ms DESC, task_id DESC LIMIT ?2",
        )?;
        let ids = statement
            .query_map(params![actor_id.as_str(), i64::from(limit)], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                let task_id = TaskId::parse(&id)
                    .map_err(|error| corrupt(&format!("Task row has invalid identity: {error}")))?;
                let task = load_verified_projection(self.connection(), &task_id)?
                    .ok_or_else(|| corrupt("Task list row has no projection"))?;
                if task.owner_actor_id() != actor_id {
                    return Err(corrupt(
                        "Task list owner row diverges from its immutable event owner",
                    ));
                }
                Ok(task)
            })
            .collect()
    }

    /// Loads the latest durable Task projection.
    ///
    /// # Errors
    ///
    /// Returns `TaskNotFound` or rejects a corrupt or divergent projection.
    pub fn load_task(&self, task_id: &TaskId) -> Result<TaskAggregate, StoreError> {
        load_verified_projection(self.connection(), task_id)?.ok_or(StoreError::TaskNotFound)
    }

    /// Returns whether cancellation was requested for the active Run and has
    /// not yet reached a durable Run-cancelled event.
    ///
    /// # Errors
    ///
    /// Returns a mismatch, corruption, or SQLite read error.
    pub fn run_cancellation_requested(
        &self,
        task_id: &TaskId,
        run_id: &cosh_gateway_contracts::ids::RunId,
    ) -> Result<bool, StoreError> {
        let task = self.load_task(task_id)?;
        if task.active_run_id() != Some(run_id) {
            return Err(invalid("cancellation query does not match the active Run"));
        }
        let mut statement = self.connection().prepare(
            "SELECT payload_json FROM task_events
             WHERE task_id=?1 AND event_type IN ('cancellation_requested', 'run_cancelled')
             ORDER BY revision",
        )?;
        let payloads = statement
            .query_map(params![task_id.as_str()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut requested = false;
        for payload in payloads {
            let event = serde_json::from_str::<TaskEventEnvelope>(&payload)
                .map_err(|error| corrupt(&format!("cancellation event cannot decode: {error}")))?;
            match event.event {
                cosh_gateway_contracts::task::TaskEvent::CancellationRequested {
                    run_id: event_run,
                    ..
                } if &event_run == run_id => requested = true,
                cosh_gateway_contracts::task::TaskEvent::RunCancelled {
                    run_id: event_run, ..
                } if &event_run == run_id => requested = false,
                _ => {}
            }
        }
        Ok(requested)
    }

    /// Rebuilds a Task from its immutable events and verifies the stored
    /// projection matches the deterministic reducer result.
    ///
    /// # Errors
    ///
    /// Returns `TaskNotFound` or rejects corrupt, incomplete, or divergent data.
    pub fn recover_task(&self, task_id: &TaskId) -> Result<TaskAggregate, StoreError> {
        self.load_task(task_id)
    }

    /// Loads a bounded page of immutable Task events after a revision cursor.
    ///
    /// # Errors
    ///
    /// Returns `TaskNotFound` when the stream is absent or rejects corrupt
    /// stored events. Authorization remains the coordinator's responsibility.
    pub fn load_task_events_for_owner(
        &self,
        task_id: &TaskId,
        actor_id: &ActorId,
        after_revision: Option<u64>,
        limit: u16,
    ) -> Result<(Vec<TaskEventEnvelope>, u64), StoreError> {
        if limit == 0 || limit > 64 {
            return Err(invalid("Task event page limit must be between 1 and 64"));
        }
        let revision = self
            .connection()
            .query_row(
                "SELECT revision FROM tasks WHERE task_id = ?1 AND owner_actor_id = ?2",
                params![task_id.as_str(), actor_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(revision) = revision else {
            return Err(StoreError::TaskNotFound);
        };
        let revision = u64::try_from(revision).map_err(|_| corrupt("negative Task revision"))?;
        let after_revision = after_revision.unwrap_or(0);
        let after_sql = sqlite_integer(after_revision, "Task event cursor")?;
        let limit_sql = i64::from(limit);
        let mut statement = self.connection().prepare(
            "SELECT revision, payload_json FROM task_events
             WHERE task_id = ?1 AND revision > ?2
             ORDER BY revision ASC LIMIT ?3",
        )?;
        let rows = statement
            .query_map(params![task_id.as_str(), after_sql, limit_sql], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let events = rows
            .into_iter()
            .map(|(stored_revision, payload)| {
                let event = serde_json::from_str::<TaskEventEnvelope>(&payload)
                    .map_err(|error| corrupt(&format!("Task event cannot be decoded: {error}")))?;
                let stored_revision = u64::try_from(stored_revision)
                    .map_err(|_| corrupt("negative Task event revision"))?;
                if event.revision != stored_revision
                    || &event.task_id != task_id
                    || event.header.correlation.actor_id.as_ref() != Some(actor_id)
                {
                    return Err(corrupt(
                        "Task event page row diverges from its identity or owner",
                    ));
                }
                Ok(event)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((events, revision))
    }
}
