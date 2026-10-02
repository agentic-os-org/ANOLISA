
fn reduce_commit(
    current: Option<TaskAggregate>,
    events: &[TaskEventEnvelope],
) -> Result<TaskAggregate, StoreError> {
    match current {
        Some(mut aggregate) => {
            for event in events {
                aggregate.apply(event)?;
            }
            Ok(aggregate)
        }
        None => Ok(TaskAggregate::replay(events)?),
    }
}

fn persist_projection(
    transaction: &Transaction<'_>,
    aggregate: &TaskAggregate,
    previous_revision: u64,
    committed_at_ms: u64,
) -> Result<(), StoreError> {
    let revision = sqlite_integer(aggregate.revision(), "Task revision")?;
    let previous_revision = sqlite_integer(previous_revision, "previous Task revision")?;
    let committed_at_ms = sqlite_integer(committed_at_ms, "commit timestamp")?;
    let snapshot_json = serde_json::to_string(aggregate)?;
    let target_ref = serde_json::to_string(aggregate.target())?;
    let state = task_state_name(aggregate.state())?;
    if previous_revision == 0 {
        transaction.execute(
            "INSERT INTO tasks(
                 task_id, owner_actor_id, target_ref, revision, state,
                 snapshot_json, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                aggregate.task_id().as_str(),
                aggregate.owner_actor_id().as_str(),
                target_ref,
                revision,
                state,
                snapshot_json,
                committed_at_ms,
            ],
        )?;
    } else {
        let changed = transaction.execute(
            "UPDATE tasks SET revision = ?2, state = ?3, snapshot_json = ?4,
                 updated_at_ms = ?5
             WHERE task_id = ?1 AND revision = ?6",
            params![
                aggregate.task_id().as_str(),
                revision,
                state,
                snapshot_json,
                committed_at_ms,
                previous_revision,
            ],
        )?;
        if changed != 1 {
            return Err(corrupt("Task projection compare-and-swap changed no row"));
        }
    }
    Ok(())
}

fn append_events(
    transaction: &Transaction<'_>,
    events: &[TaskEventEnvelope],
) -> Result<(), StoreError> {
    let mut statement = transaction.prepare(
        "INSERT INTO task_events(
             event_id, task_id, revision, event_type, schema_version,
             payload_json, occurred_at_ms, causation_id, correlation_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for event in events {
        let revision = sqlite_integer(event.revision, "event revision")?;
        let occurred_at_ms = sqlite_integer(event.header.occurred_at_ms, "event timestamp")?;
        let payload_json = serde_json::to_string(event)?;
        let event_type = serde_json::to_value(event.event.kind())?
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| corrupt("Task event kind is not a string"))?;
        statement.execute(params![
            event.header.message_id.as_str(),
            event.task_id.as_str(),
            revision,
            event_type,
            i64::from(event.header.schema_version),
            payload_json,
            occurred_at_ms,
            event
                .header
                .correlation
                .causation_message_id
                .as_ref()
                .map(MessageId::as_str),
            Option::<&str>::None,
        ])?;
    }
    Ok(())
}

pub(super) fn append_internal_task_event(
    transaction: &Transaction<'_>,
    task_id: &TaskId,
    actor_id: &ActorId,
    committed_at_ms: u64,
    event: TaskEvent,
    outbox: Option<(BoundedName, serde_json::Value)>,
) -> Result<TaskEventEnvelope, StoreError> {
    let serialized_outbox = outbox
        .as_ref()
        .map(|(_, payload)| {
            let payload_json = serde_json::to_string(payload)?;
            validate_serialized_payload_bytes(payload_json.len(), "internal Outbox")?;
            Ok::<_, StoreError>(payload_json)
        })
        .transpose()?;
    let task = load_verified_projection(transaction, task_id)?.ok_or(StoreError::TaskNotFound)?;
    if task.owner_actor_id() != actor_id {
        return Err(invalid("internal Task event actor does not own the Task"));
    }
    let previous = load_events(transaction, task_id)?
        .into_iter()
        .last()
        .ok_or_else(|| corrupt("internal Task event has no immutable predecessor"))?;
    let mut correlation = previous.header.correlation.clone();
    correlation.actor_id = Some(actor_id.clone());
    correlation.task_id = Some(task_id.clone());
    correlation.run_id = task.active_run_id().cloned();
    correlation.causation_message_id = Some(previous.header.message_id);
    let revision = task
        .revision()
        .checked_add(1)
        .ok_or_else(|| corrupt("internal Task event revision overflow"))?;
    let envelope = migration_event(task_id, revision, committed_at_ms, correlation, event);
    let aggregate = reduce_commit(Some(task.clone()), std::slice::from_ref(&envelope))?;
    persist_projection(transaction, &aggregate, task.revision(), committed_at_ms)?;
    append_events(transaction, std::slice::from_ref(&envelope))?;
    if let Some(((delivery_kind, _), payload_json)) = outbox.zip(serialized_outbox) {
        transaction.execute(
            "INSERT INTO outbox(
                 delivery_id, task_id, event_id, delivery_kind, payload_json,
                 state, next_attempt_at_ms, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?6)",
            params![
                DeliveryId::new().as_str(),
                task_id.as_str(),
                envelope.header.message_id.as_str(),
                delivery_kind.as_str(),
                payload_json,
                sqlite_integer(committed_at_ms, "internal Outbox timestamp")?,
            ],
        )?;
    }
    Ok(envelope)
}

fn append_outbox(
    transaction: &Transaction<'_>,
    task_id: &TaskId,
    commit: &TaskCommit,
) -> Result<(), StoreError> {
    let created_at_ms = sqlite_integer(commit.committed_at_ms, "Outbox timestamp")?;
    let mut statement = transaction.prepare(
        "INSERT INTO outbox(
             delivery_id, task_id, event_id, delivery_kind, payload_json,
             state, next_attempt_at_ms, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7)",
    )?;
    for intent in &commit.outbox {
        let next_attempt_at_ms =
            sqlite_integer(intent.next_attempt_at_ms, "Outbox next-attempt timestamp")?;
        statement.execute(params![
            intent.delivery_id.as_str(),
            task_id.as_str(),
            intent.event_id.as_str(),
            intent.delivery_kind.as_str(),
            serde_json::to_string(&intent.payload)?,
            next_attempt_at_ms,
            created_at_ms,
        ])?;
    }
    Ok(())
}

fn insert_receipt(
    transaction: &Transaction<'_>,
    commit: &TaskCommit,
    receipt: &CommitReceipt,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO command_receipts(
             actor_id, idempotency_key, command_digest, task_id,
             task_revision, receipt_json, committed_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            commit.actor_id.as_str(),
            commit.idempotency_key.as_str(),
            commit.command_digest.as_str(),
            receipt.task_id.as_str(),
            sqlite_integer(receipt.revision, "receipt Task revision")?,
            serde_json::to_string(receipt)?,
            sqlite_integer(commit.committed_at_ms, "receipt timestamp")?,
        ],
    )?;
    Ok(())
}
