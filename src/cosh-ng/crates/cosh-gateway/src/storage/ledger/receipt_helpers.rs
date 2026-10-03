
fn replay<T: DeserializeOwned>(
    transaction: &Transaction<'_>,
    command: &LedgerCommand,
    operation: &str,
) -> Result<Option<T>, StoreError> {
    let used_by_task = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM command_receipts
         WHERE actor_id=?1 AND idempotency_key=?2)",
        params![command.actor_id.as_str(), command.idempotency_key.as_str()],
        |row| row.get::<_, bool>(0),
    )?;
    if used_by_task {
        return Err(StoreError::IdempotencyConflict);
    }
    let row = transaction
        .query_row(
            "SELECT command_digest, operation, result_json FROM ledger_receipts
         WHERE actor_id=?1 AND idempotency_key=?2",
            params![command.actor_id.as_str(), command.idempotency_key.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((digest, stored_operation, result)) = row else {
        return Ok(None);
    };
    if digest != command.command_digest.as_str() || stored_operation != operation {
        return Err(StoreError::IdempotencyConflict);
    }
    Ok(Some(serde_json::from_str(&result)?))
}

fn insert_receipt<T: Serialize>(
    transaction: &Transaction<'_>,
    command: &LedgerCommand,
    operation: &str,
    result: &T,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO ledger_receipts(actor_id, idempotency_key, command_digest, operation,
         result_json, committed_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            command.actor_id.as_str(),
            command.idempotency_key.as_str(),
            command.command_digest.as_str(),
            operation,
            serde_json::to_string(result)?,
            integer(command.committed_at_ms, "ledger timestamp")?
        ],
    )?;
    Ok(())
}
