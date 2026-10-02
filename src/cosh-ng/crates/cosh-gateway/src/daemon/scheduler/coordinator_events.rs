impl TaskCoordinator {
    fn request_runtime_shutdown(
        &mut self,
        claim: &LeaseClaim,
        now_ms: u64,
    ) -> Result<(), GatewayDaemonError> {
        if self
            .store
            .run_cancellation_requested(&claim.task_id, &claim.run_id)?
        {
            return Ok(());
        }
        self.require_current_lease(claim, now_ms)?;
        let task = self.store.load_task(&claim.task_id)?;
        let event = self.event(
            task.owner_actor_id(),
            &claim.task_id,
            Some(&claim.run_id),
            task.revision().saturating_add(1),
            now_ms,
            TaskEvent::CancellationRequested {
                run_id: claim.run_id.clone(),
                cause: CancelReason::RuntimeShutdown,
            },
        );
        self.commit_internal(
            task.owner_actor_id(),
            claim,
            "shutdown-request",
            0,
            vec![event],
            now_ms,
        )?;
        Ok(())
    }

    fn record_provider_approval(
        &mut self,
        claim: &LeaseClaim,
        permission: &RuntimePermissionRef,
        request: &CapabilityRequest,
        approval: &ApprovalRequest,
        binding: &RuntimeBindingRef,
        now_ms: u64,
    ) -> Result<TaskView, GatewayDaemonError> {
        self.require_current_lease(claim, now_ms)?;
        let command = LedgerCommand {
            actor_id: request.actor.actor_id.clone(),
            idempotency_key: IdempotencyKey::new(format!(
                "scheduler-approval-{}",
                request.request_id.as_str()
            ))
            .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
            command_digest: digest_json(&(
                "record_provider_approval",
                request,
                approval,
                permission,
                binding,
                claim.generation,
            ))?,
            committed_at_ms: now_ms,
        };
        DurableApprovalCoordinator::new(&mut self.store)
            .record_provider_pending(&command, request, approval, permission, binding, claim)
            .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?;
        let task = self.store.load_task(&claim.task_id)?;
        Ok(TaskView::from(&task))
    }

    fn record_runtime_update(
        &mut self,
        claim: &LeaseClaim,
        sequence: u64,
        update: RuntimeUpdate,
        now_ms: u64,
    ) -> Result<TaskView, GatewayDaemonError> {
        self.require_current_lease(claim, now_ms)?;
        let task = self.store.load_task(&claim.task_id)?;
        let event = self.event(
            task.owner_actor_id(),
            &claim.task_id,
            Some(&claim.run_id),
            task.revision().saturating_add(1),
            now_ms,
            TaskEvent::RuntimeEventRecorded {
                run_id: claim.run_id.clone(),
                update,
            },
        );
        self.commit_internal(
            task.owner_actor_id(),
            claim,
            "update",
            sequence,
            vec![event],
            now_ms,
        )
    }

    fn settle_succeeded(
        &mut self,
        claim: &LeaseClaim,
        now_ms: u64,
    ) -> Result<TaskView, GatewayDaemonError> {
        self.require_current_lease(claim, now_ms)?;
        let task = self.store.load_task(&claim.task_id)?;
        let revision = task
            .revision()
            .checked_add(1)
            .ok_or_else(|| GatewayDaemonError::Protocol("Task revision overflow".to_owned()))?;
        let run = self.event(
            task.owner_actor_id(),
            &claim.task_id,
            Some(&claim.run_id),
            revision,
            now_ms,
            TaskEvent::RunSucceeded {
                run_id: claim.run_id.clone(),
            },
        );
        let completed = self.event(
            task.owner_actor_id(),
            &claim.task_id,
            Some(&claim.run_id),
            revision.saturating_add(1),
            now_ms,
            TaskEvent::TaskSucceeded,
        );
        self.commit_internal(
            task.owner_actor_id(),
            claim,
            "succeed",
            0,
            vec![run, completed],
            now_ms,
        )
    }

    fn settle_failed(
        &mut self,
        claim: &LeaseClaim,
        error: ContractError,
        now_ms: u64,
    ) -> Result<TaskView, GatewayDaemonError> {
        self.require_current_lease(claim, now_ms)?;
        let task = self.store.load_task(&claim.task_id)?;
        let revision = task
            .revision()
            .checked_add(1)
            .ok_or_else(|| GatewayDaemonError::Protocol("Task revision overflow".to_owned()))?;
        let run = self.event(
            task.owner_actor_id(),
            &claim.task_id,
            Some(&claim.run_id),
            revision,
            now_ms,
            TaskEvent::RunFailed {
                run_id: claim.run_id.clone(),
                error: error.clone(),
            },
        );
        let events = if error.retryable {
            vec![run]
        } else {
            let completed_revision = revision
                .checked_add(1)
                .ok_or_else(|| GatewayDaemonError::Protocol("Task revision overflow".to_owned()))?;
            let completed = self.event(
                task.owner_actor_id(),
                &claim.task_id,
                Some(&claim.run_id),
                completed_revision,
                now_ms,
                TaskEvent::TaskFailed { error },
            );
            vec![run, completed]
        };
        self.commit_internal(task.owner_actor_id(), claim, "fail", 0, events, now_ms)
    }

    fn settle_cancelled(
        &mut self,
        claim: &LeaseClaim,
        now_ms: u64,
    ) -> Result<TaskView, GatewayDaemonError> {
        self.require_current_lease(claim, now_ms)?;
        let task = self.store.load_task(&claim.task_id)?;
        let revision = task.revision().saturating_add(1);
        let run = self.event(
            task.owner_actor_id(),
            &claim.task_id,
            Some(&claim.run_id),
            revision,
            now_ms,
            TaskEvent::RunCancelled {
                run_id: claim.run_id.clone(),
                stage: CancellationStage::Runtime,
            },
        );
        let completed = self.event(
            task.owner_actor_id(),
            &claim.task_id,
            Some(&claim.run_id),
            revision.saturating_add(1),
            now_ms,
            TaskEvent::TaskCancelled,
        );
        self.commit_internal(
            task.owner_actor_id(),
            claim,
            "cancel",
            0,
            vec![run, completed],
            now_ms,
        )
    }

    fn commit_internal(
        &mut self,
        actor_id: &ActorId,
        claim: &LeaseClaim,
        operation: &str,
        sequence: u64,
        events: Vec<cosh_gateway_contracts::task::TaskEventEnvelope>,
        now_ms: u64,
    ) -> Result<TaskView, GatewayDaemonError> {
        let revision = events
            .first()
            .map_or(0, |event| event.revision.saturating_sub(1));
        self.store.commit_task(&TaskCommit {
            actor_id: actor_id.clone(),
            idempotency_key: IdempotencyKey::new(format!(
                "scheduler-{operation}-{}-{}-{sequence}",
                claim.run_id.as_str(),
                claim.generation
            ))
            .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
            command_digest: digest_json(&(
                operation,
                &claim.task_id,
                &claim.run_id,
                &claim.lease_owner,
                claim.generation,
                claim.revision,
                sequence,
                &events,
            ))?,
            expected_revision: Some(revision),
            events,
            outbox: Vec::new(),
            committed_at_ms: now_ms,
        })?;
        Ok(TaskView::from(&self.store.load_task(&claim.task_id)?))
    }

    fn require_current_lease(
        &self,
        claim: &LeaseClaim,
        now_ms: u64,
    ) -> Result<(), GatewayDaemonError> {
        let lease = self.store.load_run_lease(&claim.run_id)?;
        if lease.task_id != claim.task_id
            || lease.lease_owner != claim.lease_owner
            || lease.generation != claim.generation
            || lease.revision != claim.revision
            || lease.expires_at_ms <= now_ms
        {
            return Err(StoreError::GenerationFenced {
                expected: claim.generation,
                actual: lease.generation,
            }
            .into());
        }
        Ok(())
    }

    fn release_lease(&mut self, claim: &LeaseClaim, now_ms: u64) -> Result<(), GatewayDaemonError> {
        let task = self.store.load_task(&claim.task_id)?;
        let command = LedgerCommand {
            actor_id: task.owner_actor_id().clone(),
            idempotency_key: IdempotencyKey::new(format!(
                "scheduler-release-{}-{}",
                claim.run_id.as_str(),
                claim.generation
            ))
            .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
            command_digest: digest_json(&(
                "release_run_lease",
                &claim.task_id,
                &claim.run_id,
                &claim.lease_owner,
                claim.generation,
                claim.revision,
            ))?,
            committed_at_ms: now_ms,
        };
        self.store.release_run_lease(&command, claim)?;
        Ok(())
    }
}
