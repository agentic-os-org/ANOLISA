impl<F: RuntimeFactory> TaskScheduler<F> {
    pub(super) fn expire_active_brokered_approval(
        &mut self,
        approval: &ApprovalRecord,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let command = LedgerCommand {
            actor_id: approval.actor_id.clone(),
            idempotency_key: IdempotencyKey::new(format!(
                "scheduler-expire-brokered-{}",
                approval.approval_id.as_str()
            ))
            .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
            command_digest: digest_json(&(
                "expire_brokered_approval",
                &approval.approval_id,
                approval.revision,
            ))?,
            committed_at_ms: now_ms,
        };
        let resolved = self.coordinator.store.resolve_approval(
            &command,
            &approval.approval_id,
            approval.revision,
            crate::storage::ApprovalResolution::Cancel,
        )?;
        let expired = match resolved {
            LedgerOutcome::Applied(record) | LedgerOutcome::Replayed(record) => record,
        };
        if expired.state != ApprovalState::Expired {
            return Err(GatewayDaemonError::Protocol(
                "brokered approval expiry did not persist the expired state".to_owned(),
            ));
        }
        let error = runtime_lost_error(
            "brokered_approval_expired",
            "The brokered approval expired before it was resolved",
        )?;
        self.shutdown_after_brokered_failure(error, now_ms)
    }

    fn finish_suspended_after_brokered_result(
        &mut self,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let stopped = self
            .active
            .as_mut()
            .ok_or_else(no_active_run)?
            .handle
            .shutdown(CancelReason::RuntimeShutdown)
            .is_ok();
        if !stopped {
            return Err(GatewayDaemonError::Protocol(
                "Runtime shutdown after an uncertain execution was not acknowledged".to_owned(),
            ));
        }
        let (actor_id, task_id, lease, binding) = {
            let active = self.active.as_ref().ok_or_else(no_active_run)?;
            (
                active.scheduled.actor.actor_id.clone(),
                active.scheduled.task_id.clone(),
                active.lease.clone(),
                active.binding.clone(),
            )
        };
        self.coordinator
            .close_runtime_binding(&actor_id, &lease, &binding, now_ms)?;
        let task = self.coordinator.store.load_task(&task_id)?;
        if task.state() != TaskState::Suspended {
            return Err(GatewayDaemonError::Protocol(
                "uncertain brokered execution did not suspend its Task".to_owned(),
            ));
        }
        self.coordinator.release_lease(&lease, now_ms)?;
        self.active.take();
        Ok(SchedulerTick::Settled(TaskView::from(&task)))
    }

    fn reject_replayed_brokered_dispatch(
        &mut self,
        record: BrokeredRuntimeDispatchRecord,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let error = runtime_lost_error(
            "brokered_runtime_dispatch_replayed",
            "Brokered Runtime callback dispatch cannot be safely replayed",
        )?;
        if record.state == BrokeredRuntimeDispatchState::Started {
            let (actor_id, lease) = {
                let active = self.active.as_ref().ok_or_else(no_active_run)?;
                (
                    active.scheduled.actor.actor_id.clone(),
                    active.lease.clone(),
                )
            };
            return self.fail_unknown_brokered_dispatch(&actor_id, &lease, &record, error, now_ms);
        }
        self.shutdown_after_brokered_failure(error, now_ms)
    }

    fn fail_unknown_brokered_dispatch(
        &mut self,
        actor_id: &ActorId,
        lease: &LeaseClaim,
        record: &BrokeredRuntimeDispatchRecord,
        error: ContractError,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let command = brokered_dispatch_command(
            actor_id,
            "unknown",
            record.kind,
            &record.brokered,
            record.revision,
            now_ms,
        )?;
        match self
            .coordinator
            .store
            .mark_brokered_runtime_dispatch_unknown(
                &command,
                record.kind,
                &record.brokered,
                &record.payload_digest,
                record.revision,
                lease,
            )? {
            LedgerOutcome::Applied(marked)
                if marked.state == BrokeredRuntimeDispatchState::Unknown => {}
            LedgerOutcome::Applied(_) | LedgerOutcome::Replayed(_) => {
                return Err(GatewayDaemonError::Protocol(
                    "brokered Runtime dispatch could not be marked indeterminate".to_owned(),
                ))
            }
        }
        let task_id = self
            .active
            .as_ref()
            .ok_or_else(no_active_run)?
            .scheduled
            .task_id
            .clone();
        if self.coordinator.store.load_task(&task_id)?.state() == TaskState::Suspended {
            return self.finish_suspended_after_brokered_result(now_ms);
        }
        self.shutdown_after_brokered_failure(error, now_ms)
    }

    fn shutdown_after_brokered_failure(
        &mut self,
        error: ContractError,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let acknowledged = self
            .active
            .as_mut()
            .ok_or_else(no_active_run)?
            .handle
            .shutdown(CancelReason::RuntimeShutdown)
            .is_ok();
        if !acknowledged {
            self.active.as_mut().ok_or_else(no_active_run)?.abort_error = Some(error);
            return Err(GatewayDaemonError::Protocol(
                "Runtime shutdown after a brokered dispatch failure was not acknowledged"
                    .to_owned(),
            ));
        }
        self.finish_failed(error, refreshed_now_ms(now_ms)?)
    }
}
