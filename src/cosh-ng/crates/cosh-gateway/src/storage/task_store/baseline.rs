use crate::daemon::{
    PreRuntimeBaselineState, PreRuntimeBaselineView, PreRuntimeCheckpointBinding,
    PreRuntimeCheckpointEvidence,
};
use cosh_gateway_contracts::{common::BoundedText, task::CheckpointPolicy};

pub(crate) struct PreRuntimeBaselineRecord {
    pub(crate) view: PreRuntimeBaselineView,
    pub(crate) binding: Option<PreRuntimeCheckpointBinding>,
}

impl SqliteTaskStore {
    pub(crate) fn load_task_launch_spec(
        &self,
        task_id: &TaskId,
    ) -> Result<
        Option<(
            cosh_gateway_contracts::task::TaskLaunchSpecV1,
            Option<CheckpointId>,
        )>,
        StoreError,
    > {
        let payload = self
            .connection()
            .query_row(
                "SELECT payload_json FROM outbox
             WHERE task_id=?1 AND delivery_kind IN ('pre_runtime_checkpoint', 'runtime_start')
             ORDER BY created_at_ms, delivery_id LIMIT 1",
                params![task_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        #[derive(Deserialize)]
        struct LaunchProjection {
            launch: cosh_gateway_contracts::task::TaskLaunchSpecV1,
            baseline_id: Option<CheckpointId>,
        }
        let value: serde_json::Value = serde_json::from_str(&payload)?;
        if value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(4)
        {
            return Ok(None);
        }
        let projection: LaunchProjection = serde_json::from_value(value)?;
        Ok(Some((projection.launch, projection.baseline_id)))
    }

    pub(crate) fn record_pre_runtime_baseline_started(
        &mut self,
        claim: &OutboxClaim,
        baseline_id: &CheckpointId,
        run_id: &RunId,
        policy: CheckpointPolicy,
        binding: Option<&PreRuntimeCheckpointBinding>,
        now_ms: u64,
    ) -> Result<bool, StoreError> {
        let policy = baseline_policy_name(policy)?;
        let changed = self.connection_mut().execute(
            "INSERT OR IGNORE INTO pre_runtime_baselines(
                 task_id, run_id, baseline_id, policy, state,
                 binding_json, created_at_ms, updated_at_ms
             )
             SELECT task_id, ?2, ?3, ?4, 'started', ?5, ?6, ?6
             FROM outbox
             WHERE delivery_id=?1 AND task_id=?7 AND state='leased'
               AND attempt=?8 AND lease_owner=?9 AND lease_expires_at_ms > ?6",
            params![
                claim.delivery_id.as_str(),
                run_id.as_str(),
                baseline_id.as_str(),
                policy,
                binding.map(serde_json::to_string).transpose()?,
                sqlite_integer(now_ms, "baseline start timestamp")?,
                claim.task_id.as_str(),
                sqlite_integer(claim.attempt, "baseline Outbox attempt")?,
                claim.lease_owner.as_str(),
            ],
        )?;
        if changed == 1 {
            return Ok(true);
        }
        let existing = self.load_pre_runtime_baseline(&claim.task_id)?;
        if existing
            .as_ref()
            .is_some_and(|view| view.baseline_id == *baseline_id)
        {
            Ok(false)
        } else {
            Err(StoreError::GenerationFenced {
                expected: claim.attempt,
                actual: 0,
            })
        }
    }

    pub(crate) fn complete_pre_runtime_baseline(
        &mut self,
        claim: &OutboxClaim,
        state: PreRuntimeBaselineState,
        evidence: Option<&PreRuntimeCheckpointEvidence>,
        reason: Option<&BoundedText>,
        runtime_delivery: Option<&OutboxIntent>,
        lifecycle_events: &[TaskEvent],
        now_ms: u64,
    ) -> Result<(), StoreError> {
        let state_name = baseline_state_name(state)?;
        let now = sqlite_integer(now_ms, "baseline completion timestamp")?;
        let attempt = sqlite_integer(claim.attempt, "baseline Outbox attempt")?;
        let transaction = self
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE pre_runtime_baselines
             SET state=?2, evidence_json=?3, reason_json=?4, updated_at_ms=?5
             WHERE task_id=?1 AND state='started'",
            params![
                claim.task_id.as_str(),
                state_name,
                evidence.map(serde_json::to_string).transpose()?,
                reason.map(serde_json::to_string).transpose()?,
                now,
            ],
        )?;
        if changed != 1 {
            return Err(corrupt("baseline completion lost its started state"));
        }
        let changed = transaction.execute(
            "UPDATE outbox
             SET state='delivered', lease_owner=NULL, lease_expires_at_ms=NULL,
                 delivered_at_ms=?2
             WHERE delivery_id=?1 AND state='leased' AND attempt=?3
               AND lease_owner=?4 AND lease_expires_at_ms > ?2",
            params![
                claim.delivery_id.as_str(),
                now,
                attempt,
                claim.lease_owner.as_str(),
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::GenerationFenced {
                expected: claim.attempt,
                actual: 0,
            });
        }
        if let Some(intent) = runtime_delivery {
            transaction.execute(
                "INSERT INTO outbox(
                     delivery_id, task_id, event_id, delivery_kind, payload_json,
                     state, next_attempt_at_ms, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?6)",
                params![
                    intent.delivery_id.as_str(),
                    claim.task_id.as_str(),
                    intent.event_id.as_str(),
                    intent.delivery_kind.as_str(),
                    serde_json::to_string(&intent.payload)?,
                    now,
                ],
            )?;
        }
        if !lifecycle_events.is_empty() {
            let task = load_verified_projection(&transaction, &claim.task_id)?
                .ok_or(StoreError::TaskNotFound)?;
            let actor_id = task.owner_actor_id().clone();
            for event in lifecycle_events {
                append_internal_task_event(
                    &transaction,
                    &claim.task_id,
                    &actor_id,
                    now_ms,
                    event.clone(),
                    None,
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn load_pre_runtime_baseline(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<PreRuntimeBaselineView>, StoreError> {
        Ok(self
            .load_pre_runtime_baseline_record(task_id)?
            .map(|record| record.view))
    }

    pub(crate) fn load_pre_runtime_baseline_record(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<PreRuntimeBaselineRecord>, StoreError> {
        let row = self
            .connection()
            .query_row(
                "SELECT baseline_id, policy, state, evidence_json, reason_json, binding_json
             FROM pre_runtime_baselines WHERE task_id=?1",
                params![task_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((baseline_id, policy, state, evidence, reason, binding)) = row else {
            return Ok(None);
        };
        Ok(Some(PreRuntimeBaselineRecord {
            view: PreRuntimeBaselineView {
                baseline_id: CheckpointId::parse(&baseline_id)
                    .map_err(|error| corrupt(&format!("invalid baseline identity: {error}")))?,
                policy: policy_from_name(&policy)?,
                state: baseline_state_from_name(&state)?,
                evidence: evidence
                    .map(|value| serde_json::from_str(&value))
                    .transpose()?,
                reason: reason
                    .map(|value| serde_json::from_str(&value))
                    .transpose()?,
            },
            binding: binding
                .map(|value| serde_json::from_str(&value))
                .transpose()?,
        }))
    }
}

fn baseline_policy_name(policy: CheckpointPolicy) -> Result<&'static str, StoreError> {
    match policy {
        CheckpointPolicy::Auto => Ok("auto"),
        CheckpointPolicy::On => Ok("on"),
        CheckpointPolicy::Off => Err(invalid("checkpoint Off cannot create a baseline")),
    }
}

fn policy_from_name(value: &str) -> Result<CheckpointPolicy, StoreError> {
    match value {
        "auto" => Ok(CheckpointPolicy::Auto),
        "on" => Ok(CheckpointPolicy::On),
        _ => Err(corrupt("invalid durable baseline policy")),
    }
}

fn baseline_state_name(state: PreRuntimeBaselineState) -> Result<&'static str, StoreError> {
    match state {
        PreRuntimeBaselineState::Started => Ok("started"),
        PreRuntimeBaselineState::Created => Ok("created"),
        PreRuntimeBaselineState::Skipped => Ok("skipped"),
        PreRuntimeBaselineState::Unknown => Ok("unknown"),
        PreRuntimeBaselineState::Failed => Ok("failed"),
        PreRuntimeBaselineState::Pending => {
            Err(invalid("pending baseline is represented by Outbox"))
        }
    }
}

fn baseline_state_from_name(value: &str) -> Result<PreRuntimeBaselineState, StoreError> {
    match value {
        "started" => Ok(PreRuntimeBaselineState::Started),
        "created" => Ok(PreRuntimeBaselineState::Created),
        "skipped" => Ok(PreRuntimeBaselineState::Skipped),
        "unknown" => Ok(PreRuntimeBaselineState::Unknown),
        "failed" => Ok(PreRuntimeBaselineState::Failed),
        _ => Err(corrupt("invalid durable baseline state")),
    }
}
