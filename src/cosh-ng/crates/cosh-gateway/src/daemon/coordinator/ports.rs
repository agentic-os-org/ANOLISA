struct DaemonTaskPorts<'a> {
    coordinator: &'a mut TaskCoordinator,
    scheduler: &'a mut Option<TaskScheduler<Box<dyn RuntimeFactory>>>,
    task_snapshot_driver: &'a mut Option<Box<dyn TaskSnapshotDriver>>,
}

impl TaskCommandPort for DaemonTaskPorts<'_> {
    fn submit(
        &mut self,
        actor: &ActorRef,
        workspace: &WorkspaceRef,
        request: SubmitTask,
    ) -> Result<TaskView, GatewayDaemonError> {
        self.coordinator.submit_admitted(actor, workspace, request)
    }

    fn submit_launch(
        &mut self,
        actor: &ActorRef,
        catalog: &TaskLaunchCatalog,
        request: SubmitLaunch,
    ) -> Result<TaskView, GatewayDaemonError> {
        self.coordinator
            .submit_launch_admitted(actor, catalog, request)
    }

    fn cancel(
        &mut self,
        actor_id: &ActorId,
        request: CancelTask,
    ) -> Result<TaskView, GatewayDaemonError> {
        self.coordinator.cancel(actor_id, request)
    }

    fn retry(
        &mut self,
        actor: &ActorRef,
        catalog: &TaskLaunchCatalog,
        request: RetryTask,
    ) -> Result<TaskView, GatewayDaemonError> {
        let command_digest = digest_json(&("retry", &request.task_id, &request.previous_run_id))?;
        if let Some(receipt) = self.coordinator.store.load_command_receipt(
            &actor.actor_id,
            &request.idempotency_key,
            &command_digest,
        )? {
            let task = self.coordinator.store.load_task(&receipt.task_id)?;
            authorize(&task, &actor.actor_id)?;
            return self.coordinator.task_view(&task);
        }
        let payload = self.coordinator.store.load_runtime_start_intent_for_retry(
            &actor.actor_id,
            &request.task_id,
            &request.previous_run_id,
        )?;
        let intent = scheduler::decode_runtime_start_intent(payload, catalog)?;
        self.coordinator.retry_admitted(
            actor,
            &intent.target,
            &intent.workspace,
            &intent.runtime,
            request,
        )
    }

    fn resolve_approval(
        &mut self,
        actor_id: &ActorId,
        request: ResolveApproval,
    ) -> Result<TaskView, GatewayDaemonError> {
        let scheduler = self.scheduler.as_mut().ok_or_else(|| {
            GatewayDaemonError::Protocol("Gateway scheduler is not attached".to_owned())
        })?;
        match scheduler.resolve_approval(
            actor_id,
            request.idempotency_key,
            &request.approval_id,
            request.decision,
            now_ms()?,
        )? {
            SchedulerTick::Started(view)
            | SchedulerTick::Progressed(view)
            | SchedulerTick::Settled(view) => Ok(view),
            SchedulerTick::Idle => Err(GatewayDaemonError::Protocol(
                "approval resolution made no durable progress".to_owned(),
            )),
        }
    }

    fn resolve_approval_for_task(
        &mut self,
        actor_id: &ActorId,
        request: ResolveApprovalForTask,
    ) -> Result<TaskView, GatewayDaemonError> {
        let scheduler = self.scheduler.as_mut().ok_or_else(|| {
            GatewayDaemonError::Protocol("Gateway scheduler is not attached".to_owned())
        })?;
        match scheduler.resolve_approval_for_task(
            actor_id,
            request.idempotency_key,
            &request.task_id,
            &request.approval_id,
            request.decision,
            now_ms()?,
        )? {
            SchedulerTick::Started(view)
            | SchedulerTick::Progressed(view)
            | SchedulerTick::Settled(view) => Ok(view),
            SchedulerTick::Idle => Err(GatewayDaemonError::Protocol(
                "approval resolution made no durable progress".to_owned(),
            )),
        }
    }

    fn append_input(
        &mut self,
        actor_id: &ActorId,
        request: AppendTaskInput,
    ) -> Result<TaskView, GatewayDaemonError> {
        let scheduler = self.scheduler.as_mut().ok_or_else(|| {
            GatewayDaemonError::Protocol("Gateway scheduler is not attached".to_owned())
        })?;
        match scheduler.resolve_input(
            actor_id,
            request.idempotency_key,
            &request.task_id,
            &request.input_request_id,
            request.response,
            request.expected_revision,
            now_ms()?,
        )? {
            SchedulerTick::Started(view)
            | SchedulerTick::Progressed(view)
            | SchedulerTick::Settled(view) => Ok(view),
            SchedulerTick::Idle => Err(GatewayDaemonError::Protocol(
                "input append made no durable progress".to_owned(),
            )),
        }
    }

    fn switch_snapshot(
        &mut self,
        actor_id: &ActorId,
        request: SwitchTaskSnapshot,
    ) -> Result<TaskSnapshotSwitchView, GatewayDaemonError> {
        let driver = self.task_snapshot_driver.as_mut().ok_or_else(|| {
            GatewayDaemonError::Protocol("Task snapshot provider is not attached".to_owned())
        })?;
        self.coordinator
            .switch_snapshot(actor_id, request, driver.as_mut())
    }
}

impl TaskProjectionPort for DaemonTaskPorts<'_> {
    fn list(&self, actor_id: &ActorId, limit: u16) -> Result<TaskListPage, GatewayDaemonError> {
        self.coordinator.list(actor_id, limit)
    }

    fn get(&self, actor_id: &ActorId, task_id: &TaskId) -> Result<TaskView, GatewayDaemonError> {
        self.coordinator.get(actor_id, task_id)
    }

    fn events(
        &self,
        actor_id: &ActorId,
        task_id: &TaskId,
        after_revision: Option<u64>,
        limit: u16,
    ) -> Result<TaskEventPage, GatewayDaemonError> {
        self.coordinator
            .events(actor_id, task_id, after_revision, limit)
    }

    fn snapshots(
        &mut self,
        actor_id: &ActorId,
        task_id: &TaskId,
    ) -> Result<TaskSnapshotList, GatewayDaemonError> {
        self.coordinator.snapshots(actor_id, task_id)
    }

    fn snapshot_preview(
        &mut self,
        actor_id: &ActorId,
        request: &InspectTaskSnapshot,
    ) -> Result<TaskSnapshotPreview, GatewayDaemonError> {
        let driver = self.task_snapshot_driver.as_mut().ok_or_else(|| {
            GatewayDaemonError::Protocol("Task snapshot provider is not attached".to_owned())
        })?;
        self.coordinator
            .snapshot_preview(actor_id, request, driver.as_mut())
    }
}
