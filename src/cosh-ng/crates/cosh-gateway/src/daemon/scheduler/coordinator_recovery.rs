impl TaskCoordinator {
    fn recover_expired_active_run(
        &mut self,
        brokered_driver: &mut dyn BrokeredExecutionDriver,
        worker_id: &BoundedOpaque,
        now_ms: u64,
        lease_expires_at_ms: u64,
    ) -> Result<Option<TaskView>, GatewayDaemonError> {
        let Some(expired) = self.store.load_expired_active_lease(now_ms)? else {
            return Ok(None);
        };
        let command = LeaseCommand {
            command: LedgerCommand {
                actor_id: expired.actor_id.clone(),
                idempotency_key: IdempotencyKey::new(format!(
                    "scheduler-recover-lease-{}-{}",
                    expired.run_id.as_str(),
                    expired.generation
                ))
                .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
                command_digest: digest_json(&(
                    "recover_run_lease",
                    &expired.task_id,
                    &expired.run_id,
                    worker_id,
                    expired.generation,
                    lease_expires_at_ms,
                ))?,
                committed_at_ms: now_ms,
            },
            task_id: expired.task_id,
            run_id: expired.run_id,
            lease_owner: worker_id.clone(),
            expires_at_ms: lease_expires_at_ms,
        };
        let record = match self.store.acquire_run_lease(&command)? {
            LedgerOutcome::Applied(record) | LedgerOutcome::Replayed(record) => record,
        };
        let actor_id = record.actor_id.clone();
        let claim = LeaseClaim {
            task_id: record.task_id,
            run_id: record.run_id,
            lease_owner: record.lease_owner,
            generation: record.generation,
            revision: record.revision,
        };
        self.store
            .mark_runtime_bindings_lost_for_run(&claim.run_id, now_ms)?;
        let input_recovery = LedgerCommand {
            actor_id,
            idempotency_key: IdempotencyKey::new(format!(
                "scheduler-recover-input-{}-{}",
                claim.run_id.as_str(),
                claim.generation
            ))
            .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
            command_digest: digest_json(&(
                "recover_runtime_input_dispatch_for_run",
                &claim.task_id,
                &claim.run_id,
                claim.generation,
            ))?,
            committed_at_ms: now_ms,
        };
        self.store.recover_runtime_input_dispatch_for_run(
            &input_recovery,
            &claim.run_id,
            &claim,
        )?;
        self.store
            .cancel_pending_approvals_for_run(&claim.run_id, now_ms)?;
        self.store
            .mark_provider_dispatches_unknown_for_run(&claim.run_id, now_ms)?;
        self.store
            .mark_brokered_dispatches_unknown_for_run(&claim.run_id, now_ms)?;
        self.reconcile_started_brokered_executions(brokered_driver, &claim, now_ms)?;
        self.store
            .recover_brokered_executions_for_run(&claim.run_id, now_ms)?;
        let recovered_task = self.store.load_task(&claim.task_id)?;
        if recovered_task.cancellation_requested()
            && recovered_task.active_run_can_be_cancelled(&claim.run_id)
        {
            let view = self.settle_cancelled(&claim, now_ms)?;
            self.release_lease(&claim, now_ms)?;
            return Ok(Some(view));
        }
        if recovered_task.state() == TaskState::Suspended {
            let view = TaskView::from(&recovered_task);
            self.release_lease(&claim, now_ms)?;
            return Ok(Some(view));
        }
        let view = self.settle_failed(
            &claim,
            runtime_lost_error(
                "runtime_lost",
                "Runtime ownership was lost across daemon restart",
            )?,
            now_ms,
        )?;
        self.release_lease(&claim, now_ms)?;
        Ok(Some(view))
    }

    fn reconcile_started_brokered_executions(
        &mut self,
        brokered_driver: &mut dyn BrokeredExecutionDriver,
        claim: &LeaseClaim,
        now_ms: u64,
    ) -> Result<(), GatewayDaemonError> {
        use crate::capability::ExecutionTargetOutcome;

        let candidates = self
            .store
            .load_started_brokered_recovery_candidates(claim, now_ms)?;
        for candidate in candidates {
            let outcome = brokered_driver.reconcile_started(BrokeredRecoveryContext {
                execution: &candidate.execution,
                request: &candidate.request,
            });
            let ExecutionTargetOutcome::Conclusive {
                succeeded,
                receipt_digest,
                safe_detail,
                typed_result,
            } = outcome
            else {
                continue;
            };
            let command = LedgerCommand {
                actor_id: candidate.execution.actor_id.clone(),
                idempotency_key: IdempotencyKey::new(format!(
                    "scheduler-reconcile-execution-{}-{}",
                    candidate.execution.execution_id.as_str(),
                    claim.generation
                ))
                .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
                command_digest: digest_json(&(
                    "reconcile_started_execution",
                    &candidate.execution.execution_id,
                    claim.generation,
                    &receipt_digest,
                    succeeded,
                    &typed_result,
                ))?,
                committed_at_ms: now_ms,
            };
            self.store.complete_execution(
                &command,
                &ExecutionCompletion {
                    execution_id: candidate.execution.execution_id,
                    expected_revision: candidate.execution.revision,
                    succeeded,
                    receipt_digest,
                    safe_detail,
                    typed_result,
                },
            )?;
        }
        Ok(())
    }

    fn claim_runtime_start(
        &mut self,
        worker_id: &BoundedOpaque,
        now_ms: u64,
        lease_expires_at_ms: u64,
    ) -> Result<RuntimeStartClaim, GatewayDaemonError> {
        let delivery_kind = runtime_start_delivery_kind();
        let Some(candidate) = self.store.peek_ready_outbox(&delivery_kind, now_ms)? else {
            return Ok(RuntimeStartClaim::Empty);
        };
        let intent = decode_runtime_start_intent(
            candidate.payload.clone(),
            &self.launch_catalog,
        )?;
        if intent.task_id != candidate.task_id {
            return Err(GatewayDaemonError::Protocol(
                "runtime start Outbox identity mismatch".to_owned(),
            ));
        }
        let Some(claim) = self.store.claim_outbox_candidate(
            &delivery_kind,
            &candidate,
            worker_id,
            now_ms,
            lease_expires_at_ms,
        )?
        else {
            return Ok(RuntimeStartClaim::Empty);
        };
        let task = self.store.load_task(&intent.task_id)?;
        if task.owner_actor_id() != &intent.actor.actor_id
            || task.active_run_id() != Some(&intent.run_id)
            || task.target() != &intent.target
        {
            return Err(GatewayDaemonError::Protocol(
                "runtime start intent no longer matches its queued Task".to_owned(),
            ));
        }
        if !matches!(task.state(), TaskState::Queued | TaskState::Running) {
            self.store.complete_outbox(&claim, now_ms)?;
            return Ok(RuntimeStartClaim::Empty);
        }
        let lease =
            self.acquire_start_lease(&intent, &claim, worker_id, now_ms, lease_expires_at_ms)?;
        if task.state() == TaskState::Running {
            // The preceding worker crossed RunStarted but did not acknowledge
            // its Outbox delivery. The expired Run lease proves that no live
            // scheduler still owns the handle, so fail closed after takeover.
            self.store
                .mark_runtime_bindings_lost_for_run(&lease.run_id, now_ms)?;
            self.store
                .cancel_pending_approvals_for_run(&lease.run_id, now_ms)?;
            self.store
                .mark_provider_dispatches_unknown_for_run(&lease.run_id, now_ms)?;
            let view = self.settle_failed(
                &lease,
                runtime_lost_error(
                    "runtime_lost",
                    "Runtime ownership was lost before start acknowledgement",
                )?,
                now_ms,
            )?;
            self.store.complete_outbox(&claim, now_ms)?;
            self.release_lease(&lease, now_ms)?;
            return Ok(RuntimeStartClaim::Recovered(view));
        }
        let started = self.event(
            &intent.actor.actor_id,
            &intent.task_id,
            Some(&intent.run_id),
            task.revision().saturating_add(1),
            now_ms,
            TaskEvent::RunStarted {
                run_id: intent.run_id.clone(),
            },
        );
        self.store.commit_task(&TaskCommit {
            actor_id: intent.actor.actor_id.clone(),
            idempotency_key: internal_key("start", &claim),
            command_digest: digest_json(&("start_runtime", &intent.task_id, &intent.run_id))?,
            expected_revision: Some(task.revision()),
            events: vec![started],
            outbox: Vec::new(),
            committed_at_ms: now_ms,
        })?;
        Ok(RuntimeStartClaim::Claimed {
            outbox: claim,
            intent: Box::new(intent),
            lease,
        })
    }

    fn acquire_start_lease(
        &mut self,
        intent: &RuntimeStartIntent,
        claim: &OutboxClaim,
        worker_id: &BoundedOpaque,
        now_ms: u64,
        lease_expires_at_ms: u64,
    ) -> Result<LeaseClaim, GatewayDaemonError> {
        let lease_command = LeaseCommand {
            command: internal_ledger_command(
                &intent.actor.actor_id,
                "lease",
                claim,
                now_ms,
                &(
                    &intent.task_id,
                    &intent.run_id,
                    worker_id,
                    lease_expires_at_ms,
                ),
            )?,
            task_id: intent.task_id.clone(),
            run_id: intent.run_id.clone(),
            lease_owner: worker_id.clone(),
            expires_at_ms: lease_expires_at_ms,
        };
        let lease = match self.store.acquire_run_lease(&lease_command)? {
            LedgerOutcome::Applied(lease) | LedgerOutcome::Replayed(lease) => lease,
        };
        Ok(LeaseClaim {
            task_id: lease.task_id,
            run_id: lease.run_id,
            lease_owner: lease.lease_owner,
            generation: lease.generation,
            revision: lease.revision,
        })
    }

    fn renew_lease(
        &mut self,
        actor_id: &ActorId,
        claim: &LeaseClaim,
        expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<LeaseClaim, GatewayDaemonError> {
        let command = LeaseCommand {
            command: LedgerCommand {
                actor_id: actor_id.clone(),
                idempotency_key: IdempotencyKey::new(format!(
                    "scheduler-renew-{}-{}",
                    claim.run_id.as_str(),
                    claim.revision
                ))
                .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
                command_digest: digest_json(&(
                    "renew_run_lease",
                    &claim.task_id,
                    &claim.run_id,
                    &claim.lease_owner,
                    claim.generation,
                    claim.revision,
                    expires_at_ms,
                    now_ms,
                ))?,
                committed_at_ms: now_ms,
            },
            task_id: claim.task_id.clone(),
            run_id: claim.run_id.clone(),
            lease_owner: claim.lease_owner.clone(),
            expires_at_ms,
        };
        let record = match self
            .store
            .renew_run_lease(&command, claim.generation, claim.revision)?
        {
            LedgerOutcome::Applied(record) | LedgerOutcome::Replayed(record) => record,
        };
        Ok(LeaseClaim {
            task_id: record.task_id,
            run_id: record.run_id,
            lease_owner: record.lease_owner,
            generation: record.generation,
            revision: record.revision,
        })
    }

    fn persist_runtime_binding(
        &mut self,
        claim: &LeaseClaim,
        binding: &RuntimeBindingRef,
        now_ms: u64,
    ) -> Result<TaskView, GatewayDaemonError> {
        self.require_current_lease(claim, now_ms)?;
        let task = self.store.load_task(&claim.task_id)?;
        let command = LedgerCommand {
            actor_id: task.owner_actor_id().clone(),
            idempotency_key: IdempotencyKey::new(format!(
                "scheduler-bind-ledger-{}-{}",
                claim.run_id.as_str(),
                claim.generation
            ))
            .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
            command_digest: digest_json(&(
                "bind_runtime",
                &claim.task_id,
                &claim.run_id,
                &claim.lease_owner,
                claim.generation,
                binding,
            ))?,
            committed_at_ms: now_ms,
        };
        self.store.bind_runtime(&command, binding, claim)?;
        let event = self.event(
            task.owner_actor_id(),
            &claim.task_id,
            Some(&claim.run_id),
            task.revision().saturating_add(1),
            now_ms,
            TaskEvent::RuntimeBound {
                run_id: claim.run_id.clone(),
                binding: binding.clone(),
            },
        );
        self.commit_internal(task.owner_actor_id(), claim, "bind", 0, vec![event], now_ms)
    }

    fn close_runtime_binding(
        &mut self,
        actor_id: &ActorId,
        claim: &LeaseClaim,
        binding: &RuntimeBindingRef,
        now_ms: u64,
    ) -> Result<(), GatewayDaemonError> {
        let command = LedgerCommand {
            actor_id: actor_id.clone(),
            idempotency_key: IdempotencyKey::new(format!(
                "scheduler-close-binding-{}-{}",
                claim.run_id.as_str(),
                claim.generation
            ))
            .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
            command_digest: digest_json(&(
                "close_runtime_binding",
                &binding.binding_id,
                binding.runtime_generation,
                claim.generation,
            ))?,
            committed_at_ms: now_ms,
        };
        self.store.close_runtime_binding(
            &command,
            &binding.binding_id,
            binding.runtime_generation,
            claim,
        )?;
        Ok(())
    }

    fn record_runtime_binding_sequence(
        &mut self,
        claim: &LeaseClaim,
        binding: &RuntimeBindingRef,
        sequence: u64,
        now_ms: u64,
    ) -> Result<(), GatewayDaemonError> {
        self.store.record_runtime_sequence(
            &binding.binding_id,
            &binding.runtime_instance_id,
            binding.runtime_generation,
            sequence,
            now_ms,
            claim,
        )?;
        Ok(())
    }

}
