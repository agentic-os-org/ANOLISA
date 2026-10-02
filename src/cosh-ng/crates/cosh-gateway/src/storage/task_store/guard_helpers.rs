enum RunCommitGuard<'a> {
    Retry(&'a RunId),
    SuspendedCancel(&'a RunId),
}

fn validate_commit_shape(commit: &TaskCommit) -> Result<(&TaskId, Vec<MessageId>), StoreError> {
    let first = commit
        .events
        .first()
        .ok_or_else(|| invalid("event batch is empty"))?;
    if commit
        .events
        .iter()
        .any(|event| event.task_id != first.task_id)
    {
        return Err(invalid("event batch contains multiple Task identities"));
    }
    if commit
        .events
        .iter()
        .any(|event| event.header.correlation.actor_id.as_ref() != Some(&commit.actor_id))
    {
        return Err(invalid(
            "every event actor correlation must match the admitted commit actor",
        ));
    }
    let event_ids = commit
        .events
        .iter()
        .map(|event| event.header.message_id.clone())
        .collect::<Vec<_>>();
    let unique_event_ids = event_ids.iter().collect::<BTreeSet<_>>();
    if unique_event_ids.len() != event_ids.len() {
        return Err(invalid("event batch reuses a message identity"));
    }
    if commit.outbox.iter().any(|intent| {
        !event_ids
            .iter()
            .any(|event_id| event_id == &intent.event_id)
    }) {
        return Err(invalid(
            "Outbox intent references an event outside the commit",
        ));
    }
    Ok((&first.task_id, event_ids))
}

fn require_retry_run_quiescent(
    transaction: &Transaction<'_>,
    commit: &TaskCommit,
    previous_run_id: &RunId,
) -> Result<(), StoreError> {
    let [event] = commit.events.as_slice() else {
        return Err(StoreError::LedgerConflict {
            message: "retry commit must contain the exact previous Run transition".to_owned(),
        });
    };
    let TaskEvent::RunRetryQueued {
        previous_run_id: event_previous_run_id,
        next_run_id,
    } = &event.event
    else {
        return Err(StoreError::LedgerConflict {
            message: "retry commit must contain the exact previous Run transition".to_owned(),
        });
    };
    if event_previous_run_id != previous_run_id {
        return Err(StoreError::LedgerConflict {
            message: "retry event does not match the guarded previous Run".to_owned(),
        });
    }
    let [intent] = commit.outbox.as_slice() else {
        return Err(StoreError::LedgerConflict {
            message: "retry commit must contain one Runtime start intent".to_owned(),
        });
    };
    if intent.delivery_kind.as_str() != "runtime_start"
        || intent
            .payload
            .pointer("/actor/actor_id")
            .and_then(serde_json::Value::as_str)
            != Some(commit.actor_id.as_str())
        || intent
            .payload
            .get("task_id")
            .and_then(serde_json::Value::as_str)
            != Some(event.task_id.as_str())
        || intent
            .payload
            .get("run_id")
            .and_then(serde_json::Value::as_str)
            != Some(next_run_id.as_str())
    {
        return Err(StoreError::LedgerConflict {
            message: "retry Runtime start intent does not match the next Run".to_owned(),
        });
    }
    require_run_quiescent(transaction, commit, &event.task_id, previous_run_id)
}

fn require_suspended_cancel_quiescent(
    transaction: &Transaction<'_>,
    commit: &TaskCommit,
    run_id: &RunId,
) -> Result<(), StoreError> {
    let [requested, run_cancelled, task_cancelled] = commit.events.as_slice() else {
        return Err(StoreError::LedgerConflict {
            message: "suspended cancel commit must contain its exact terminal transitions"
                .to_owned(),
        });
    };
    if !matches!(
        &requested.event,
        TaskEvent::CancellationRequested { run_id: event_run_id, .. } if event_run_id == run_id
    ) || !matches!(
        &run_cancelled.event,
        TaskEvent::RunCancelled { run_id: event_run_id, .. } if event_run_id == run_id
    ) || !matches!(task_cancelled.event, TaskEvent::TaskCancelled)
        || requested.task_id != run_cancelled.task_id
        || requested.task_id != task_cancelled.task_id
        || !commit.outbox.is_empty()
    {
        return Err(StoreError::LedgerConflict {
            message: "suspended cancel commit does not match the guarded Run".to_owned(),
        });
    }
    require_run_quiescent(transaction, commit, &requested.task_id, run_id)
}

fn require_run_quiescent(
    transaction: &Transaction<'_>,
    commit: &TaskCommit,
    task_id: &TaskId,
    run_id: &RunId,
) -> Result<(), StoreError> {
    let lease = transaction
        .query_row(
            "SELECT task_id, actor_id, expires_at_ms FROM run_leases WHERE run_id=?1",
            params![run_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((lease_task_id, lease_actor_id, expires_at_ms)) = lease else {
        return Err(StoreError::LedgerConflict {
            message: "retry requires the previous Run lease".to_owned(),
        });
    };
    let expires_at_ms = u64::try_from(expires_at_ms)
        .map_err(|_| corrupt("previous Run lease has a negative deadline"))?;
    if lease_task_id != task_id.as_str()
        || lease_actor_id != commit.actor_id.as_str()
        || expires_at_ms > commit.committed_at_ms
    {
        return Err(StoreError::LedgerConflict {
            message: "previous Run lease is live or does not match retry ownership".to_owned(),
        });
    }
    let active_bindings = transaction.query_row(
        "SELECT COUNT(*) FROM runtime_bindings WHERE run_id=?1 AND state='active'",
        params![run_id.as_str()],
        |row| row.get::<_, i64>(0),
    )?;
    if active_bindings != 0 {
        return Err(StoreError::LedgerConflict {
            message: "previous Run still has an active Runtime binding".to_owned(),
        });
    }
    let pending_inputs = transaction.query_row(
        "SELECT COUNT(*) FROM runtime_input_requests
         WHERE run_id=?1 AND state='pending'",
        params![run_id.as_str()],
        |row| row.get::<_, i64>(0),
    )?;
    let unsettled_dispatches = transaction.query_row(
        "SELECT COUNT(*) FROM runtime_input_dispatches
         WHERE run_id=?1 AND state IN ('prepared', 'started')",
        params![run_id.as_str()],
        |row| row.get::<_, i64>(0),
    )?;
    if pending_inputs != 0 || unsettled_dispatches != 0 {
        return Err(StoreError::LedgerConflict {
            message: "previous Run still has unsettled Runtime input".to_owned(),
        });
    }
    Ok(())
}
