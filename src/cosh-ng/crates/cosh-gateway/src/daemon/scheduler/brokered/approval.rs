impl<F: RuntimeFactory> TaskScheduler<F> {
    pub(super) fn resolve_brokered_approval(
        &mut self,
        actor_id: &ActorId,
        idempotency_key: IdempotencyKey,
        approval: ApprovalRecord,
        decision: ApprovalDecision,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let resumable_approved = approval.state == ApprovalState::Approved
            && decision == ApprovalDecision::Approve
            && self.active.as_ref().is_some_and(|active| {
                active.scheduled.actor.actor_id == *actor_id
                    && active.pending_brokered.as_ref().is_some_and(|pending| {
                        pending.approval.approval_id == approval.approval_id
                            && pending.resolution.is_none()
                    })
            })
            && matches!(
                self.coordinator
                    .store
                    .load_brokered_runtime_dispatch_record(
                        &approval.request_id,
                        BrokeredRuntimeDispatchKind::Result,
                    ),
                Err(StoreError::LedgerNotFound { .. })
            );
        if approval.state != ApprovalState::Pending && !resumable_approved {
            return self.replay_resolved_brokered_approval(actor_id, &approval, decision, now_ms);
        }
        if self.active.is_none() {
            return Err(GatewayDaemonError::Protocol(
                "brokered approval requires its live Runtime handle".to_owned(),
            ));
        }
        self.ensure_active_operation_budget(now_ms)?;
        let (lease, pending, task_id, run_id) = {
            let active = self.active.as_ref().ok_or_else(no_active_run)?;
            if &active.scheduled.actor.actor_id != actor_id {
                return Err(GatewayDaemonError::Unauthorized);
            }
            (
                active.lease.clone(),
                active.pending_brokered.clone(),
                active.scheduled.task_id.clone(),
                active.scheduled.run_id.clone(),
            )
        };
        let pending = pending.ok_or_else(|| {
            GatewayDaemonError::Protocol(
                "brokered approval does not match a live Runtime callback".to_owned(),
            )
        })?;
        if pending.approval.approval_id != approval.approval_id
            || pending.approval.request_id != approval.request_id
            || approval.task_id != task_id
            || approval.run_id != run_id
        {
            return Err(GatewayDaemonError::Unauthorized);
        }
        if approval.state == ApprovalState::Pending && now_ms >= approval.expires_at_ms {
            self.expire_active_brokered_approval(&approval, now_ms)?;
            return Err(GatewayDaemonError::Protocol(
                "brokered approval is no longer resolvable".to_owned(),
            ));
        }
        let request = self
            .coordinator
            .store
            .load_brokered_request(&approval.request_id)?;
        if request.approval_id.as_ref() != Some(&approval.approval_id)
            || Some(&request.runtime_fence) != approval.runtime_fence.as_ref()
            || Some(&request.target_identity_digest) != approval.target_identity_digest.as_ref()
            || request.request.actor.actor_id != *actor_id
            || request.request.task_id != task_id
            || request.request.run_id != run_id
            || request.request.request_id != pending.brokered.request_id
            || request.operation != pending.brokered.operation
        {
            return Err(GatewayDaemonError::Unauthorized);
        }
        let resolution = match self.brokered_driver.resolve(
            &mut self.coordinator.store,
            BrokeredResolutionContext {
                approval: &approval,
                request: &request,
                brokered: &pending.brokered,
                lease: &lease,
                idempotency_key: &idempotency_key,
                decision,
                now_ms,
            },
        ) {
            Ok(resolution) => resolution,
            Err(error) => return self.shutdown_after_brokered_failure(error, now_ms),
        };
        validate_resolution(&approval, decision, &resolution)?;
        self.active
            .as_mut()
            .ok_or_else(no_active_run)?
            .pending_brokered
            .as_mut()
            .ok_or_else(no_active_run)?
            .resolution = Some(resolution.clone());
        let payload_digest = digest_json(&resolution.delivery)?;
        let prepare_command = brokered_dispatch_command(
            actor_id,
            "prepare",
            BrokeredRuntimeDispatchKind::Result,
            &pending.brokered,
            0,
            now_ms,
        )?;
        let prepared_outcome = match &resolution.source {
            BrokeredResolutionSource::ApprovalDenied { approval_id } => self
                .coordinator
                .store
                .prepare_brokered_denied_result_dispatch(
                    &prepare_command,
                    approval_id,
                    &pending.brokered,
                    &resolution.delivery,
                    &lease,
                )?,
            BrokeredResolutionSource::Execution { execution_id } => self
                .coordinator
                .store
                .prepare_brokered_execution_result_dispatch(
                    &prepare_command,
                    execution_id,
                    &pending.brokered,
                    &resolution.delivery,
                    &lease,
                )?,
        };
        let prepared = match prepared_outcome {
            LedgerOutcome::Applied(record) => record,
            LedgerOutcome::Replayed(record) => {
                return self.reject_replayed_brokered_dispatch(record, now_ms)
            }
        };
        let uncertain = matches!(
            &resolution.delivery.outcome,
            cosh_gateway_contracts::runtime::BrokeredExecutionOutcome::Uncertain { .. }
        );
        if let Some(tick) = self.dispatch_brokered_result(
            actor_id,
            &lease,
            &pending.brokered,
            resolution.delivery,
            &payload_digest,
            prepared,
            now_ms,
        )? {
            return Ok(tick);
        }
        self.active
            .as_mut()
            .ok_or_else(no_active_run)?
            .pending_brokered = None;
        if uncertain {
            return self.finish_suspended_after_brokered_result(now_ms);
        }
        let task = self.coordinator.store.load_task(&task_id)?;
        Ok(SchedulerTick::Progressed(TaskView::from(&task)))
    }

}
