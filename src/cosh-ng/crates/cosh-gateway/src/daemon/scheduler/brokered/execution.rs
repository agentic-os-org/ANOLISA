impl<F: RuntimeFactory> TaskScheduler<F> {
    pub(super) fn admit_brokered_execution(
        &mut self,
        brokered: BrokeredExecutionRef,
        request: CapabilityRequest,
        operation: BrokeredOperation,
        summary: ToolSummary,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let (scheduled, binding, lease) = {
            let active = self.active.as_ref().ok_or_else(no_active_run)?;
            if active.pending_permission.is_some() || active.pending_brokered.is_some() {
                return self.finish_failed(
                    runtime_lost_error(
                        "runtime_brokered_callback_order_invalid",
                        "Runtime emitted another callback while one was pending",
                    )?,
                    now_ms,
                );
            }
            (
                active.scheduled.clone(),
                active.binding.clone(),
                active.lease.clone(),
            )
        };
        if brokered.binding_id != binding.binding_id
            || brokered.runtime_generation != binding.runtime_generation
            || brokered.run_id != scheduled.run_id
            || brokered.request_id != request.request_id
            || brokered.operation != operation
            || request.actor.actor_id != scheduled.actor.actor_id
            || request.task_id != scheduled.task_id
            || request.run_id != scheduled.run_id
            || request.target != scheduled.target
            || request.expires_at_ms <= now_ms
        {
            return self.finish_failed(
                runtime_lost_error(
                    "runtime_brokered_binding_invalid",
                    "Brokered Runtime request did not match the active Run",
                )?,
                now_ms,
            );
        }
        self.coordinator.record_runtime_binding_sequence(
            &lease,
            &binding,
            brokered.event_sequence,
            now_ms,
        )?;
        let runtime_fence = RuntimeExecutionFence {
            binding_id: binding.binding_id.clone(),
            runtime_generation: binding.runtime_generation,
            lease_generation: lease.generation,
            lease_revision: lease.revision,
        };
        let plan = match self.brokered_driver.plan_approval(BrokeredApprovalContext {
            scheduled: &scheduled,
            brokered: &brokered,
            request: &request,
            operation: &operation,
            summary: &summary,
            runtime_fence: &runtime_fence,
            now_ms,
        }) {
            Ok(plan) => plan,
            Err(error) => return self.shutdown_after_brokered_failure(error, now_ms),
        };
        if plan.approval.expires_at_ms > request.expires_at_ms
            || plan.approval.expires_at_ms <= now_ms
        {
            return self.finish_failed(
                runtime_lost_error(
                    "brokered_approval_plan_invalid",
                    "Brokered approval plan did not preserve the admitted request",
                )?,
                now_ms,
            );
        }
        let command = brokered_dispatch_command(
            &scheduled.actor.actor_id,
            "admit",
            BrokeredRuntimeDispatchKind::Acknowledgement,
            &brokered,
            0,
            now_ms,
        )?;
        let approval_record = DurableApprovalCoordinator::new(&mut self.coordinator.store)
            .record_pending(
                &command,
                &request,
                &plan.approval,
                crate::capability::BrokeredApprovalBinding {
                    operation: &operation,
                    target_identity_digest: &plan.target_identity_digest,
                    runtime_fence: &runtime_fence,
                    provider_binding: plan.provider_binding.as_ref(),
                },
            )
            .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?;
        if approval_record.state != ApprovalState::Pending {
            return self.finish_failed(
                runtime_lost_error(
                    "brokered_approval_not_pending",
                    "Brokered approval was not durably pending before acknowledgement",
                )?,
                now_ms,
            );
        }
        let acknowledgement = BrokeredRequestAcknowledgement {
            request_id: request.request_id,
            approval_id: plan.approval.approval_id.clone(),
        };
        let payload_digest = digest_json(&acknowledgement)?;
        let prepare_command = brokered_dispatch_command(
            &scheduled.actor.actor_id,
            "prepare",
            BrokeredRuntimeDispatchKind::Acknowledgement,
            &brokered,
            0,
            now_ms,
        )?;
        let prepared = match self
            .coordinator
            .store
            .prepare_brokered_acknowledgement_dispatch(
                &prepare_command,
                &plan.approval.approval_id,
                &brokered,
                &payload_digest,
                &lease,
            )? {
            LedgerOutcome::Applied(record) => record,
            LedgerOutcome::Replayed(record) => {
                return self.reject_replayed_brokered_dispatch(record, now_ms)
            }
        };
        if let Some(tick) = self.dispatch_brokered_acknowledgement(
            &scheduled.actor.actor_id,
            &lease,
            &brokered,
            acknowledgement,
            &payload_digest,
            prepared,
            now_ms,
        )? {
            return Ok(tick);
        }
        let task = self.coordinator.store.load_task(&scheduled.task_id)?;
        self.active
            .as_mut()
            .ok_or_else(no_active_run)?
            .pending_brokered = Some(PendingBrokered {
            brokered,
            approval: plan.approval.clone(),
            resolution: None,
        });
        if scheduled.launch.approval == ApprovalPolicy::AllowAll {
            let key = IdempotencyKey::new(format!(
                "task-allow-all-brokered-{}",
                plan.approval.approval_id.as_str()
            ))
            .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?;
            self.resolve_approval(
                &scheduled.actor.actor_id,
                key,
                &plan.approval.approval_id,
                ApprovalDecision::Approve,
                now_ms,
            )
        } else {
            Ok(SchedulerTick::Progressed(TaskView::from(&task)))
        }
    }

    fn replay_resolved_brokered_approval(
        &mut self,
        actor_id: &ActorId,
        approval: &ApprovalRecord,
        decision: ApprovalDecision,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        if !matches!(
            (approval.state, decision),
            (ApprovalState::Denied, ApprovalDecision::Deny)
                | (ApprovalState::Approved, ApprovalDecision::Approve)
        ) {
            return Err(GatewayDaemonError::Protocol(
                "brokered approval was already resolved with another decision".to_owned(),
            ));
        }
        let request = self
            .coordinator
            .store
            .load_brokered_request(&approval.request_id)?;
        if request.approval_id.as_ref() != Some(&approval.approval_id)
            || request.request.actor.actor_id != *actor_id
            || request.request.task_id != approval.task_id
            || request.request.run_id != approval.run_id
            || Some(&request.runtime_fence) != approval.runtime_fence.as_ref()
            || Some(&request.target_identity_digest) != approval.target_identity_digest.as_ref()
        {
            return Err(GatewayDaemonError::Unauthorized);
        }
        let dispatch = self
            .coordinator
            .store
            .load_brokered_runtime_dispatch_record(
                &approval.request_id,
                BrokeredRuntimeDispatchKind::Result,
            )
            .map_err(|error| match error {
                StoreError::LedgerNotFound { .. } => GatewayDaemonError::Protocol(
                    "brokered result was resolved without a durable dispatch".to_owned(),
                ),
                other => GatewayDaemonError::Store(other),
            })?;
        validate_replayed_dispatch(
            actor_id,
            approval,
            &request,
            decision,
            &dispatch,
            &self.coordinator.store,
        )?;
        match dispatch.state {
            BrokeredRuntimeDispatchState::Delivered => {
                let task = self.coordinator.store.load_task(&approval.task_id)?;
                Ok(SchedulerTick::Progressed(TaskView::from(&task)))
            }
            BrokeredRuntimeDispatchState::Started => {
                self.coordinator
                    .store
                    .mark_brokered_dispatches_unknown_for_run(&approval.run_id, now_ms)?;
                Err(indeterminate_replay_error())
            }
            BrokeredRuntimeDispatchState::Unknown => Err(indeterminate_replay_error()),
            BrokeredRuntimeDispatchState::Prepared => {
                self.resume_prepared_brokered_result(actor_id, approval, decision, dispatch, now_ms)
            }
        }
    }

    fn resume_prepared_brokered_result(
        &mut self,
        actor_id: &ActorId,
        approval: &ApprovalRecord,
        decision: ApprovalDecision,
        dispatch: BrokeredRuntimeDispatchRecord,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let (lease, pending, resolution) = {
            let active = self.active.as_ref().ok_or_else(|| {
                GatewayDaemonError::Protocol(
                    "prepared brokered result lost its live Runtime payload".to_owned(),
                )
            })?;
            let pending = active.pending_brokered.as_ref().ok_or_else(|| {
                GatewayDaemonError::Protocol(
                    "prepared brokered result lost its live Runtime callback".to_owned(),
                )
            })?;
            if active.scheduled.actor.actor_id != *actor_id
                || active.scheduled.task_id != approval.task_id
                || active.scheduled.run_id != approval.run_id
                || pending.approval.approval_id != approval.approval_id
                || pending.brokered != dispatch.brokered
            {
                return Err(GatewayDaemonError::Unauthorized);
            }
            let resolution = pending.resolution.clone().ok_or_else(|| {
                GatewayDaemonError::Protocol(
                    "prepared brokered result lost its exact payload".to_owned(),
                )
            })?;
            (active.lease.clone(), pending.clone(), resolution)
        };
        validate_resolution(approval, decision, &resolution)?;
        let payload_digest = digest_json(&resolution.delivery)?;
        if payload_digest != dispatch.payload_digest {
            return Err(GatewayDaemonError::Unauthorized);
        }
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
            dispatch,
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
        let task = self.coordinator.store.load_task(&approval.task_id)?;
        Ok(SchedulerTick::Progressed(TaskView::from(&task)))
    }

}
