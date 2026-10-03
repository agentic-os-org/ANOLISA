impl<F: RuntimeFactory> TaskScheduler<F> {
    /// Persists and dispatches one exact actor response to a pending Runtime question.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_input(
        &mut self,
        actor_id: &ActorId,
        idempotency_key: IdempotencyKey,
        task_id: &TaskId,
        request_id: &InputRequestId,
        response: RuntimeInputResponse,
        expected_task_revision: Option<u64>,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let request = self
            .coordinator
            .store
            .load_runtime_input_request(request_id)?;
        if request.actor_id != *actor_id || request.task_id != *task_id {
            return Err(GatewayDaemonError::Unauthorized);
        }
        let task = self.coordinator.store.load_task(task_id)?;
        if task.owner_actor_id() != actor_id {
            return Err(GatewayDaemonError::Unauthorized);
        }
        let active_matches = self.active.as_ref().is_some_and(|active| {
            active.scheduled.actor.actor_id == *actor_id
                && active.scheduled.task_id == *task_id
                && active.scheduled.run_id == request.run_id
                && active
                    .pending_input
                    .as_ref()
                    .is_some_and(|pending| pending.request.request_id() == request_id)
        });
        if request.state == RuntimeInputRequestState::Pending && !active_matches {
            return Err(GatewayDaemonError::Protocol(
                "Runtime input resolution requires its live waiting handle".to_owned(),
            ));
        }
        if request.state == RuntimeInputRequestState::Pending && now_ms >= request.expires_at_ms {
            self.expire_active_input(now_ms)?;
            return Err(GatewayDaemonError::Protocol(
                "Runtime input request is no longer resolvable".to_owned(),
            ));
        }
        let expected_revision = expected_task_revision.unwrap_or(task.revision());
        let command = LedgerCommand {
            actor_id: actor_id.clone(),
            idempotency_key,
            command_digest: digest_json(&(
                "resolve_runtime_input",
                task_id,
                request_id,
                &response,
                expected_task_revision,
            ))?,
            committed_at_ms: now_ms,
        };
        let prepared = match self.coordinator.store.resolve_runtime_input(
            &command,
            request_id,
            expected_revision,
            &response,
        )? {
            LedgerOutcome::Applied(record) | LedgerOutcome::Replayed(record) => record,
        };
        if prepared.actor_id != *actor_id
            || prepared.task_id != *task_id
            || prepared.run_id != request.run_id
            || prepared.request_id != *request_id
            || prepared.response != response
        {
            return Err(GatewayDaemonError::Unauthorized);
        }
        match prepared.state {
            RuntimeInputDispatchState::Delivered => {
                return Ok(SchedulerTick::Progressed(TaskView::from(
                    &self.coordinator.store.load_task(task_id)?,
                )));
            }
            RuntimeInputDispatchState::Started | RuntimeInputDispatchState::Unknown => {
                if !active_matches {
                    return Err(GatewayDaemonError::Protocol(
                        "Runtime input response delivery is indeterminate".to_owned(),
                    ));
                }
                return self.fail_unknown_input_dispatch(&prepared, now_ms);
            }
            RuntimeInputDispatchState::Prepared => {}
        }
        if !active_matches {
            return Err(GatewayDaemonError::Protocol(
                "Prepared Runtime input has no matching live handle".to_owned(),
            ));
        }
        self.ensure_active_operation_budget(now_ms)?;
        let lease = self
            .active
            .as_ref()
            .ok_or_else(no_active_run)?
            .lease
            .clone();
        let start_command =
            runtime_input_command(actor_id, "start", request_id, prepared.revision, now_ms)?;
        let started = match self.coordinator.store.start_runtime_input_dispatch(
            &start_command,
            request_id,
            &prepared.response_digest,
            prepared.revision,
            &lease,
        )? {
            LedgerOutcome::Applied(started)
                if started.state == RuntimeInputDispatchState::Started =>
            {
                started
            }
            LedgerOutcome::Applied(_) => {
                return Err(GatewayDaemonError::Protocol(
                    "Runtime input dispatch start reached an invalid state".to_owned(),
                ));
            }
            LedgerOutcome::Replayed(replayed)
                if replayed.state == RuntimeInputDispatchState::Delivered =>
            {
                return Ok(SchedulerTick::Progressed(TaskView::from(
                    &self.coordinator.store.load_task(task_id)?,
                )));
            }
            LedgerOutcome::Replayed(replayed) => {
                return self.fail_unknown_input_dispatch(&replayed, now_ms);
            }
        };
        let dispatch = self
            .active
            .as_mut()
            .ok_or_else(no_active_run)?
            .handle
            .resolve_input(&request.request, started.response.clone());
        let dispatched_at_ms = refreshed_now_ms(now_ms)?;
        self.require_active_lease_time(dispatched_at_ms)?;
        if dispatch.is_err() {
            return self.fail_unknown_input_dispatch(&started, dispatched_at_ms);
        }
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_input_dispatch_completion) {
            return self.fail_unknown_input_dispatch(&started, dispatched_at_ms);
        }
        let complete_command = runtime_input_command(
            actor_id,
            "complete",
            request_id,
            started.revision,
            dispatched_at_ms,
        )?;
        let completed = self.coordinator.store.complete_runtime_input_dispatch(
            &complete_command,
            request_id,
            &started.response_digest,
            started.revision,
            &lease,
        );
        match completed {
            Ok(LedgerOutcome::Applied(record))
                if record.state == RuntimeInputDispatchState::Delivered => {}
            Ok(LedgerOutcome::Replayed(record))
                if record.state == RuntimeInputDispatchState::Delivered => {}
            Ok(_) => {
                return self.fail_unknown_input_dispatch(&started, dispatched_at_ms);
            }
            Err(_) => {
                let observed = self
                    .coordinator
                    .store
                    .load_runtime_input_dispatch(request_id);
                if !matches!(
                    observed,
                    Ok(RuntimeInputDispatchRecord {
                        state: RuntimeInputDispatchState::Delivered,
                        ..
                    })
                ) {
                    return self.fail_unknown_input_dispatch(&started, dispatched_at_ms);
                }
            }
        }
        self.active
            .as_mut()
            .ok_or_else(no_active_run)?
            .pending_input = None;
        Ok(SchedulerTick::Progressed(TaskView::from(
            &self.coordinator.store.load_task(task_id)?,
        )))
    }

    fn fail_unknown_input_dispatch(
        &mut self,
        started: &RuntimeInputDispatchRecord,
        now_ms: u64,
    ) -> Result<SchedulerTick, GatewayDaemonError> {
        let lease = self
            .active
            .as_ref()
            .ok_or_else(no_active_run)?
            .lease
            .clone();
        let command = runtime_input_command(
            &started.actor_id,
            "unknown",
            &started.request_id,
            started.revision,
            now_ms,
        )?;
        let marked = self.coordinator.store.mark_runtime_input_dispatch_unknown(
            &command,
            &started.request_id,
            &started.response_digest,
            started.revision,
            &lease,
        );
        let marked = match marked {
            Ok(LedgerOutcome::Applied(record)) | Ok(LedgerOutcome::Replayed(record)) => record,
            Err(error) => {
                let _ = self
                    .active
                    .as_mut()
                    .ok_or_else(no_active_run)?
                    .handle
                    .shutdown(CancelReason::RuntimeShutdown);
                return Err(error.into());
            }
        };
        if marked.state != RuntimeInputDispatchState::Unknown {
            return Err(GatewayDaemonError::Protocol(
                "Runtime input dispatch did not reach Unknown".to_owned(),
            ));
        }
        self.active
            .as_mut()
            .ok_or_else(no_active_run)?
            .pending_input = None;
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_input_unknown_cleanup) {
            return Err(GatewayDaemonError::Protocol(
                "injected failure before uncertain input Runtime cleanup".to_owned(),
            ));
        }
        self.finish_suspended_after_input(now_ms)
    }

}
