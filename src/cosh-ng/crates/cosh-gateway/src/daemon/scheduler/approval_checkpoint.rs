impl<F: RuntimeFactory> TaskScheduler<F> {
    fn ensure_approval_checkpoint_barrier(
        &mut self,
        approval: &ApprovalRecord,
        now_ms: u64,
    ) -> Result<(), GatewayDaemonError> {
        let (policy, workspace, runtime_fence) = {
            let active = self.active.as_ref().ok_or_else(no_active_run)?;
            let fence = RuntimeExecutionFence {
                binding_id: active.binding.binding_id.clone(),
                runtime_generation: active.binding.runtime_generation,
                lease_generation: active.lease.generation,
                lease_revision: active.lease.revision,
            };
            (
                active.scheduled.launch.checkpoint,
                active.scheduled.workspace.clone(),
                fence,
            )
        };
        if policy == CheckpointPolicy::Off {
            return Ok(());
        }
        let existing = match self
            .coordinator
            .store
            .load_approval_checkpoint_record(&approval.approval_id)
        {
            Ok(record) => Some(record),
            Err(StoreError::LedgerNotFound { .. }) => None,
            Err(error) => return Err(error.into()),
        };
        let record = if let Some(record) = existing {
            self.validate_approval_checkpoint_record(approval, &runtime_fence, policy, record)?
        } else {
            let checkpoint_id = CheckpointId::new();
            self.coordinator.store.record_approval_checkpoint_intent(
                &approval.approval_id,
                &approval.task_id,
                &approval.run_id,
                &checkpoint_id,
                policy,
                &runtime_fence,
                now_ms,
            )?
        };
        let request = ApprovalCheckpointRequest {
            checkpoint_id: record.checkpoint_id.clone(),
            approval_id: approval.approval_id.clone(),
            task_id: approval.task_id.clone(),
            run_id: approval.run_id.clone(),
            workspace,
            runtime_fence,
        };
        let terminal = match record.state {
            ApprovalCheckpointState::Intent => {
                self.prepare_and_create_approval_checkpoint(&request, policy, now_ms)?
            }
            ApprovalCheckpointState::Started => {
                self.reconcile_started_approval_checkpoint(&request, &record, policy, now_ms)?
            }
            _ => record,
        };
        if terminal.state == ApprovalCheckpointState::Created
            || (policy == CheckpointPolicy::Auto
                && terminal.state == ApprovalCheckpointState::Skipped)
        {
            return Ok(());
        }
        Err(GatewayDaemonError::Protocol(
            "approval checkpoint barrier did not authorize Runtime Permission".to_owned(),
        ))
    }

    fn validate_approval_checkpoint_record(
        &self,
        approval: &ApprovalRecord,
        runtime_fence: &RuntimeExecutionFence,
        policy: CheckpointPolicy,
        record: ApprovalCheckpointRecord,
    ) -> Result<ApprovalCheckpointRecord, GatewayDaemonError> {
        if record.approval_id != approval.approval_id
            || record.task_id != approval.task_id
            || record.run_id != approval.run_id
            || record.policy != policy
            || record.runtime_fence != *runtime_fence
        {
            return Err(GatewayDaemonError::Protocol(
                "approval checkpoint barrier binding changed".to_owned(),
            ));
        }
        Ok(record)
    }

    fn prepare_and_create_approval_checkpoint(
        &mut self,
        request: &ApprovalCheckpointRequest,
        policy: CheckpointPolicy,
        now_ms: u64,
    ) -> Result<ApprovalCheckpointRecord, GatewayDaemonError> {
        let prepared = match self.checkpoint_driver.as_mut() {
            Some(driver) => driver.prepare_approval_checkpoint(request),
            None => Ok(ApprovalCheckpointPrepareResult::Unavailable {
                reason: approval_checkpoint_reason("checkpoint provider is not attached"),
            }),
        };
        let prepared = match prepared {
            Ok(value) => value,
            Err(error) => {
                return self
                    .coordinator
                    .store
                    .complete_approval_checkpoint_intent(
                        &request.approval_id,
                        ApprovalCheckpointState::Failed,
                        &approval_checkpoint_reason(error.safe_message.as_str()),
                        now_ms,
                    )
                    .map_err(Into::into)
            }
        };
        let binding = match prepared {
            ApprovalCheckpointPrepareResult::Prepared(binding) => binding,
            ApprovalCheckpointPrepareResult::KnownNoEffect { reason }
            | ApprovalCheckpointPrepareResult::Unavailable { reason } => {
                let state = if policy == CheckpointPolicy::Auto {
                    ApprovalCheckpointState::Skipped
                } else {
                    ApprovalCheckpointState::Failed
                };
                return self
                    .coordinator
                    .store
                    .complete_approval_checkpoint_intent(
                        &request.approval_id,
                        state,
                        &reason,
                        now_ms,
                    )
                    .map_err(Into::into);
            }
        };
        if !self.coordinator.store.start_approval_checkpoint(
            &request.approval_id,
            &binding,
            now_ms,
        )? {
            let record = self
                .coordinator
                .store
                .load_approval_checkpoint_record(&request.approval_id)?;
            return if record.state == ApprovalCheckpointState::Started {
                self.reconcile_started_approval_checkpoint(request, &record, policy, now_ms)
            } else {
                Ok(record)
            };
        }
        let result = match self.checkpoint_driver.as_mut() {
            Some(driver) => driver.create_approval_checkpoint(request, &binding),
            None => unreachable!("prepared approval checkpoint requires an attached driver"),
        };
        match result {
            Ok(ApprovalCheckpointCreateResult::Created { evidence }) => {
                self.complete_approval_checkpoint_created(request, &binding, evidence, now_ms)
            }
            Ok(ApprovalCheckpointCreateResult::KnownNoEffect { reason })
            | Ok(ApprovalCheckpointCreateResult::Unavailable { reason }) => {
                self.complete_approval_checkpoint_no_effect(request, policy, reason, now_ms)
            }
            Ok(ApprovalCheckpointCreateResult::PossiblyApplied { .. }) => {
                let record = self
                    .coordinator
                    .store
                    .load_approval_checkpoint_record(&request.approval_id)?;
                self.reconcile_started_approval_checkpoint(request, &record, policy, now_ms)
            }
            Err(error) => self.complete_approval_checkpoint_no_effect(
                request,
                CheckpointPolicy::On,
                approval_checkpoint_reason(error.safe_message.as_str()),
                now_ms,
            ),
        }
    }

    fn reconcile_started_approval_checkpoint(
        &mut self,
        request: &ApprovalCheckpointRequest,
        record: &ApprovalCheckpointRecord,
        policy: CheckpointPolicy,
        now_ms: u64,
    ) -> Result<ApprovalCheckpointRecord, GatewayDaemonError> {
        let binding = record.binding.as_ref().ok_or_else(|| {
            GatewayDaemonError::Protocol(
                "started approval checkpoint has no durable binding".to_owned(),
            )
        })?;
        let result = match self.checkpoint_driver.as_mut() {
            Some(driver) => driver.reconcile_approval_checkpoint(request, binding),
            None => Ok(ApprovalCheckpointReconcileResult::Unknown {
                reason: approval_checkpoint_reason(
                    "checkpoint reconciliation evidence is unavailable",
                ),
            }),
        };
        match result {
            Ok(ApprovalCheckpointReconcileResult::Created { evidence }) => {
                self.complete_approval_checkpoint_created(request, binding, evidence, now_ms)
            }
            Ok(ApprovalCheckpointReconcileResult::NotApplied) => self
                .complete_approval_checkpoint_no_effect(
                    request,
                    policy,
                    approval_checkpoint_reason(
                        "exact evidence proves the approval checkpoint was not applied",
                    ),
                    now_ms,
                ),
            Ok(ApprovalCheckpointReconcileResult::Unknown { reason }) => self
                .coordinator
                .store
                .complete_approval_checkpoint(
                    &request.approval_id,
                    ApprovalCheckpointState::Unknown,
                    None,
                    Some(&reason),
                    now_ms,
                )
                .map_err(Into::into),
            Err(error) => self
                .coordinator
                .store
                .complete_approval_checkpoint(
                    &request.approval_id,
                    ApprovalCheckpointState::Unknown,
                    None,
                    Some(&approval_checkpoint_reason(error.safe_message.as_str())),
                    now_ms,
                )
                .map_err(Into::into),
        }
    }

    fn complete_approval_checkpoint_created(
        &mut self,
        request: &ApprovalCheckpointRequest,
        binding: &PreRuntimeCheckpointBinding,
        evidence: ApprovalCheckpointEvidence,
        now_ms: u64,
    ) -> Result<ApprovalCheckpointRecord, GatewayDaemonError> {
        let (state, evidence, reason) = if evidence.checkpoint_id == request.checkpoint_id
            && evidence.provider_generation == binding.provider_generation
        {
            (ApprovalCheckpointState::Created, Some(evidence), None)
        } else {
            (
                ApprovalCheckpointState::Unknown,
                None,
                Some(approval_checkpoint_reason(
                    "approval checkpoint evidence binding did not match",
                )),
            )
        };
        self.coordinator
            .store
            .complete_approval_checkpoint(
                &request.approval_id,
                state,
                evidence.as_ref(),
                reason.as_ref(),
                now_ms,
            )
            .map_err(Into::into)
    }

    fn complete_approval_checkpoint_no_effect(
        &mut self,
        request: &ApprovalCheckpointRequest,
        policy: CheckpointPolicy,
        reason: BoundedText,
        now_ms: u64,
    ) -> Result<ApprovalCheckpointRecord, GatewayDaemonError> {
        let state = if policy == CheckpointPolicy::Auto {
            ApprovalCheckpointState::Skipped
        } else {
            ApprovalCheckpointState::Failed
        };
        self.coordinator
            .store
            .complete_approval_checkpoint(&request.approval_id, state, None, Some(&reason), now_ms)
            .map_err(Into::into)
    }
}

fn approval_checkpoint_reason(value: &str) -> BoundedText {
    BoundedText::new(value).unwrap_or_else(|_| {
        BoundedText::new("approval checkpoint failed").unwrap_or_else(|_| unreachable!())
    })
}
