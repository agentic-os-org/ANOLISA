impl<F: RuntimeFactory> TaskScheduler<F> {
    /// Performs one non-blocking claim, cancel, poll, or settlement step.
    ///
    /// # Errors
    ///
    /// Returns a storage, fencing, protocol, or Runtime-start error. Durable
    /// state is never advanced after a stale lease is detected.
    pub fn tick(&mut self, now_ms: u64) -> Result<SchedulerTick, GatewayDaemonError> {
        if self.active.is_some() {
            self.ensure_active_operation_budget(now_ms)?;
            return self.poll_active(now_ms);
        }
        if self.shutting_down {
            return Ok(SchedulerTick::Idle);
        }

        if let Some(tick) = self.process_pre_runtime_checkpoint(now_ms)? {
            return Ok(tick);
        }

        let lease_deadline = deadline(now_ms, self.config.lease_duration_ms)?;
        if let Some(view) = self.coordinator.recover_expired_active_run(
            self.brokered_driver.as_mut(),
            &self.worker_id,
            now_ms,
            lease_deadline,
        )? {
            return Ok(SchedulerTick::Settled(view));
        }
        let (claim, intent, lease) =
            match self
                .coordinator
                .claim_runtime_start(&self.worker_id, now_ms, lease_deadline)?
            {
                RuntimeStartClaim::Empty => return Ok(SchedulerTick::Idle),
                RuntimeStartClaim::Recovered(view) => return Ok(SchedulerTick::Settled(view)),
                RuntimeStartClaim::Claimed {
                    outbox,
                    intent,
                    lease,
                } => (outbox, *intent, lease),
            };
        let scheduled = ScheduledRun {
            actor: intent.actor,
            task_id: intent.task_id,
            run_id: intent.run_id,
            runtime: intent.runtime,
            intent: intent.intent,
            target: intent.target,
            workspace: intent.workspace,
            capability_profile: intent.capability_profile,
            launch: intent.launch,
            lease_generation: lease.generation,
        };
        match self.factory.open(&scheduled) {
            Ok(StartedRuntime {
                binding,
                mut handle,
            }) => {
                let opened_at_ms = refreshed_now_ms(now_ms)?;
                if opened_at_ms >= lease_deadline {
                    let _ = handle.shutdown(CancelReason::RuntimeShutdown);
                    return Err(stale_operation_error());
                }
                let mut lease = lease;
                let mut lease_expires_at_ms = lease_deadline;
                renew_for_operation(
                    &mut self.coordinator,
                    &scheduled.actor.actor_id,
                    &mut lease,
                    &mut lease_expires_at_ms,
                    self.config,
                    opened_at_ms,
                )?;
                self.coordinator
                    .persist_runtime_binding(&lease, &binding, opened_at_ms)?;
                self.coordinator.record_runtime_binding_sequence(
                    &lease,
                    &binding,
                    1,
                    opened_at_ms,
                )?;
                let bound_at_ms = refreshed_now_ms(opened_at_ms)?;
                if bound_at_ms >= lease_expires_at_ms {
                    let _ = handle.shutdown(CancelReason::RuntimeShutdown);
                    return Err(stale_operation_error());
                }
                if let Err(ack_error) = self.coordinator.store.complete_outbox(&claim, bound_at_ms)
                {
                    let abort_error = runtime_lost_error(
                        "runtime_start_unacknowledged",
                        "Runtime start could not be acknowledged durably",
                    )?;
                    let shutdown_acknowledged =
                        handle.shutdown(CancelReason::RuntimeShutdown).is_ok();
                    self.active = Some(ActiveRun {
                        scheduled,
                        lease,
                        lease_expires_at_ms,
                        next_event_sequence: 1,
                        abort_error: Some(abort_error.clone()),
                        binding,
                        terminal: None,
                        binding_closed: false,
                        task_settled: false,
                        pending_permission: None,
                        expected_provider_terminal: None,
                        pending_brokered: None,
                        pending_input: None,
                        handle,
                    });
                    if shutdown_acknowledged {
                        let stopped_at_ms = refreshed_now_ms(bound_at_ms)?;
                        self.require_active_lease_time(stopped_at_ms)?;
                        self.finish_failed(abort_error, stopped_at_ms)?;
                    }
                    return Err(ack_error.into());
                }
                let task = self.coordinator.store.load_task(&scheduled.task_id)?;
                let view = TaskView::from(&task);
                self.active = Some(ActiveRun {
                    scheduled,
                    lease,
                    lease_expires_at_ms,
                    next_event_sequence: 1,
                    abort_error: None,
                    binding,
                    terminal: None,
                    binding_closed: false,
                    task_settled: false,
                    pending_permission: None,
                    expected_provider_terminal: None,
                    pending_brokered: None,
                    pending_input: None,
                    handle,
                });
                self.ensure_active_operation_budget(bound_at_ms)?;
                let begin_result = self
                    .active
                    .as_mut()
                    .ok_or_else(no_active_run)?
                    .handle
                    .begin();
                let began_at_ms = refreshed_now_ms(bound_at_ms)?;
                self.require_active_lease_time(began_at_ms)?;
                if let Err(error) = begin_result {
                    return self.finish_failed(error, began_at_ms);
                }
                Ok(SchedulerTick::Started(view))
            }
            Err(error) => {
                let failed_at_ms = refreshed_now_ms(now_ms)?;
                if failed_at_ms >= lease_deadline {
                    return Err(stale_operation_error());
                }
                let view = self
                    .coordinator
                    .settle_failed(&lease, error, failed_at_ms)?;
                self.coordinator
                    .store
                    .complete_outbox(&claim, failed_at_ms)?;
                self.coordinator.release_lease(&lease, failed_at_ms)?;
                Ok(SchedulerTick::Settled(view))
            }
        }
    }

    /// Stops claiming work and converges the active Runtime under its lease.
    pub fn shutdown(&mut self, now_ms: u64) -> Result<SchedulerTick, GatewayDaemonError> {
        self.shutting_down = true;
        if self.active.is_none() {
            return Ok(SchedulerTick::Idle);
        }
        self.ensure_active_operation_budget(now_ms)?;
        if self
            .active
            .as_ref()
            .ok_or_else(no_active_run)?
            .terminal
            .is_some()
        {
            return self.finish_terminal(now_ms);
        }
        if let Some(abort_error) = self
            .active
            .as_ref()
            .ok_or_else(no_active_run)?
            .abort_error
            .clone()
        {
            let _ = self
                .active
                .as_mut()
                .ok_or_else(no_active_run)?
                .handle
                .shutdown(CancelReason::RuntimeShutdown);
            let stopped_at_ms = refreshed_now_ms(now_ms)?;
            self.require_active_lease_time(stopped_at_ms)?;
            return self.finish_failed(abort_error, stopped_at_ms);
        }
        self.coordinator.request_runtime_shutdown(
            &self.active.as_ref().ok_or_else(no_active_run)?.lease,
            now_ms,
        )?;
        self.cancel_pending_input(now_ms)?;
        let result = self
            .active
            .as_mut()
            .ok_or_else(no_active_run)?
            .handle
            .shutdown(CancelReason::RuntimeShutdown);
        let stopped_at_ms = refreshed_now_ms(now_ms)?;
        self.require_active_lease_time(stopped_at_ms)?;
        match result {
            Ok(()) => self.finish_cancelled(stopped_at_ms),
            // The daemon is already shutting down and will never poll this
            // handle again. Persist the observed Runtime failure and release
            // its lease so restart cannot strand or replay the Run.
            Err(error) => self.finish_failed(error, stopped_at_ms),
        }
    }
}
