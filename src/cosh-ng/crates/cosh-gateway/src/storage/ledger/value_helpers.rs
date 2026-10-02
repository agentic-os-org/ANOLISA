
fn state_name<T: Serialize>(state: T) -> Result<String, StoreError> {
    serde_json::to_value(state)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| corrupt("ledger state is not serialized as a string"))
}

fn validate_command(command: &LedgerCommand) -> Result<(), StoreError> {
    integer(command.committed_at_ms, "ledger timestamp")?;
    Ok(())
}

fn next_integer(value: u64, field: &str) -> Result<u64, StoreError> {
    let next = value
        .checked_add(1)
        .ok_or_else(|| conflict(&format!("{field} overflow")))?;
    integer(next, field)?;
    Ok(next)
}

fn require_not_before(now_ms: u64, previous_ms: u64, operation: &str) -> Result<(), StoreError> {
    if now_ms < previous_ms {
        Err(conflict(&format!(
            "{operation} timestamp precedes the durable entity timestamp",
        )))
    } else {
        Ok(())
    }
}

fn parse_approval_state(value: &str) -> Result<ApprovalState, StoreError> {
    parse_state(value)
}
fn parse_permit_state(value: &str) -> Result<PermitState, StoreError> {
    parse_state(value)
}
fn parse_execution_state(value: &str) -> Result<ExecutionState, StoreError> {
    parse_state(value)
}
fn parse_typed_result_state(value: &str) -> Result<TypedExecutionResultState, StoreError> {
    parse_state(value)
}
fn parse_runtime_state(value: &str) -> Result<RuntimeBindingState, StoreError> {
    parse_state(value)
}

fn parse_state<T: DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned())).map_err(StoreError::from)
}

fn parse_id<T: std::str::FromStr>(value: &str) -> Result<T, StoreError>
where
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| corrupt(&format!("invalid ledger identity: {error}")))
}

fn integer(value: u64, field: &str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| conflict(&format!("{field} exceeds SQLite INTEGER range")))
}

fn unsigned(value: i64, field: &str) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| corrupt(&format!("negative {field}")))
}

fn conflict(message: &str) -> StoreError {
    StoreError::LedgerConflict {
        message: message.to_owned(),
    }
}

fn corrupt(message: &str) -> StoreError {
    StoreError::Corrupt {
        message: message.to_owned(),
    }
}

fn not_found(entity: &str, id: &str) -> StoreError {
    StoreError::LedgerNotFound {
        entity: format!("{entity} {id}"),
    }
}
