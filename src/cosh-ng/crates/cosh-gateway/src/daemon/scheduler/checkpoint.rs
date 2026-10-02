impl<F: RuntimeFactory> TaskScheduler<F> {
    fn process_pre_runtime_checkpoint(
        &mut self,
        now_ms: u64,
    ) -> Result<Option<SchedulerTick>, GatewayDaemonError> {
        let delivery_kind = pre_runtime_checkpoint_delivery_kind();
        let Some(candidate) = self
            .coordinator
            .store
            .peek_ready_outbox(&delivery_kind, now_ms)?
        else {
            return Ok(None);
        };
        let intent = decode_runtime_start_intent(
            candidate.payload.clone(),
            &self.coordinator.launch_catalog,
        )?;
        if intent.task_id != candidate.task_id || intent.launch.checkpoint == CheckpointPolicy::Off
        {
            return Err(GatewayDaemonError::Protocol(
                "pre-Runtime checkpoint Outbox has invalid launch data".to_owned(),
            ));
        }
        let baseline_id = intent.baseline_id.clone().ok_or_else(|| {
            GatewayDaemonError::Protocol(
                "pre-Runtime checkpoint Outbox has no baseline identity".to_owned(),
            )
        })?;
        let lease_deadline = deadline(now_ms, self.config.lease_duration_ms)?;
        let Some(claim) = self.coordinator.store.claim_outbox_candidate(
            &delivery_kind,
            &candidate,
            &self.worker_id,
            now_ms,
            lease_deadline,
        )?
        else {
            return Ok(Some(SchedulerTick::Idle));
        };
        let request = PreRuntimeCheckpointRequest {
            baseline_id: baseline_id.clone(),
            task_id: intent.task_id.clone(),
            run_id: intent.run_id.clone(),
            workspace: intent.workspace.clone(),
        };
        let existing = self
            .coordinator
            .store
            .load_pre_runtime_baseline_record(&claim.task_id)?;
        let (resolution, durable_binding) = if let Some(existing) = existing {
            if existing.view.baseline_id != baseline_id
                || existing.view.state != PreRuntimeBaselineState::Started
            {
                return Err(GatewayDaemonError::Protocol(
                    "checkpoint Outbox does not match its durable started baseline".to_owned(),
                ));
            }
            match existing.binding {
                None => (
                    BaselineResolution::Unknown(
                        BoundedText::new(
                            "legacy checkpoint attempt has no exact durable provider binding",
                        )
                        .unwrap_or_else(|_| unreachable!()),
                    ),
                    None,
                ),
                Some(binding) => match self.checkpoint_driver.as_mut() {
                    Some(driver) => match driver.reconcile_baseline(&request, &binding) {
                        Ok(PreRuntimeCheckpointReconcileResult::Created { evidence }) => {
                            (BaselineResolution::Created(evidence), Some(binding))
                        }
                        Ok(PreRuntimeCheckpointReconcileResult::NotApplied) => (
                            BaselineResolution::KnownNoEffect(
                                BoundedText::new(
                                    "exact evidence proves the baseline was not applied",
                                )
                                .unwrap_or_else(|_| unreachable!()),
                            ),
                            Some(binding),
                        ),
                        Ok(PreRuntimeCheckpointReconcileResult::Unknown { reason }) => {
                            (BaselineResolution::Unknown(reason), Some(binding))
                        }
                        Err(error) => (
                            BaselineResolution::Unknown(
                                BoundedText::new(error.safe_message.as_str())
                                    .unwrap_or_else(|_| unreachable!()),
                            ),
                            Some(binding),
                        ),
                    },
                    None => (
                        BaselineResolution::Unknown(
                            BoundedText::new("checkpoint reconciliation evidence is unavailable")
                                .unwrap_or_else(|_| unreachable!()),
                        ),
                        Some(binding),
                    ),
                },
            }
        } else {
            let prepared = match self.checkpoint_driver.as_mut() {
                Some(driver) => match driver.prepare_baseline(&request) {
                    Ok(binding) => Some(binding),
                    Err(error) => {
                        let inserted = self.coordinator.store.record_pre_runtime_baseline_started(
                            &claim,
                            &baseline_id,
                            &intent.run_id,
                            intent.launch.checkpoint,
                            None,
                            now_ms,
                        )?;
                        if !inserted {
                            self.coordinator.store.retry_outbox(
                                &claim,
                                now_ms,
                                now_ms.saturating_add(1),
                            )?;
                            return Ok(Some(SchedulerTick::Idle));
                        }
                        let reason = BoundedText::new(error.safe_message.as_str())
                            .unwrap_or_else(|_| unreachable!());
                        let resolution = BaselineResolution::KnownNoEffect(reason);
                        return self.complete_pre_runtime_checkpoint(
                            &claim,
                            &intent,
                            &baseline_id,
                            resolution,
                            None,
                            now_ms,
                        );
                    }
                },
                None => None,
            };
            let inserted = self.coordinator.store.record_pre_runtime_baseline_started(
                &claim,
                &baseline_id,
                &intent.run_id,
                intent.launch.checkpoint,
                prepared.as_ref(),
                now_ms,
            )?;
            if !inserted {
                self.coordinator
                    .store
                    .retry_outbox(&claim, now_ms, now_ms.saturating_add(1))?;
                return Ok(Some(SchedulerTick::Idle));
            }
            match (self.checkpoint_driver.as_mut(), prepared) {
                (Some(driver), Some(binding)) => match driver.create_baseline(&request, &binding) {
                    Ok(PreRuntimeCheckpointCreateResult::Created { evidence }) => {
                        (BaselineResolution::Created(evidence), Some(binding))
                    }
                    Ok(PreRuntimeCheckpointCreateResult::KnownNoEffect { reason })
                    | Ok(PreRuntimeCheckpointCreateResult::Unavailable { reason }) => {
                        (BaselineResolution::KnownNoEffect(reason), Some(binding))
                    }
                    Ok(PreRuntimeCheckpointCreateResult::PossiblyApplied { .. }) => {
                        self.coordinator.store.retry_outbox(
                            &claim,
                            now_ms,
                            now_ms.saturating_add(1),
                        )?;
                        return Ok(Some(SchedulerTick::Idle));
                    }
                    Err(error) => (
                        BaselineResolution::KnownNoEffect(
                            BoundedText::new(error.safe_message.as_str())
                                .unwrap_or_else(|_| unreachable!()),
                        ),
                        Some(binding),
                    ),
                },
                (None, None) => (
                    BaselineResolution::KnownNoEffect(
                        BoundedText::new("checkpoint provider is not attached")
                            .unwrap_or_else(|_| unreachable!()),
                    ),
                    None,
                ),
                _ => unreachable!("prepared bindings require an attached checkpoint driver"),
            }
        };
        self.complete_pre_runtime_checkpoint(
            &claim,
            &intent,
            &baseline_id,
            resolution,
            durable_binding.as_ref(),
            now_ms,
        )
    }

    fn complete_pre_runtime_checkpoint(
        &mut self,
        claim: &OutboxClaim,
        intent: &RuntimeStartIntent,
        baseline_id: &CheckpointId,
        resolution: BaselineResolution,
        binding: Option<&PreRuntimeCheckpointBinding>,
        now_ms: u64,
    ) -> Result<Option<SchedulerTick>, GatewayDaemonError> {
        let (state, evidence, reason, start_runtime) = match resolution {
            BaselineResolution::Created(evidence)
                if evidence.baseline_id == *baseline_id
                    && binding.is_some_and(|binding| {
                        binding.provider_generation == evidence.provider_generation
                    }) =>
            {
                (PreRuntimeBaselineState::Created, Some(evidence), None, true)
            }
            BaselineResolution::Created(_) => (
                PreRuntimeBaselineState::Unknown,
                None,
                Some(
                    BoundedText::new(
                        "checkpoint evidence identity does not match the Task baseline",
                    )
                    .unwrap_or_else(|_| unreachable!()),
                ),
                false,
            ),
            BaselineResolution::KnownNoEffect(reason)
                if intent.launch.checkpoint == CheckpointPolicy::Auto =>
            {
                (PreRuntimeBaselineState::Skipped, None, Some(reason), true)
            }
            BaselineResolution::KnownNoEffect(reason) => {
                (PreRuntimeBaselineState::Failed, None, Some(reason), false)
            }
            BaselineResolution::Unknown(reason) => {
                (PreRuntimeBaselineState::Unknown, None, Some(reason), false)
            }
        };
        let runtime_delivery = start_runtime.then(|| OutboxIntent {
            delivery_id: DeliveryId::new(),
            event_id: claim.event_id.clone(),
            delivery_kind: runtime_start_delivery_kind(),
            payload: claim.payload.clone(),
            next_attempt_at_ms: now_ms,
        });
        let lifecycle_events = match state {
            PreRuntimeBaselineState::Failed => {
                let error = ContractError::new(
                    "required_checkpoint_failed",
                    ErrorCategory::RuntimeUnavailable,
                    false,
                    reason.as_ref().map_or(
                        "Required checkpoint failed before Runtime start",
                        BoundedText::as_str,
                    ),
                )
                .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?;
                vec![
                    TaskEvent::RunFailed {
                        run_id: intent.run_id.clone(),
                        error: error.clone(),
                    },
                    TaskEvent::TaskFailed { error },
                ]
            }
            PreRuntimeBaselineState::Unknown => vec![TaskEvent::RunSuspended {
                run_id: intent.run_id.clone(),
                reason: SuspensionCode::OperatorRequired,
            }],
            _ => Vec::new(),
        };
        self.coordinator.store.complete_pre_runtime_baseline(
            claim,
            state,
            evidence.as_ref(),
            reason.as_ref(),
            runtime_delivery.as_ref(),
            &lifecycle_events,
            now_ms,
        )?;
        let task = self.coordinator.store.load_task(&intent.task_id)?;
        Ok(Some(SchedulerTick::Progressed(
            self.coordinator.task_view(&task)?,
        )))
    }
}

enum BaselineResolution {
    Created(super::PreRuntimeCheckpointEvidence),
    KnownNoEffect(BoundedText),
    Unknown(BoundedText),
}
