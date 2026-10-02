impl TaskCoordinator {
    fn snapshots(
        &self,
        actor_id: &ActorId,
        task_id: &TaskId,
    ) -> Result<TaskSnapshotList, GatewayDaemonError> {
        let task = self.store.load_task(task_id)?;
        authorize(&task, actor_id)?;
        let (launch, _) = self
            .store
            .load_task_launch_spec(task_id)?
            .ok_or_else(|| GatewayDaemonError::Protocol("Task has no managed launch".to_owned()))?;
        Ok(TaskSnapshotList {
            task_id: task_id.clone(),
            state: task.state(),
            revision: task.revision(),
            workspace: launch.workspace,
            snapshots: self.store.load_task_snapshots(task_id)?,
        })
    }

    fn snapshot_preview(
        &self,
        actor_id: &ActorId,
        request: &InspectTaskSnapshot,
        driver: &mut dyn TaskSnapshotDriver,
    ) -> Result<TaskSnapshotPreview, GatewayDaemonError> {
        let list = self.snapshots(actor_id, &request.task_id)?;
        if !list
            .snapshots
            .iter()
            .any(|snapshot| snapshot.snapshot_id == request.snapshot_id)
        {
            return Err(GatewayDaemonError::Protocol(
                "snapshot is not a proven-created checkpoint owned by this Task".to_owned(),
            ));
        }
        let mut provider = driver
            .preview(&TaskSnapshotProviderRequest {
                task_id: request.task_id.clone(),
                snapshot_id: request.snapshot_id.clone(),
                workspace: list.workspace.clone(),
            })
            .map_err(|error| {
                GatewayDaemonError::Protocol(error.safe_message.as_str().to_owned())
            })?;
        // Reserve half the frame for the envelope and bounded Task/workspace fields.
        // Count serialized bytes so escaped paths and details cannot exceed the budget.
        let mut bytes = 2;
        let mut shown = 0;
        for change in &provider.changes {
            bytes += serde_json::to_vec(change)?.len() + 1;
            if bytes > MAX_GATEWAY_FRAME_BYTES / 2 {
                break;
            }
            shown += 1;
        }
        let changes_omitted = provider.changes.len() - shown;
        provider.changes.truncate(shown);
        Ok(TaskSnapshotPreview {
            task_id: request.task_id.clone(),
            state: list.state,
            revision: list.revision,
            workspace: list.workspace,
            snapshot_id: request.snapshot_id.clone(),
            changes: provider.changes,
            changes_omitted,
            preview_digest: provider.preview_digest,
        })
    }

    fn switch_snapshot(
        &mut self,
        actor_id: &ActorId,
        request: SwitchTaskSnapshot,
        driver: &mut dyn TaskSnapshotDriver,
    ) -> Result<TaskSnapshotSwitchView, GatewayDaemonError> {
        let command_digest = digest_json(&(
            "switch_task_snapshot",
            &request.task_id,
            &request.snapshot_id,
            &request.preview_digest,
            request.expected_revision,
        ))?;
        if let Some(record) = self
            .store
            .load_task_snapshot_switch(actor_id, &request.idempotency_key)?
        {
            if record.command_digest != command_digest {
                return Err(GatewayDaemonError::Protocol(
                    "idempotency key was already used for another snapshot switch".to_owned(),
                ));
            }
            return match (record.state.as_str(), record.result.as_ref()) {
                ("succeeded", Some(result)) => Ok(result.clone()),
                ("switch_started" | "unknown", _) => Err(GatewayDaemonError::Protocol(format!(
                    "snapshot switch outcome is uncertain; do not retry; recovery snapshot is {}",
                    record.recovery_snapshot_id.as_str()
                ))),
                ("intent" | "recovery_created", _) => {
                    self.resume_snapshot_switch(actor_id, request, record, driver)
                }
                _ => Err(GatewayDaemonError::Protocol(
                    "snapshot switch cannot be resumed from its durable terminal state".to_owned(),
                )),
            };
        }

        let preview = self.snapshot_preview(
            actor_id,
            &InspectTaskSnapshot {
                task_id: request.task_id.clone(),
                snapshot_id: request.snapshot_id.clone(),
            },
            driver,
        )?;
        require_task_snapshot_terminal(preview.state)?;
        if preview.revision != request.expected_revision {
            return Err(GatewayDaemonError::Protocol(
                "Task revision changed after snapshot preview".to_owned(),
            ));
        }
        if preview.preview_digest != request.preview_digest {
            return Err(GatewayDaemonError::Protocol(
                "workspace changed after snapshot preview; preview again".to_owned(),
            ));
        }
        let recovery_snapshot_id = CheckpointId::new();
        let now = now_ms()?;
        self.store.record_task_snapshot_switch_intent(
            actor_id,
            &request.idempotency_key,
            &command_digest,
            &request.task_id,
            &request.snapshot_id,
            &request.preview_digest,
            request.expected_revision,
            &recovery_snapshot_id,
            now,
        )?;
        let record = self
            .store
            .load_task_snapshot_switch(actor_id, &request.idempotency_key)?
            .ok_or_else(|| GatewayDaemonError::Protocol("snapshot switch intent missing".to_owned()))?;
        self.resume_snapshot_switch(actor_id, request, record, driver)
    }

    fn resume_snapshot_switch(
        &mut self,
        actor_id: &ActorId,
        request: SwitchTaskSnapshot,
        record: crate::storage::TaskSnapshotSwitchRecord,
        driver: &mut dyn TaskSnapshotDriver,
    ) -> Result<TaskSnapshotSwitchView, GatewayDaemonError> {
        let task = self.store.load_task(&request.task_id)?;
        authorize(&task, actor_id)?;
        require_task_snapshot_terminal(task.state())?;
        if task.revision() != request.expected_revision {
            return Err(GatewayDaemonError::Protocol(
                "Task revision changed before snapshot switch".to_owned(),
            ));
        }
        let (launch, _) = self.store.load_task_launch_spec(&request.task_id)?
            .ok_or_else(|| GatewayDaemonError::Protocol("Task has no managed launch".to_owned()))?;
        let provider_request = TaskSnapshotProviderRequest {
            task_id: request.task_id.clone(),
            snapshot_id: request.snapshot_id.clone(),
            workspace: launch.workspace,
        };
        if matches!(record.state.as_str(), "intent" | "recovery_created") {
            let current = driver
                .preview(&provider_request)
                .map_err(|error| {
                    GatewayDaemonError::Protocol(error.safe_message.as_str().to_owned())
                })?;
            if current.preview_digest != record.preview_digest {
                self.store.transition_task_snapshot_switch(
                    actor_id,
                    &request.idempotency_key,
                    &record.state,
                    "failed",
                    None,
                    Some("workspace changed after snapshot preview"),
                    now_ms()?,
                )?;
                return Err(GatewayDaemonError::Protocol(
                    "workspace changed after snapshot preview; preview again".to_owned(),
                ));
            }
        }
        if record.state == "intent" {
            if let Err(error) = driver.create_recovery(
                &provider_request,
                &record.recovery_snapshot_id,
                &request.preview_digest,
            ) {
                self.store.transition_task_snapshot_switch(
                    actor_id, &request.idempotency_key, "intent", "failed", None,
                    Some(error.safe_message.as_str()), now_ms()?,
                )?;
                return Err(GatewayDaemonError::Protocol(format!(
                    "recovery snapshot could not be proven: {}",
                    error.safe_message.as_str()
                )));
            }
            self.store.transition_task_snapshot_switch(
                actor_id, &request.idempotency_key, "intent", "recovery_created", None, None,
                now_ms()?,
            )?;
        }
        let current = driver.preview(&provider_request).map_err(|error| {
            GatewayDaemonError::Protocol(error.safe_message.as_str().to_owned())
        })?;
        if current.preview_digest != record.preview_digest {
            self.store.transition_task_snapshot_switch(
                actor_id,
                &request.idempotency_key,
                "recovery_created",
                "failed",
                None,
                Some("workspace changed after recovery snapshot creation"),
                now_ms()?,
            )?;
            return Err(GatewayDaemonError::Protocol(
                "workspace changed while preparing the switch; recovery exists but switch was not attempted"
                    .to_owned(),
            ));
        }
        self.store.transition_task_snapshot_switch(
            actor_id, &request.idempotency_key, "recovery_created", "switch_started", None, None,
            now_ms()?,
        )?;
        let switched = match driver.switch(
            &provider_request,
            &record.preview_digest,
            &record.recovery_snapshot_id,
            &record.command_digest,
        ) {
            Ok(TaskSnapshotProviderSwitchResult::Switched(switched)) => switched,
            Ok(TaskSnapshotProviderSwitchResult::Rejected { reason }) => {
                self.store.transition_task_snapshot_switch(
                    actor_id,
                    &request.idempotency_key,
                    "switch_started",
                    "failed",
                    None,
                    Some(reason.as_str()),
                    now_ms()?,
                )?;
                return Err(GatewayDaemonError::Protocol(format!(
                    "snapshot switch was rejected before changing the workspace; recovery snapshot is {}: {}",
                    record.recovery_snapshot_id.as_str(),
                    reason.as_str()
                )));
            }
            Ok(TaskSnapshotProviderSwitchResult::PossiblyApplied { error }) => {
                self.store.transition_task_snapshot_switch(
                    actor_id,
                    &request.idempotency_key,
                    "switch_started",
                    "unknown",
                    None,
                    Some(error.safe_message.as_str()),
                    now_ms()?,
                )?;
                return Err(GatewayDaemonError::Protocol(format!(
                    "snapshot switch outcome is uncertain; do not retry; recovery snapshot is {}: {}",
                    record.recovery_snapshot_id.as_str(),
                    error.safe_message.as_str()
                )));
            }
            Err(error) => {
                self.store.transition_task_snapshot_switch(
                    actor_id, &request.idempotency_key, "switch_started", "failed", None,
                    Some(error.safe_message.as_str()), now_ms()?,
                )?;
                return Err(GatewayDaemonError::Protocol(format!(
                    "snapshot switch was rejected before provider dispatch; recovery snapshot is {}: {}",
                    record.recovery_snapshot_id.as_str(), error.safe_message.as_str()
                )));
            }
        };
        let result = TaskSnapshotSwitchView {
            task_id: request.task_id,
            snapshot_id: request.snapshot_id,
            recovery_snapshot_id: record.recovery_snapshot_id,
            from: switched.from,
            to: switched.to,
        };
        self.store.transition_task_snapshot_switch(
            actor_id, &request.idempotency_key, "switch_started", "succeeded", Some(&result), None,
            now_ms()?,
        )?;
        Ok(result)
    }
}
