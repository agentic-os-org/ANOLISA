use cosh_gateway_contracts::{capability::RuntimeExecutionFence, ids::ApprovalId};

use crate::daemon::{
    ApprovalCheckpointEvidence, ApprovalCheckpointRecord, ApprovalCheckpointState,
};

impl SqliteTaskStore {
    pub(crate) fn record_approval_checkpoint_intent(
        &mut self,
        approval_id: &ApprovalId,
        task_id: &TaskId,
        run_id: &RunId,
        checkpoint_id: &CheckpointId,
        policy: CheckpointPolicy,
        runtime_fence: &RuntimeExecutionFence,
        now_ms: u64,
    ) -> Result<ApprovalCheckpointRecord, StoreError> {
        let policy_name = approval_checkpoint_policy_name(policy)?;
        let changed = self.connection_mut().execute(
            "INSERT OR IGNORE INTO approval_checkpoint_barriers(
                 approval_id, task_id, run_id, checkpoint_id, policy,
                 runtime_fence_json, state, created_at_ms, updated_at_ms
             )
             SELECT approval_id, task_id, run_id, ?4, ?5, ?6, 'intent', ?7, ?7
             FROM approvals
             WHERE approval_id=?1 AND task_id=?2 AND run_id=?3 AND state='pending'
               AND permission_ref_json IS NOT NULL",
            params![
                approval_id.as_str(),
                task_id.as_str(),
                run_id.as_str(),
                checkpoint_id.as_str(),
                policy_name,
                serde_json::to_string(runtime_fence)?,
                sqlite_integer(now_ms, "approval checkpoint timestamp")?,
            ],
        )?;
        let record = self.load_approval_checkpoint_record(approval_id)?;
        if changed == 0
            && (record.task_id != *task_id
                || record.run_id != *run_id
                || record.checkpoint_id != *checkpoint_id
                || record.policy != policy
                || record.runtime_fence != *runtime_fence)
        {
            return Err(StoreError::LedgerConflict {
                message: "approval checkpoint intent binding changed".to_owned(),
            });
        }
        Ok(record)
    }

    pub(crate) fn start_approval_checkpoint(
        &mut self,
        approval_id: &ApprovalId,
        binding: &PreRuntimeCheckpointBinding,
        now_ms: u64,
    ) -> Result<bool, StoreError> {
        let changed = self.connection_mut().execute(
            "UPDATE approval_checkpoint_barriers
             SET state='started', binding_json=?2, updated_at_ms=?3
             WHERE approval_id=?1 AND state='intent' AND binding_json IS NULL",
            params![
                approval_id.as_str(),
                serde_json::to_string(binding)?,
                sqlite_integer(now_ms, "approval checkpoint start timestamp")?,
            ],
        )?;
        Ok(changed == 1)
    }

    pub(crate) fn complete_approval_checkpoint(
        &mut self,
        approval_id: &ApprovalId,
        state: ApprovalCheckpointState,
        evidence: Option<&ApprovalCheckpointEvidence>,
        reason: Option<&BoundedText>,
        now_ms: u64,
    ) -> Result<ApprovalCheckpointRecord, StoreError> {
        let state_name = approval_checkpoint_state_name(state)?;
        let changed = self.connection_mut().execute(
            "UPDATE approval_checkpoint_barriers
             SET state=?2, evidence_json=?3, reason_json=?4, updated_at_ms=?5
             WHERE approval_id=?1 AND state='started' AND binding_json IS NOT NULL",
            params![
                approval_id.as_str(),
                state_name,
                evidence.map(serde_json::to_string).transpose()?,
                reason.map(serde_json::to_string).transpose()?,
                sqlite_integer(now_ms, "approval checkpoint completion timestamp")?,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::LedgerConflict {
                message: "approval checkpoint completion lost its started state".to_owned(),
            });
        }
        self.load_approval_checkpoint_record(approval_id)
    }

    pub(crate) fn complete_approval_checkpoint_intent(
        &mut self,
        approval_id: &ApprovalId,
        state: ApprovalCheckpointState,
        reason: &BoundedText,
        now_ms: u64,
    ) -> Result<ApprovalCheckpointRecord, StoreError> {
        if !matches!(
            state,
            ApprovalCheckpointState::Skipped | ApprovalCheckpointState::Failed
        ) {
            return Err(invalid(
                "pre-effect approval checkpoint terminal is invalid",
            ));
        }
        let changed = self.connection_mut().execute(
            "UPDATE approval_checkpoint_barriers
             SET state=?2, reason_json=?3, updated_at_ms=?4
             WHERE approval_id=?1 AND state='intent' AND binding_json IS NULL",
            params![
                approval_id.as_str(),
                approval_checkpoint_state_name(state)?,
                serde_json::to_string(reason)?,
                sqlite_integer(now_ms, "approval checkpoint completion timestamp")?,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::LedgerConflict {
                message: "approval checkpoint pre-effect completion lost its intent".to_owned(),
            });
        }
        self.load_approval_checkpoint_record(approval_id)
    }

    pub(crate) fn load_approval_checkpoint_record(
        &self,
        approval_id: &ApprovalId,
    ) -> Result<ApprovalCheckpointRecord, StoreError> {
        let row = self
            .connection()
            .query_row(
                "SELECT task_id, run_id, checkpoint_id, policy, runtime_fence_json,
                        state, binding_json, evidence_json, reason_json
                 FROM approval_checkpoint_barriers WHERE approval_id=?1",
                params![approval_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::LedgerNotFound {
                entity: format!("approval checkpoint {}", approval_id.as_str()),
            })?;
        Ok(ApprovalCheckpointRecord {
            approval_id: approval_id.clone(),
            task_id: TaskId::parse(&row.0).map_err(|error| corrupt(&error.to_string()))?,
            run_id: RunId::parse(&row.1).map_err(|error| corrupt(&error.to_string()))?,
            checkpoint_id: CheckpointId::parse(&row.2)
                .map_err(|error| corrupt(&error.to_string()))?,
            policy: approval_checkpoint_policy_from_name(&row.3)?,
            runtime_fence: serde_json::from_str(&row.4)?,
            state: approval_checkpoint_state_from_name(&row.5)?,
            binding: row
                .6
                .map(|value| serde_json::from_str(&value))
                .transpose()?,
            evidence: row
                .7
                .map(|value| serde_json::from_str(&value))
                .transpose()?,
            reason: row
                .8
                .map(|value| serde_json::from_str(&value))
                .transpose()?,
        })
    }
}

fn approval_checkpoint_policy_name(policy: CheckpointPolicy) -> Result<&'static str, StoreError> {
    match policy {
        CheckpointPolicy::Auto => Ok("auto"),
        CheckpointPolicy::On => Ok("on"),
        CheckpointPolicy::Off => Err(invalid("checkpoint Off has no approval barrier")),
    }
}

fn approval_checkpoint_policy_from_name(value: &str) -> Result<CheckpointPolicy, StoreError> {
    match value {
        "auto" => Ok(CheckpointPolicy::Auto),
        "on" => Ok(CheckpointPolicy::On),
        _ => Err(corrupt("invalid approval checkpoint policy")),
    }
}

fn approval_checkpoint_state_name(
    state: ApprovalCheckpointState,
) -> Result<&'static str, StoreError> {
    match state {
        ApprovalCheckpointState::Created => Ok("created"),
        ApprovalCheckpointState::Skipped => Ok("skipped"),
        ApprovalCheckpointState::Unknown => Ok("unknown"),
        ApprovalCheckpointState::Failed => Ok("failed"),
        ApprovalCheckpointState::Intent | ApprovalCheckpointState::Started => {
            Err(invalid("approval checkpoint terminal state required"))
        }
    }
}

fn approval_checkpoint_state_from_name(value: &str) -> Result<ApprovalCheckpointState, StoreError> {
    match value {
        "intent" => Ok(ApprovalCheckpointState::Intent),
        "started" => Ok(ApprovalCheckpointState::Started),
        "created" => Ok(ApprovalCheckpointState::Created),
        "skipped" => Ok(ApprovalCheckpointState::Skipped),
        "unknown" => Ok(ApprovalCheckpointState::Unknown),
        "failed" => Ok(ApprovalCheckpointState::Failed),
        _ => Err(corrupt("invalid approval checkpoint state")),
    }
}
