#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskSnapshotSwitchRecord {
    pub(crate) command_digest: Digest,
    pub(crate) task_id: TaskId,
    pub(crate) snapshot_id: CheckpointId,
    pub(crate) preview_digest: Digest,
    pub(crate) expected_revision: u64,
    pub(crate) recovery_snapshot_id: CheckpointId,
    pub(crate) state: String,
    pub(crate) result: Option<TaskSnapshotSwitchView>,
}

impl SqliteTaskStore {
    pub(crate) fn load_task_snapshots(
        &self,
        task_id: &TaskId,
    ) -> Result<Vec<TaskSnapshotView>, StoreError> {
        let mut snapshots = Vec::new();
        let baseline = self.connection().query_row(
            "SELECT baseline_id, run_id FROM pre_runtime_baselines
             WHERE task_id=?1 AND state='created' AND evidence_json IS NOT NULL",
            params![task_id.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ).optional()?;
        if let Some((snapshot_id, run_id)) = baseline {
            snapshots.push(TaskSnapshotView {
                snapshot_id: CheckpointId::parse(&snapshot_id)
                    .map_err(|error| corrupt(&format!("invalid baseline identity: {error}")))?,
                kind: TaskSnapshotKind::Baseline,
                run_id: Some(RunId::parse(&run_id)
                    .map_err(|error| corrupt(&format!("invalid baseline Run identity: {error}")))?),
                approval_id: None,
            });
        }
        let mut statement = self.connection().prepare(
            "SELECT checkpoint_id, run_id, approval_id
             FROM approval_checkpoint_barriers
             WHERE task_id=?1 AND state='created' AND evidence_json IS NOT NULL
             ORDER BY created_at_ms, approval_id",
        )?;
        let rows = statement.query_map(params![task_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?.collect::<Result<Vec<_>, _>>()?;
        for (snapshot_id, run_id, approval_id) in rows {
            snapshots.push(TaskSnapshotView {
                snapshot_id: CheckpointId::parse(&snapshot_id)
                    .map_err(|error| corrupt(&format!("invalid approval checkpoint identity: {error}")))?,
                kind: TaskSnapshotKind::PreEffect,
                run_id: Some(RunId::parse(&run_id)
                    .map_err(|error| corrupt(&format!("invalid approval checkpoint Run: {error}")))?),
                approval_id: Some(cosh_gateway_contracts::ids::ApprovalId::parse(&approval_id)
                    .map_err(|error| corrupt(&format!("invalid checkpoint approval: {error}")))?),
            });
        }
        let mut statement = self.connection().prepare(
            "SELECT operation_json, result_json, run_id
             FROM brokered_execution_results
             WHERE task_id=?1 ORDER BY committed_at_ms, execution_id",
        )?;
        let brokered = statement
            .query_map(params![task_id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (operation_json, result_json, run_id) in brokered {
            use cosh_gateway_contracts::capability::BrokeredOperation;
            use cosh_gateway_contracts::runtime::{
                BrokeredOperationResult, WorkspaceCheckpointCreateV1Outcome,
            };
            let operation: BrokeredOperation = serde_json::from_str(&operation_json)?;
            let result: BrokeredOperationResult = serde_json::from_str(&result_json)?;
            let (
                BrokeredOperation::WorkspaceCheckpointCreateV1(operation),
                BrokeredOperationResult::WorkspaceCheckpointCreateV1(result),
            ) = (operation, result);
            let WorkspaceCheckpointCreateV1Outcome::Created { snapshot_id } = result.outcome else {
                continue;
            };
            if operation.checkpoint_id != result.checkpoint_id
                || snapshot_id.as_str() != result.checkpoint_id.as_str()
            {
                return Err(corrupt("brokered checkpoint result identity diverged"));
            }
            snapshots.push(TaskSnapshotView {
                snapshot_id: result.checkpoint_id,
                kind: TaskSnapshotKind::Brokered,
                run_id: Some(RunId::parse(&run_id).map_err(|error| {
                    corrupt(&format!("invalid brokered checkpoint Run: {error}"))
                })?),
                approval_id: None,
            });
        }
        let mut statement = self.connection().prepare(
            "SELECT recovery_snapshot_id FROM task_snapshot_switches
             WHERE task_id=?1 AND recovery_proven=1
             ORDER BY created_at_ms, recovery_snapshot_id",
        )?;
        let recovery_ids = statement
            .query_map(params![task_id.as_str()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for snapshot_id in recovery_ids {
            snapshots.push(TaskSnapshotView {
                snapshot_id: CheckpointId::parse(&snapshot_id).map_err(|error| {
                    corrupt(&format!("invalid switch recovery checkpoint: {error}"))
                })?,
                kind: TaskSnapshotKind::SwitchRecovery,
                run_id: None,
                approval_id: None,
            });
        }
        Ok(snapshots)
    }

    pub(crate) fn load_task_snapshot_switch(
        &self,
        actor_id: &ActorId,
        key: &IdempotencyKey,
    ) -> Result<Option<TaskSnapshotSwitchRecord>, StoreError> {
        let row = self.connection().query_row(
            "SELECT command_digest, task_id, snapshot_id, preview_digest,
                    expected_revision, recovery_snapshot_id, state, result_json
             FROM task_snapshot_switches WHERE actor_id=?1 AND idempotency_key=?2",
            params![actor_id.as_str(), key.as_str()],
            |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?, row.get::<_, String>(5)?,
                row.get::<_, String>(6)?, row.get::<_, Option<String>>(7)?,
            )),
        ).optional()?;
        row.map(|row| Ok(TaskSnapshotSwitchRecord {
            command_digest: Digest::parse(&row.0).map_err(|error| corrupt(&error.to_string()))?,
            task_id: TaskId::parse(&row.1).map_err(|error| corrupt(&error.to_string()))?,
            snapshot_id: CheckpointId::parse(&row.2).map_err(|error| corrupt(&error.to_string()))?,
            preview_digest: Digest::parse(&row.3).map_err(|error| corrupt(&error.to_string()))?,
            expected_revision: u64::try_from(row.4).map_err(|_| corrupt("negative switch revision"))?,
            recovery_snapshot_id: CheckpointId::parse(&row.5).map_err(|error| corrupt(&error.to_string()))?,
            state: row.6,
            result: row.7.map(|value| serde_json::from_str(&value)).transpose()?,
        })).transpose()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_task_snapshot_switch_intent(
        &mut self,
        actor_id: &ActorId,
        key: &IdempotencyKey,
        command_digest: &Digest,
        task_id: &TaskId,
        snapshot_id: &CheckpointId,
        preview_digest: &Digest,
        expected_revision: u64,
        recovery_snapshot_id: &CheckpointId,
        now_ms: u64,
    ) -> Result<(), StoreError> {
        self.connection_mut().execute(
            "INSERT INTO task_snapshot_switches(
                 actor_id, idempotency_key, command_digest, task_id, snapshot_id,
                 preview_digest, expected_revision, recovery_snapshot_id, state,
                 created_at_ms, updated_at_ms
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'intent',?9,?9)",
            params![actor_id.as_str(), key.as_str(), command_digest.as_str(), task_id.as_str(),
                snapshot_id.as_str(), preview_digest.as_str(),
                sqlite_integer(expected_revision, "snapshot switch revision")?,
                recovery_snapshot_id.as_str(), sqlite_integer(now_ms, "snapshot switch timestamp")?],
        )?;
        Ok(())
    }

    pub(crate) fn transition_task_snapshot_switch(
        &mut self,
        actor_id: &ActorId,
        key: &IdempotencyKey,
        from: &str,
        to: &str,
        result: Option<&TaskSnapshotSwitchView>,
        reason: Option<&str>,
        now_ms: u64,
    ) -> Result<(), StoreError> {
        let changed = self.connection_mut().execute(
            "UPDATE task_snapshot_switches SET state=?4, result_json=?5, reason=?6,
                    updated_at_ms=?7,
                    recovery_proven=CASE WHEN ?3='intent' AND ?4='recovery_created'
                        THEN 1 ELSE recovery_proven END
             WHERE actor_id=?1 AND idempotency_key=?2 AND state=?3",
            params![actor_id.as_str(), key.as_str(), from, to,
                result.map(serde_json::to_string).transpose()?, reason,
                sqlite_integer(now_ms, "snapshot switch timestamp")?],
        )?;
        if changed != 1 {
            return Err(StoreError::LedgerConflict { message: "snapshot switch state changed concurrently".to_owned() });
        }
        Ok(())
    }
}
