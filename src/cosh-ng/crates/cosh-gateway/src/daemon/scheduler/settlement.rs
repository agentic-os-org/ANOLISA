impl<F: RuntimeFactory> TaskScheduler<F> {
    fn ensure_active_operation_budget(&mut self, now_ms: u64) -> Result<(), GatewayDaemonError> {
        let active = self.active.as_mut().ok_or_else(no_active_run)?;
        renew_for_operation(
            &mut self.coordinator,
            &active.scheduled.actor.actor_id,
            &mut active.lease,
            &mut active.lease_expires_at_ms,
            self.config,
            now_ms,
        )
    }

    fn require_active_lease_time(&self, now_ms: u64) -> Result<(), GatewayDaemonError> {
        let active = self.active.as_ref().ok_or_else(no_active_run)?;
        if now_ms >= active.lease_expires_at_ms {
            Err(stale_operation_error())
        } else {
            Ok(())
        }
    }

    fn finish_succeeded(&mut self, now_ms: u64) -> Result<SchedulerTick, GatewayDaemonError> {
        self.active.as_mut().ok_or_else(no_active_run)?.terminal = Some(TerminalOutcome::Succeeded);
        self.finish_terminal(now_ms)
    }

    fn finish_failed(
        &mut self,
        error: ContractError,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        self.active.as_mut().ok_or_else(no_active_run)?.terminal =
            Some(TerminalOutcome::Failed(error));
        self.finish_terminal(now_ms)
    }

    fn finish_cancelled(&mut self, now_ms: u64) -> Result<SchedulerTick, GatewayDaemonError> {
        self.active.as_mut().ok_or_else(no_active_run)?.terminal = Some(TerminalOutcome::Cancelled);
        self.finish_terminal(now_ms)
    }

    fn finish_terminal(&mut self, now_ms: u64) -> Result<SchedulerTick, GatewayDaemonError> {
        self.require_active_lease_time(now_ms)?;
        let (actor_id, run_id, lease, binding, terminal, binding_closed, task_settled) = {
            let active = self.active.as_ref().ok_or_else(no_active_run)?;
            (
                active.scheduled.actor.actor_id.clone(),
                active.scheduled.run_id.clone(),
                active.lease.clone(),
                active.binding.clone(),
                active.terminal.clone().ok_or_else(no_active_run)?,
                active.binding_closed,
                active.task_settled,
            )
        };
        self.coordinator
            .store
            .cancel_pending_approvals_for_run(&run_id, now_ms)?;
        if !binding_closed {
            self.coordinator
                .close_runtime_binding(&actor_id, &lease, &binding, now_ms)?;
            self.active
                .as_mut()
                .ok_or_else(no_active_run)?
                .binding_closed = true;
        }
        let view = if task_settled {
            let task_id = &self
                .active
                .as_ref()
                .ok_or_else(no_active_run)?
                .scheduled
                .task_id;
            TaskView::from(&self.coordinator.store.load_task(task_id)?)
        } else {
            let view = match terminal {
                TerminalOutcome::Succeeded => self.coordinator.settle_succeeded(&lease, now_ms)?,
                TerminalOutcome::Failed(error) => {
                    self.coordinator.settle_failed(&lease, error, now_ms)?
                }
                TerminalOutcome::Cancelled => self.coordinator.settle_cancelled(&lease, now_ms)?,
            };
            self.active.as_mut().ok_or_else(no_active_run)?.task_settled = true;
            view
        };
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_terminal_lease_release) {
            return Err(GatewayDaemonError::Protocol(
                "injected failure before terminal Run lease release".to_owned(),
            ));
        }
        self.coordinator.release_lease(&lease, now_ms)?;
        self.active.take();
        Ok(SchedulerTick::Settled(view))
    }
}
