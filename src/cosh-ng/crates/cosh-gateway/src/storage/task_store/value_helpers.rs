
fn task_state_name(state: TaskState) -> Result<String, StoreError> {
    serde_json::to_value(state)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| corrupt("Task state is not a string"))
}

fn legacy_recovery_digest(task_id: &TaskId, run_id: &RunId) -> Result<Digest, StoreError> {
    let digest = Sha256::digest(
        format!(
            "cosh.gateway.legacy-runtime-start-recovery.v1\0{}\0{}",
            task_id.as_str(),
            run_id.as_str()
        )
        .as_bytes(),
    );
    Digest::parse(format!("{digest:x}"))
        .map_err(|error| invalid(&format!("legacy recovery digest is invalid: {error}")))
}

fn migration_event(
    task_id: &TaskId,
    revision: u64,
    occurred_at_ms: u64,
    correlation: Correlation,
    event: TaskEvent,
) -> TaskEventEnvelope {
    TaskEventEnvelope {
        header: ContractHeader::new(
            ContractSchema::TaskEvent,
            MessageId::new(),
            occurred_at_ms,
            correlation,
        ),
        task_id: task_id.clone(),
        revision,
        event,
    }
}

fn current_time_ms() -> Result<u64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| corrupt("system clock predates Unix epoch during legacy recovery"))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|_| invalid("legacy recovery timestamp exceeds u64 range"))
}

fn sqlite_integer(value: u64, field: &str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| invalid(&format!("{field} exceeds SQLite INTEGER range")))
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidCommit {
        message: message.to_string(),
    }
}
fn corrupt(message: &str) -> StoreError {
    StoreError::Corrupt {
        message: message.to_string(),
    }
}
