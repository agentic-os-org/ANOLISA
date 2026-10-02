
fn validate_commit_resource_bounds(commit: &TaskCommit) -> Result<(), StoreError> {
    if commit.events.len() > MAX_TASK_EVENTS_PER_COMMIT {
        return Err(invalid(&format!(
            "event batch exceeds {MAX_TASK_EVENTS_PER_COMMIT} entries"
        )));
    }
    if commit.outbox.len() > MAX_OUTBOX_INTENTS_PER_COMMIT {
        return Err(invalid(&format!(
            "Outbox batch exceeds {MAX_OUTBOX_INTENTS_PER_COMMIT} entries"
        )));
    }

    for event in &commit.events {
        validate_serialized_payload_bytes(serde_json::to_vec(event)?.len(), "Task event")?;
    }
    for intent in &commit.outbox {
        validate_serialized_payload_bytes(serde_json::to_vec(&intent.payload)?.len(), "Outbox")?;
    }
    if serde_json::to_vec(commit)?.len() > MAX_TASK_COMMIT_SERIALIZED_BYTES {
        return Err(invalid(&format!(
            "serialized commit exceeds {MAX_TASK_COMMIT_SERIALIZED_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_serialized_payload_bytes(
    payload_bytes: usize,
    payload_kind: &str,
) -> Result<(), StoreError> {
    if payload_bytes > MAX_TASK_PAYLOAD_BYTES {
        return Err(invalid(&format!(
            "{payload_kind} payload exceeds {MAX_TASK_PAYLOAD_BYTES} serialized bytes"
        )));
    }
    Ok(())
}

fn replay_receipt(
    transaction: &Transaction<'_>,
    commit: &TaskCommit,
) -> Result<Option<CommitOutcome>, StoreError> {
    let existing = transaction
        .query_row(
            "SELECT command_digest, receipt_json FROM command_receipts
             WHERE actor_id = ?1 AND idempotency_key = ?2",
            params![commit.actor_id.as_str(), commit.idempotency_key.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((digest, receipt_json)) = existing else {
        return Ok(None);
    };
    if digest != commit.command_digest.as_str() {
        return Err(StoreError::IdempotencyConflict);
    }
    let receipt = serde_json::from_str::<CommitReceipt>(&receipt_json)?;
    Ok(Some(CommitOutcome::Replayed(receipt)))
}

fn load_snapshot(
    connection: &rusqlite::Connection,
    task_id: &TaskId,
) -> Result<Option<TaskAggregate>, StoreError> {
    let stored = connection
        .query_row(
            "SELECT revision, snapshot_json FROM tasks WHERE task_id = ?1",
            params![task_id.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((revision, snapshot_json)) = stored else {
        return Ok(None);
    };
    let revision = u64::try_from(revision).map_err(|_| corrupt("negative Task revision"))?;
    let aggregate = serde_json::from_str::<TaskAggregate>(&snapshot_json)
        .map_err(|error| corrupt(&format!("Task snapshot cannot be decoded: {error}")))?;
    if aggregate.task_id() != task_id || aggregate.revision() != revision {
        return Err(corrupt(
            "Task snapshot identity or revision does not match its row",
        ));
    }
    Ok(Some(aggregate))
}

pub(super) fn load_verified_projection(
    connection: &rusqlite::Connection,
    task_id: &TaskId,
) -> Result<Option<TaskAggregate>, StoreError> {
    let snapshot = load_snapshot(connection, task_id)?;
    let events = load_events(connection, task_id)?;
    match (snapshot, events.is_empty()) {
        (None, true) => Ok(None),
        (None, false) => Err(corrupt("Task event stream has no projection")),
        (Some(_), true) => Err(corrupt("Task projection has no event stream")),
        (Some(snapshot), false) => {
            let recovered = TaskAggregate::replay(&events)?;
            if recovered != snapshot {
                return Err(corrupt("stored projection diverges from event replay"));
            }
            Ok(Some(recovered))
        }
    }
}

fn load_events(
    connection: &rusqlite::Connection,
    task_id: &TaskId,
) -> Result<Vec<TaskEventEnvelope>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT payload_json FROM task_events
         WHERE task_id = ?1 ORDER BY revision ASC",
    )?;
    let payloads = statement
        .query_map(params![task_id.as_str()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    payloads
        .into_iter()
        .map(|payload| {
            serde_json::from_str::<TaskEventEnvelope>(&payload)
                .map_err(|error| corrupt(&format!("Task event cannot be decoded: {error}")))
        })
        .collect()
}
