/// Single-writer Task lifecycle boundary used by the transport handler.
pub struct TaskCoordinator {
    store: SqliteTaskStore,
    installation_id: InstallationId,
    launch_catalog: TaskLaunchCatalog,
}

impl TaskCoordinator {
    /// Opens durable state for one Gateway installation.
    ///
    /// # Errors
    ///
    /// Returns a fail-closed storage error for unsafe or corrupt state.
    pub fn open(
        database_path: impl AsRef<Path>,
        requested_installation_id: Option<InstallationId>,
    ) -> Result<Self, GatewayDaemonError> {
        Self::open_for_capability_profile(
            database_path,
            requested_installation_id,
            GatewayCapabilityProfile::task_only_v1(),
        )
    }

    fn open_for_capability_profile(
        database_path: impl AsRef<Path>,
        requested_installation_id: Option<InstallationId>,
        expected_profile: GatewayCapabilityProfile,
    ) -> Result<Self, GatewayDaemonError> {
        let mut store = SqliteTaskStore::open(database_path)?;
        let installation_id = store.bind_installation_id(requested_installation_id.as_ref())?;
        let test_workspace = WorkspaceRef {
            scope_digest: sha256_digest(b"cosh.gateway.test.workspace.v1"),
            display_name: None,
        };
        Ok(Self {
            store,
            installation_id,
            launch_catalog: TaskLaunchCatalog::for_legacy_profile(test_workspace, expected_profile),
        })
    }

    fn open_for_launch_catalog(
        database_path: impl AsRef<Path>,
        requested_installation_id: Option<InstallationId>,
        launch_catalog: TaskLaunchCatalog,
    ) -> Result<Self, GatewayDaemonError> {
        let mut store = SqliteTaskStore::open(database_path)?;
        let installation_id = store.bind_installation_id(requested_installation_id.as_ref())?;
        Ok(Self {
            store,
            installation_id,
            launch_catalog,
        })
    }

    fn submit_admitted(
        &mut self,
        actor: &ActorRef,
        workspace: &WorkspaceRef,
        request: SubmitTask,
    ) -> Result<TaskView, GatewayDaemonError> {
        let capability_profile = self
            .launch_catalog
            .legacy_admission(&request.target, &request.runtime)
            .ok_or_else(|| {
                GatewayDaemonError::Protocol(
                    "Task target or Runtime is not admitted by this Gateway catalog".to_owned(),
                )
            })?;
        let task_runtime = if request.runtime.runtime.as_str() == "core" {
            TaskRuntime::Core
        } else {
            TaskRuntime::Codex
        };
        let launch = TaskLaunchSpecV1::new(
            request.intent.clone(),
            task_runtime,
            workspace.clone(),
            CheckpointPolicy::Off,
            if capability_profile == GatewayCapabilityProfile::delegated_acp_v1() {
                ApprovalPolicy::AllowAll
            } else {
                ApprovalPolicy::Interactive
            },
        );
        self.commit_submission(
            actor,
            request.idempotency_key,
            launch,
            request.target,
            request.runtime,
            capability_profile,
        )
    }

    fn submit_launch_admitted(
        &mut self,
        actor: &ActorRef,
        catalog: &TaskLaunchCatalog,
        request: SubmitLaunch,
    ) -> Result<TaskView, GatewayDaemonError> {
        let admission = catalog.admission(&request.launch)?;
        self.commit_submission(
            actor,
            request.idempotency_key,
            request.launch,
            admission.target,
            admission.runtime,
            admission.profile,
        )
    }

    fn commit_submission(
        &mut self,
        actor: &ActorRef,
        idempotency_key: IdempotencyKey,
        launch: TaskLaunchSpecV1,
        target: TargetRef,
        runtime: RuntimeSelector,
        capability_profile: GatewayCapabilityProfile,
    ) -> Result<TaskView, GatewayDaemonError> {
        let actor_id = &actor.actor_id;
        let task_id = TaskId::new();
        let run_id = RunId::new();
        let committed_at_ms = now_ms()?;
        let intent_digest = sha256_digest(launch.goal.as_str().as_bytes());
        let command_digest = digest_json(&("submit_launch", &launch, &target, &runtime))?;
        let submitted = self.event(
            actor_id,
            &task_id,
            None,
            1,
            committed_at_ms,
            TaskEvent::TaskSubmitted {
                intent_digest,
                target,
            },
        );
        let queued = self.event(
            actor_id,
            &task_id,
            Some(&run_id),
            2,
            committed_at_ms,
            TaskEvent::TaskQueued {
                run_id: run_id.clone(),
                runtime: runtime.clone(),
            },
        );
        let baseline_id = if launch.checkpoint == CheckpointPolicy::Off {
            None
        } else {
            Some(CheckpointId::new())
        };
        let start_intent = scheduler::RuntimeStartIntent {
            schema_version: scheduler::RUNTIME_START_SCHEMA_VERSION,
            actor: actor.clone(),
            task_id: task_id.clone(),
            run_id,
            runtime,
            intent: launch.goal.clone(),
            target: submitted_target(&submitted)?,
            workspace: launch.workspace.clone(),
            capability_profile: capability_profile.identity(),
            launch,
            baseline_id,
        };
        let delivery_kind = if start_intent.baseline_id.is_some() {
            scheduler::pre_runtime_checkpoint_delivery_kind()
        } else {
            scheduler::runtime_start_delivery_kind()
        };
        let outbox = OutboxIntent {
            delivery_id: DeliveryId::new(),
            event_id: queued.header.message_id.clone(),
            delivery_kind,
            payload: serde_json::to_value(start_intent)?,
            next_attempt_at_ms: committed_at_ms,
        };
        let outcome = self.store.commit_task(&TaskCommit {
            actor_id: actor_id.clone(),
            idempotency_key,
            command_digest,
            expected_revision: Some(0),
            events: vec![submitted, queued],
            outbox: vec![outbox],
            committed_at_ms,
        })?;
        let task_id = receipt_task_id(&outcome);
        let task = self.store.load_task(task_id)?;
        authorize(&task, actor_id)?;
        self.task_view(&task)
    }

    #[cfg(test)]
    fn submit(
        &mut self,
        actor_id: &ActorId,
        request: SubmitTask,
    ) -> Result<TaskView, GatewayDaemonError> {
        let actor = ActorRef {
            actor_id: actor_id.clone(),
            actor_kind: ActorKind::Human,
            issuer: BoundedName::new("local-os")
                .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
            assurance: AuthAssurance::LocalOs,
        };
        let workspace = WorkspaceRef {
            scope_digest: sha256_digest(b"cosh.gateway.test.workspace.v1"),
            display_name: None,
        };
        self.submit_admitted(&actor, &workspace, request)
    }

    fn get(&self, actor_id: &ActorId, task_id: &TaskId) -> Result<TaskView, GatewayDaemonError> {
        let task = self.store.load_task(task_id)?;
        authorize(&task, actor_id)?;
        self.task_view(&task)
    }

    fn list(&self, actor_id: &ActorId, limit: u16) -> Result<TaskListPage, GatewayDaemonError> {
        let tasks = self
            .store
            .load_tasks_for_owner(actor_id, limit)?
            .iter()
            .map(|task| self.task_view(task))
            .collect::<Result<Vec<_>, _>>()?;
        let page = TaskListPage { tasks };
        if serde_json::to_vec(&page)?.len() > MAX_GATEWAY_FRAME_BYTES.saturating_sub(4096) {
            return Err(GatewayDaemonError::Protocol(
                "Task list exceeds the response byte budget".to_owned(),
            ));
        }
        Ok(page)
    }

    fn events(
        &self,
        actor_id: &ActorId,
        task_id: &TaskId,
        after_revision: Option<u64>,
        limit: u16,
    ) -> Result<TaskEventPage, GatewayDaemonError> {
        let (events, task_revision) =
            self.store
                .load_task_events_for_owner(task_id, actor_id, after_revision, limit)?;
        let next_revision = events
            .last()
            .map_or(after_revision.unwrap_or(0), |event| event.revision);
        let page = TaskEventPage {
            task_id: task_id.clone(),
            has_more: next_revision < task_revision,
            events,
            next_revision,
        };
        if serde_json::to_vec(&page)?.len() > MAX_GATEWAY_FRAME_BYTES.saturating_sub(4096) {
            return Err(GatewayDaemonError::Protocol(
                "Task event page exceeds the response byte budget".to_owned(),
            ));
        }
        Ok(page)
    }

    fn cancel(
        &mut self,
        actor_id: &ActorId,
        request: CancelTask,
    ) -> Result<TaskView, GatewayDaemonError> {
        let command_digest = digest_json(&("cancel", &request.task_id, &request.run_id))?;
        if let Some(receipt) =
            self.store
                .load_command_receipt(actor_id, &request.idempotency_key, &command_digest)?
        {
            let task = self.store.load_task(&receipt.task_id)?;
            authorize(&task, actor_id)?;
            return Ok(TaskView::from(&task));
        }
        let current = self.store.load_task(&request.task_id)?;
        authorize(&current, actor_id)?;
        if current.active_run_id() != Some(&request.run_id) {
            return Err(GatewayDaemonError::Protocol(
                "cancel Run does not match the active Task Run".to_owned(),
            ));
        }
        if !matches!(
            current.state(),
            TaskState::Queued
                | TaskState::Running
                | TaskState::WaitingApproval
                | TaskState::WaitingInput
                | TaskState::Suspended
        ) {
            return Err(GatewayDaemonError::Protocol(
                "Task Run is not cancellable in its current state".to_owned(),
            ));
        }
        if current.state() == TaskState::Suspended
            && !current.active_run_can_be_cancelled(&request.run_id)
        {
            return Err(GatewayDaemonError::Protocol(
                "suspended Task Run has unresolved or uncertain execution state".to_owned(),
            ));
        }
        let committed_at_ms = now_ms()?;
        let first_revision = current.revision().saturating_add(1);
        let requested = self.event(
            actor_id,
            &request.task_id,
            Some(&request.run_id),
            first_revision,
            committed_at_ms,
            TaskEvent::CancellationRequested {
                run_id: request.run_id.clone(),
                cause: CancelReason::UserRequested,
            },
        );
        let settle_without_runtime =
            matches!(current.state(), TaskState::Queued | TaskState::Suspended);
        let events = if settle_without_runtime {
            let run_cancelled = self.event(
                actor_id,
                &request.task_id,
                Some(&request.run_id),
                first_revision.saturating_add(1),
                committed_at_ms,
                TaskEvent::RunCancelled {
                    run_id: request.run_id.clone(),
                    stage: if current.state() == TaskState::Queued {
                        CancellationStage::BeforeRuntime
                    } else {
                        CancellationStage::Runtime
                    },
                },
            );
            let task_cancelled = self.event(
                actor_id,
                &request.task_id,
                Some(&request.run_id),
                first_revision.saturating_add(2),
                committed_at_ms,
                TaskEvent::TaskCancelled,
            );
            vec![requested, run_cancelled, task_cancelled]
        } else {
            vec![requested]
        };
        let commit = TaskCommit {
            actor_id: actor_id.clone(),
            idempotency_key: request.idempotency_key,
            command_digest,
            expected_revision: request.expected_revision.or(Some(current.revision())),
            events,
            outbox: Vec::new(),
            committed_at_ms,
        };
        let outcome = if current.state() == TaskState::Suspended {
            self.store
                .commit_suspended_cancel(&commit, &request.run_id)?
        } else {
            self.store.commit_task(&commit)?
        };
        let task = self.store.load_task(receipt_task_id(&outcome))?;
        Ok(TaskView::from(&task))
    }

    fn retry_admitted(
        &mut self,
        actor: &ActorRef,
        target: &TargetRef,
        workspace: &WorkspaceRef,
        runtime: &RuntimeSelector,
        request: RetryTask,
    ) -> Result<TaskView, GatewayDaemonError> {
        let command_digest = digest_json(&("retry", &request.task_id, &request.previous_run_id))?;
        if let Some(receipt) = self.store.load_command_receipt(
            &actor.actor_id,
            &request.idempotency_key,
            &command_digest,
        )? {
            let task = self.store.load_task(&receipt.task_id)?;
            authorize(&task, &actor.actor_id)?;
            return self.task_view(&task);
        }

        let current = self.store.load_task(&request.task_id)?;
        authorize(&current, &actor.actor_id)?;
        if current.state() != TaskState::Suspended
            || current.active_run_id() != Some(&request.previous_run_id)
            || current.cancellation_requested()
        {
            return Err(GatewayDaemonError::Protocol(
                "only the exact active non-cancelled suspended Run may be retried".to_owned(),
            ));
        }
        if self
            .store
            .load_pre_runtime_baseline(&request.task_id)?
            .is_some_and(|baseline| baseline.state == PreRuntimeBaselineState::Unknown)
        {
            return Err(GatewayDaemonError::Protocol(
                "a Run suspended by uncertain checkpoint creation cannot be retried without exact provider evidence"
                    .to_owned(),
            ));
        }

        let payload = self.store.load_runtime_start_intent_for_retry(
            &actor.actor_id,
            &request.task_id,
            &request.previous_run_id,
        )?;
        let mut start_intent =
            scheduler::decode_runtime_start_intent(payload, &self.launch_catalog)?;
        if start_intent.actor != *actor
            || start_intent.task_id != request.task_id
            || start_intent.run_id != request.previous_run_id
            || current.target() != target
            || start_intent.target != *target
            || start_intent.runtime != *runtime
            || start_intent.workspace != *workspace
        {
            return Err(GatewayDaemonError::Protocol(
                "durable Runtime start intent does not match retry admission".to_owned(),
            ));
        }

        let next_run_id = RunId::new();
        let committed_at_ms = now_ms()?;
        let revision = current
            .revision()
            .checked_add(1)
            .ok_or_else(|| GatewayDaemonError::Protocol("Task revision overflow".to_owned()))?;
        let queued = self.event(
            &actor.actor_id,
            &request.task_id,
            Some(&next_run_id),
            revision,
            committed_at_ms,
            TaskEvent::RunRetryQueued {
                previous_run_id: request.previous_run_id.clone(),
                next_run_id: next_run_id.clone(),
            },
        );
        start_intent.run_id = next_run_id;
        let outbox = OutboxIntent {
            delivery_id: DeliveryId::new(),
            event_id: queued.header.message_id.clone(),
            delivery_kind: scheduler::runtime_start_delivery_kind(),
            payload: serde_json::to_value(start_intent)?,
            next_attempt_at_ms: committed_at_ms,
        };
        let outcome = self.store.commit_retry_task(
            &TaskCommit {
                actor_id: actor.actor_id.clone(),
                idempotency_key: request.idempotency_key,
                command_digest,
                expected_revision: request.expected_revision.or(Some(current.revision())),
                events: vec![queued],
                outbox: vec![outbox],
                committed_at_ms,
            },
            &request.previous_run_id,
        )?;
        let task = self.store.load_task(receipt_task_id(&outcome))?;
        self.task_view(&task)
    }

    fn event(
        &self,
        actor_id: &ActorId,
        task_id: &TaskId,
        run_id: Option<&RunId>,
        revision: u64,
        occurred_at_ms: u64,
        event: TaskEvent,
    ) -> TaskEventEnvelope {
        let mut correlation = Correlation::new(self.installation_id.clone());
        correlation.actor_id = Some(actor_id.clone());
        correlation.task_id = Some(task_id.clone());
        correlation.run_id = run_id.cloned();
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

    fn task_view(&self, task: &TaskAggregate) -> Result<TaskView, GatewayDaemonError> {
        let mut view = TaskView::from(task);
        if let Some((launch, baseline_id)) = self.store.load_task_launch_spec(task.task_id())? {
            view.launch = Some(TaskLaunchDescriptorV1::from_spec(
                sha256_digest(launch.goal.as_str().as_bytes()),
                &launch,
            ));
            view.baseline = match self.store.load_pre_runtime_baseline(task.task_id())? {
                Some(baseline) => Some(baseline),
                None => baseline_id.map(|baseline_id| PreRuntimeBaselineView {
                    baseline_id,
                    policy: launch.checkpoint,
                    state: PreRuntimeBaselineState::Pending,
                    evidence: None,
                    reason: None,
                }),
            };
        }
        Ok(view)
    }

}

include!("coordinator/snapshot.rs");

fn require_task_snapshot_terminal(state: TaskState) -> Result<(), GatewayDaemonError> {
    if matches!(state, TaskState::Succeeded | TaskState::Failed | TaskState::Cancelled) {
        Ok(())
    } else {
        Err(GatewayDaemonError::Protocol(
            "Task snapshots can be switched only after the Task is terminal".to_owned(),
        ))
    }
}

fn submitted_target(event: &TaskEventEnvelope) -> Result<TargetRef, GatewayDaemonError> {
    match &event.event {
        TaskEvent::TaskSubmitted { target, .. } => Ok(target.clone()),
        _ => Err(GatewayDaemonError::Protocol(
            "runtime start intent is not bound to Task submission".to_owned(),
        )),
    }
}

include!("coordinator/ports.rs");
