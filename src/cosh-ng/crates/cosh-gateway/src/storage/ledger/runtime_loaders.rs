
fn load_runtime_input_request(
    transaction: &rusqlite::Connection,
    request_id: &InputRequestId,
) -> Result<RuntimeInputRequestRecord, StoreError> {
    let row = transaction
        .query_row(
            "SELECT actor_id, task_id, run_id, binding_id, runtime_instance_id,
                    runtime_generation, runtime_sequence, lease_generation, lease_revision,
                    request_json, state, response_digest, revision, expires_at_ms,
                    created_at_ms, updated_at_ms
             FROM runtime_input_requests WHERE request_id=?1",
            params![request_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| not_found("runtime input request", request_id.as_str()))?;
    let request = serde_json::from_str::<RuntimeInputRequest>(&row.9)?;
    let task_id = parse_id::<TaskId>(&row.1)?;
    let run_id = parse_id::<RunId>(&row.2)?;
    if request.request_id() != request_id || request.run_id() != &run_id {
        return Err(corrupt(
            "runtime input request columns diverge from its typed payload",
        ));
    }
    let state = parse_state::<RuntimeInputRequestState>(&row.10)?;
    let response_digest = row
        .11
        .as_deref()
        .map(|value| {
            Digest::parse(value.to_owned())
                .map_err(|error| corrupt(&format!("invalid response digest: {error}")))
        })
        .transpose()?;
    if (state == RuntimeInputRequestState::Resolved) != response_digest.is_some() {
        return Err(corrupt(
            "runtime input request response digest diverges from its state",
        ));
    }
    Ok(RuntimeInputRequestRecord {
        request,
        actor_id: parse_id(&row.0)?,
        task_id,
        run_id,
        binding_id: parse_id(&row.3)?,
        runtime_instance_id: parse_id(&row.4)?,
        runtime_generation: unsigned(row.5, "runtime input generation")?,
        runtime_sequence: unsigned(row.6, "runtime input sequence")?,
        lease_generation: unsigned(row.7, "runtime input lease generation")?,
        lease_revision: unsigned(row.8, "runtime input lease revision")?,
        state,
        response_digest,
        revision: unsigned(row.12, "runtime input request revision")?,
        expires_at_ms: unsigned(row.13, "runtime input deadline")?,
        created_at_ms: unsigned(row.14, "runtime input creation")?,
        updated_at_ms: unsigned(row.15, "runtime input update")?,
    })
}

fn load_runtime_input_dispatch(
    transaction: &rusqlite::Connection,
    request_id: &InputRequestId,
) -> Result<RuntimeInputDispatchRecord, StoreError> {
    let row = transaction
        .query_row(
            "SELECT actor_id, task_id, run_id, response_json, response_digest,
                    state, revision, created_at_ms, updated_at_ms
             FROM runtime_input_dispatches WHERE request_id=?1",
            params![request_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| not_found("runtime input dispatch", request_id.as_str()))?;
    let response = serde_json::from_str::<RuntimeInputResponse>(&row.3)?;
    let response_digest = Digest::parse(row.4)
        .map_err(|error| corrupt(&format!("invalid response digest: {error}")))?;
    if runtime_input_response_digest(&response)? != response_digest {
        return Err(corrupt(
            "runtime input dispatch payload diverges from its digest",
        ));
    }
    Ok(RuntimeInputDispatchRecord {
        request_id: request_id.clone(),
        actor_id: parse_id(&row.0)?,
        task_id: parse_id(&row.1)?,
        run_id: parse_id(&row.2)?,
        response,
        response_digest,
        state: parse_state(&row.5)?,
        revision: unsigned(row.6, "runtime input dispatch revision")?,
        created_at_ms: unsigned(row.7, "runtime input dispatch creation")?,
        updated_at_ms: unsigned(row.8, "runtime input dispatch update")?,
    })
}

fn load_runtime_binding(
    transaction: &rusqlite::Connection,
    id: &RuntimeBindingId,
) -> Result<RuntimeBindingRecord, StoreError> {
    let row = transaction
        .query_row(
            "SELECT actor_id, task_id, run_id, runtime_instance_id, runtime_generation,
         binding_json, state, last_sequence, created_at_ms, updated_at_ms
         FROM runtime_bindings WHERE binding_id=?1",
            params![id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| not_found("runtime binding", id.as_str()))?;
    let binding = serde_json::from_str::<RuntimeBindingRef>(&row.5)?;
    if binding.binding_id != *id
        || binding.task_id.as_str() != row.1
        || binding.run_id.as_str() != row.2
        || binding.runtime_instance_id.as_str() != row.3
        || binding.runtime_generation != unsigned(row.4, "runtime generation")?
    {
        return Err(corrupt(
            "runtime binding columns diverge from the versioned binding contract",
        ));
    }
    Ok(RuntimeBindingRecord {
        binding,
        actor_id: parse_id(&row.0)?,
        state: parse_runtime_state(&row.6)?,
        last_sequence: unsigned(row.7, "runtime sequence")?,
        created_at_ms: unsigned(row.8, "runtime binding creation")?,
        updated_at_ms: unsigned(row.9, "runtime binding update")?,
    })
}

fn load_run_lease_optional(
    transaction: &rusqlite::Connection,
    id: &RunId,
) -> Result<Option<RunLeaseRecord>, StoreError> {
    let row = transaction.query_row(
        "SELECT task_id, actor_id, lease_owner, generation, revision, expires_at_ms, updated_at_ms
         FROM run_leases WHERE run_id=?1", params![id.as_str()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?, row.get::<_, i64>(4)?, row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?)),
    ).optional()?;
    row.map(|row| {
        Ok(RunLeaseRecord {
            task_id: parse_id(&row.0)?,
            run_id: id.clone(),
            actor_id: parse_id(&row.1)?,
            lease_owner: BoundedOpaque::new(row.2).map_err(|_| corrupt("invalid lease owner"))?,
            generation: unsigned(row.3, "lease generation")?,
            revision: unsigned(row.4, "lease revision")?,
            expires_at_ms: unsigned(row.5, "lease deadline")?,
            updated_at_ms: unsigned(row.6, "lease update")?,
        })
    })
    .transpose()
}
