impl<F: RuntimeFactory> TaskScheduler<F> {
    /// Resolves an approval only when its Task binding matches the caller.
    pub fn resolve_approval_for_task(
        &mut self,
        actor_id: &ActorId,
        idempotency_key: IdempotencyKey,
        task_id: &TaskId,
        approval_id: &ApprovalId,
        decision: ApprovalDecision,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let approval = self.coordinator.store.load_approval_record(approval_id)?;
        if approval.actor_id != *actor_id || approval.task_id != *task_id {
            return Err(GatewayDaemonError::Unauthorized);
        }
        self.resolve_approval(actor_id, idempotency_key, approval_id, decision, now_ms)
    }

    /// Resolves the only Runtime Permission approval currently held by this worker.
    pub fn resolve_approval(
        &mut self,
        actor_id: &ActorId,
        idempotency_key: IdempotencyKey,
        approval_id: &ApprovalId,
        decision: ApprovalDecision,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let approval = self.coordinator.store.load_approval_record(approval_id)?;
        if approval.actor_id != *actor_id {
            return Err(GatewayDaemonError::Unauthorized);
        }
        if approval.permission.is_none()
            && approval.target_identity_digest.is_some()
            && approval.runtime_fence.is_some()
        {
            return self.resolve_brokered_approval(
                actor_id,
                idempotency_key,
                approval,
                decision,
                now_ms,
            );
        }
        let expected_revision = approval_resolution_revision(&approval, decision)?;
        let permission = approval.permission.clone().ok_or_else(|| {
            GatewayDaemonError::Protocol(
                "approval is not bound to a Runtime Permission callback".to_owned(),
            )
        })?;
        let allow_decision = match (
            permission.callback.is_some(),
            permission.core_callback.is_some(),
        ) {
            (true, false) => RuntimePermissionDecision::ProviderNativeAllowOnce,
            (false, true) => RuntimePermissionDecision::RuntimeNativeAllowOnce,
            _ => {
                return Err(GatewayDaemonError::Protocol(
                    "live Runtime Permission must have exactly one callback binding".to_owned(),
                ))
            }
        };
        let expects_provider_denial = permission.callback.is_some();
        let resolution_command = LedgerCommand {
            actor_id: actor_id.clone(),
            idempotency_key,
            command_digest: digest_json(&(
                "resolve_provider_permission",
                approval_id,
                decision,
                expected_revision,
                &permission,
            ))?,
            committed_at_ms: now_ms,
        };
        if approval.state != ApprovalState::Pending {
            let replay_lease = LeaseClaim {
                task_id: approval.task_id.clone(),
                run_id: approval.run_id.clone(),
                lease_owner: BoundedOpaque::new("durable-replay")
                    .unwrap_or_else(|_| unreachable!()),
                generation: 0,
                revision: 0,
            };
            let replayed = self.coordinator.store.resolve_provider_permission(
                &resolution_command,
                approval_id,
                expected_revision,
                crate::storage::ApprovalResolution::Decide(decision),
                &permission,
                &replay_lease,
            )?;
            if !matches!(replayed, LedgerOutcome::Replayed(_)) {
                return Err(GatewayDaemonError::Protocol(
                    "approval replay unexpectedly changed durable state".to_owned(),
                ));
            }
            let dispatch = self
                .coordinator
                .store
                .load_provider_permission_dispatch_record(approval_id)?;
            if dispatch.actor_id != *actor_id
                || dispatch.task_id != approval.task_id
                || dispatch.run_id != approval.run_id
                || dispatch.permission != permission
                || dispatch.decision != provider_dispatch_decision(decision)
            {
                return Err(GatewayDaemonError::Unauthorized);
            }
            if dispatch.state == ProviderPermissionDispatchState::Written {
                let task = self.coordinator.store.load_task(&approval.task_id)?;
                return Ok(SchedulerTick::Progressed(TaskView::from(&task)));
            }
            if matches!(
                dispatch.state,
                ProviderPermissionDispatchState::WriteStarted
                    | ProviderPermissionDispatchState::Abandoned
                    | ProviderPermissionDispatchState::Unknown
            ) {
                self.coordinator
                    .store
                    .mark_provider_dispatches_unknown_for_run(&approval.run_id, now_ms)?;
                let active_matches = self.active.as_ref().is_some_and(|active| {
                    active.scheduled.task_id == approval.task_id
                        && active.scheduled.run_id == approval.run_id
                });
                if active_matches {
                    return self.fail_unknown_provider_dispatch(
                        runtime_lost_error(
                            "provider_permission_replay_unknown",
                            "Provider permission response delivery is indeterminate",
                        )?,
                        now_ms,
                    );
                }
                return Err(GatewayDaemonError::Protocol(
                    "provider permission response delivery is indeterminate".to_owned(),
                ));
            }
        }
        if self.active.is_none() {
            return Err(GatewayDaemonError::Protocol(
                "provider permission replay requires a live Runtime handle".to_owned(),
            ));
        }
        self.ensure_active_operation_budget(now_ms)?;
        let (lease, task_id, run_id, pending) = {
            let active = self.active.as_ref().ok_or_else(no_active_run)?;
            if &active.scheduled.actor.actor_id != actor_id {
                return Err(GatewayDaemonError::Unauthorized);
            }
            if let Some(pending) = &active.pending_permission {
                if &pending.approval.approval_id != approval_id {
                    return Err(GatewayDaemonError::Protocol(
                        "approval does not match the active Runtime callback".to_owned(),
                    ));
                }
            }
            (
                active.lease.clone(),
                active.scheduled.task_id.clone(),
                active.scheduled.run_id.clone(),
                active.pending_permission.clone(),
            )
        };
        if approval.task_id != task_id || approval.run_id != run_id {
            return Err(GatewayDaemonError::Unauthorized);
        }
        if let Some(pending) = &pending {
            if pending.permission != permission {
                return Err(GatewayDaemonError::Protocol(
                    "approval does not match the active Runtime callback".to_owned(),
                ));
            }
        }
        if now_ms >= approval.expires_at_ms {
            self.expire_active_provider_approval(approval_id, &permission, now_ms)?;
            return Err(GatewayDaemonError::Protocol(
                "approval is no longer resolvable".to_owned(),
            ));
        }
        if decision == ApprovalDecision::Approve {
            self.ensure_approval_checkpoint_barrier(&approval, now_ms)?;
        }
        let prepared = self.coordinator.store.resolve_provider_permission(
            &resolution_command,
            approval_id,
            expected_revision,
            crate::storage::ApprovalResolution::Decide(decision),
            &permission,
            &lease,
        )?;
        let prepared = match prepared {
            LedgerOutcome::Applied(prepared) => prepared,
            LedgerOutcome::Replayed(_) => self
                .coordinator
                .store
                .load_provider_permission_dispatch_record(approval_id)?,
        };
        if prepared.actor_id != *actor_id
            || prepared.task_id != task_id
            || prepared.run_id != run_id
            || prepared.permission != permission
            || prepared.decision != provider_dispatch_decision(decision)
        {
            return Err(GatewayDaemonError::Unauthorized);
        }
        match prepared.state {
            ProviderPermissionDispatchState::Written => {
                let task = self.coordinator.store.load_task(&task_id)?;
                return Ok(SchedulerTick::Progressed(TaskView::from(&task)));
            }
            ProviderPermissionDispatchState::WriteStarted
            | ProviderPermissionDispatchState::Abandoned
            | ProviderPermissionDispatchState::Unknown => {
                return self.fail_unknown_provider_dispatch(
                    runtime_lost_error(
                        "provider_permission_replay_unknown",
                        "Provider permission response delivery is indeterminate",
                    )?,
                    now_ms,
                );
            }
            ProviderPermissionDispatchState::Prepared => {}
        }
        let task = self.coordinator.store.load_task(&task_id)?;
        let view = if task.state() == TaskState::Running {
            TaskView::from(&task)
        } else {
            return self.fail_unknown_provider_dispatch(
                runtime_lost_error(
                    "provider_permission_task_state_invalid",
                    "Provider permission response no longer matches an active Task",
                )?,
                now_ms,
            );
        };
        let start_command =
            provider_dispatch_command(actor_id, "start", approval_id, prepared.revision, now_ms)?;
        let started = match self.coordinator.store.start_provider_permission_dispatch(
            &start_command,
            approval_id,
            prepared.revision,
            &lease,
        )? {
            LedgerOutcome::Applied(started) => started,
            LedgerOutcome::Replayed(_) => {
                return self.fail_unknown_provider_dispatch(
                    runtime_lost_error(
                        "provider_permission_replay_unknown",
                        "Provider permission dispatch start was replayed",
                    )?,
                    now_ms,
                );
            }
        };
        let runtime_decision = match decision {
            ApprovalDecision::Approve => allow_decision,
            ApprovalDecision::Deny => RuntimePermissionDecision::Deny {
                code: DenialCode::ApprovalDenied,
                safe_message: BoundedText::new("The Runtime operation was denied")
                    .unwrap_or_else(|_| unreachable!()),
            },
        };
        let dispatch = self
            .active
            .as_mut()
            .ok_or_else(no_active_run)?
            .handle
            .resolve_provider_permission(&permission, runtime_decision);
        let dispatched_at_ms = refreshed_now_ms(now_ms)?;
        self.require_active_lease_time(dispatched_at_ms)?;
        if let Err(error) = dispatch {
            return self.fail_unknown_provider_dispatch(error, dispatched_at_ms);
        }
        let complete_command = provider_dispatch_command(
            actor_id,
            "complete",
            approval_id,
            started.revision,
            dispatched_at_ms,
        )?;
        if self
            .coordinator
            .store
            .complete_provider_permission_dispatch(&complete_command, approval_id, started.revision)
            .is_err()
        {
            return self.fail_unknown_provider_dispatch(
                runtime_lost_error(
                    "provider_permission_receipt_unknown",
                    "Provider accepted a response whose receipt could not be persisted",
                )?,
                dispatched_at_ms,
            );
        }
        let active = self.active.as_mut().ok_or_else(no_active_run)?;
        active.pending_permission = None;
        active.expected_provider_terminal = match (decision, expects_provider_denial) {
            (ApprovalDecision::Deny, true) => Some(ExpectedProviderTerminal::Denied {
                approval_id: approval_id.clone(),
                permission,
            }),
            (ApprovalDecision::Deny | ApprovalDecision::Approve, false)
            | (ApprovalDecision::Approve, true) => None,
        };
        Ok(SchedulerTick::Progressed(view))
    }

    fn fail_unknown_provider_dispatch(
        &mut self,
        error: ContractError,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let run_id = self
            .active
            .as_ref()
            .ok_or_else(no_active_run)?
            .scheduled
            .run_id
            .clone();
        self.coordinator
            .store
            .mark_provider_dispatches_unknown_for_run(&run_id, now_ms)?;
        let shutdown_acknowledged = self
            .active
            .as_mut()
            .ok_or_else(no_active_run)?
            .handle
            .shutdown(CancelReason::RuntimeShutdown)
            .is_ok();
        if !shutdown_acknowledged {
            self.active.as_mut().ok_or_else(no_active_run)?.abort_error = Some(error);
            return Err(GatewayDaemonError::Protocol(
                "Runtime cancellation after an indeterminate provider response was not acknowledged"
                    .to_owned(),
            ));
        }
        self.finish_failed(error, refreshed_now_ms(now_ms)?)
    }
}
