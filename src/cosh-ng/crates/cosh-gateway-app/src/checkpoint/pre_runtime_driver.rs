struct PreRuntimeCheckpointOperation {
    baseline_id: CheckpointId,
    binding: CheckpointBinding,
    digest: Digest,
}
struct ApprovalCheckpointOperation {
    checkpoint_id: CheckpointId,
    binding: CheckpointBinding,
    digest: Digest,
}

impl PreRuntimeCheckpointDriver for PreRuntimeCheckpointAdapter {
    fn prepare_baseline(
        &mut self,
        request: &PreRuntimeCheckpointRequest,
    ) -> Result<PreRuntimeCheckpointBinding, ContractError> {
        let operation = self.prepare_operation(request)?;
        Ok(PreRuntimeCheckpointBinding {
            provider_workspace_id: BoundedOpaque::new(operation.binding.ws_id.clone()).map_err(
                |_| {
                    pre_runtime_checkpoint_error(
                        "checkpoint_workspace_identity_invalid",
                        ErrorCategory::Internal,
                        false,
                        "Checkpoint workspace identity exceeded its bounded contract",
                    )
                },
            )?,
            provider_generation: BoundedOpaque::new(hex_bytes(&operation.binding.generation))
                .map_err(|_| {
                    pre_runtime_checkpoint_error(
                        "checkpoint_generation_invalid",
                        ErrorCategory::Internal,
                        false,
                        "Checkpoint generation evidence was invalid",
                    )
                })?,
            operation_digest: operation.digest,
        })
    }

    fn create_baseline(
        &mut self,
        request: &PreRuntimeCheckpointRequest,
        binding: &PreRuntimeCheckpointBinding,
    ) -> Result<PreRuntimeCheckpointCreateResult, ContractError> {
        let operation = self.operation_from_binding(request, binding)?;
        let operation_digest = digest_bytes(&operation.digest)?;
        let generation = WorkspaceGenerationTokenV2::from_bytes(operation.binding.generation);
        match self.endpoint.client.guarded_create_v2(
            &operation.binding.ws_id,
            generation,
            operation.baseline_id.as_str(),
            operation_digest,
            Some("COSH durable Task baseline"),
            None,
            false,
        ) {
            Ok(evidence)
                if matches!(
                    &evidence.outcome,
                    GuardedCheckpointOutcomeV2::Created { .. }
                ) =>
            {
                Ok(PreRuntimeCheckpointCreateResult::Created {
                    evidence: Self::evidence(&operation, evidence)?,
                })
            }
            Ok(evidence) => {
                let message = match &evidence.outcome {
                    GuardedCheckpointOutcomeV2::Skipped { reason }
                        if reason == "Empty workspace, no snapshot created." =>
                    {
                        "This ws-ckpt daemon cannot create a checkpoint for an empty workspace. \
                         Upgrade ws-ckpt or add a file, then submit a new Task; \
                         alternatively submit with checkpoint=off."
                    }
                    _ => {
                        "The checkpoint provider did not create the required snapshot. \
                         Check ws-ckpt, then submit a new Task; \
                         alternatively submit with checkpoint=off."
                    }
                };
                Ok(PreRuntimeCheckpointCreateResult::KnownNoEffect {
                    reason: bounded_text(message)?,
                })
            }
            Err(failure) if failure.effect == CkptRequestEffect::KnownNoEffect => {
                Ok(PreRuntimeCheckpointCreateResult::KnownNoEffect {
                    reason: bounded_text(
                        "Checkpoint daemon rejected the baseline before applying it",
                    )?,
                })
            }
            Err(_) => Ok(PreRuntimeCheckpointCreateResult::PossiblyApplied {
                error: pre_runtime_checkpoint_error(
                    "checkpoint_create_possibly_applied",
                    ErrorCategory::Transport,
                    false,
                    "Checkpoint transport failed after baseline dispatch",
                ),
            }),
        }
    }

    fn reconcile_baseline(
        &mut self,
        request: &PreRuntimeCheckpointReconcileRequest,
        binding: &PreRuntimeCheckpointBinding,
    ) -> Result<PreRuntimeCheckpointReconcileResult, ContractError> {
        let operation = self.operation_from_binding(request, binding)?;
        let operation_digest = digest_bytes(&operation.digest)?;
        let generation = WorkspaceGenerationTokenV2::from_bytes(operation.binding.generation);
        match self.endpoint.client.checkpoint_evidence_v2(
            &operation.binding.ws_id,
            generation,
            operation.baseline_id.as_str(),
            operation_digest,
        ) {
            Ok(Some(evidence))
                if matches!(
                    &evidence.outcome,
                    GuardedCheckpointOutcomeV2::Created { .. }
                ) =>
            {
                Ok(PreRuntimeCheckpointReconcileResult::Created {
                    evidence: Self::evidence(&operation, evidence)?,
                })
            }
            Ok(Some(_)) => Ok(PreRuntimeCheckpointReconcileResult::NotApplied),
            Ok(None) => Ok(PreRuntimeCheckpointReconcileResult::Unknown {
                reason: bounded_text(
                    "No exact durable checkpoint evidence exists for this baseline",
                )?,
            }),
            Err(_) => Ok(PreRuntimeCheckpointReconcileResult::Unknown {
                reason: bounded_text("Exact checkpoint evidence could not be recovered")?,
            }),
        }
    }

    fn prepare_approval_checkpoint(
        &mut self,
        request: &ApprovalCheckpointRequest,
    ) -> Result<ApprovalCheckpointPrepareResult, ContractError> {
        let operation = self.prepare_approval_operation(request)?;
        Ok(ApprovalCheckpointPrepareResult::Prepared(
            PreRuntimeCheckpointBinding {
                provider_workspace_id: BoundedOpaque::new(operation.binding.ws_id.clone())
                    .map_err(|_| {
                        checkpoint_error("checkpoint_workspace_identity_invalid", false)
                    })?,
                provider_generation: BoundedOpaque::new(hex_bytes(&operation.binding.generation))
                    .map_err(|_| {
                    checkpoint_error("checkpoint_generation_invalid", false)
                })?,
                operation_digest: operation.digest,
            },
        ))
    }

    fn create_approval_checkpoint(
        &mut self,
        request: &ApprovalCheckpointRequest,
        binding: &PreRuntimeCheckpointBinding,
    ) -> Result<ApprovalCheckpointCreateResult, ContractError> {
        let operation = self.approval_operation_from_binding(request, binding)?;
        let operation_digest = digest_bytes(&operation.digest)?;
        let generation = WorkspaceGenerationTokenV2::from_bytes(operation.binding.generation);
        let metadata = serde_json::json!({
            "task_id": request.task_id.as_str(),
            "run_id": request.run_id.as_str(),
            "approval_id": request.approval_id.as_str(),
        })
        .to_string();
        match self.endpoint.client.guarded_create_v2(
            &operation.binding.ws_id,
            generation,
            operation.checkpoint_id.as_str(),
            operation_digest,
            Some("COSH Task pre-approval checkpoint"),
            Some(&metadata),
            false,
        ) {
            Ok(evidence)
                if matches!(
                    &evidence.outcome,
                    GuardedCheckpointOutcomeV2::Created { .. }
                ) =>
            {
                Ok(ApprovalCheckpointCreateResult::Created {
                    evidence: Self::approval_evidence(&operation, evidence)?,
                })
            }
            Ok(_) => Ok(ApprovalCheckpointCreateResult::KnownNoEffect {
                reason: bounded_text("Checkpoint provider skipped the approval checkpoint")?,
            }),
            Err(failure) if failure.effect == CkptRequestEffect::KnownNoEffect => {
                Ok(ApprovalCheckpointCreateResult::KnownNoEffect {
                    reason: bounded_text(
                        "Checkpoint daemon rejected the approval checkpoint before applying it",
                    )?,
                })
            }
            Err(_) => Ok(ApprovalCheckpointCreateResult::PossiblyApplied {
                error: checkpoint_error("checkpoint_create_possibly_applied", false),
            }),
        }
    }

    fn reconcile_approval_checkpoint(
        &mut self,
        request: &ApprovalCheckpointRequest,
        binding: &PreRuntimeCheckpointBinding,
    ) -> Result<ApprovalCheckpointReconcileResult, ContractError> {
        let operation = self.approval_operation_from_binding(request, binding)?;
        let generation = WorkspaceGenerationTokenV2::from_bytes(operation.binding.generation);
        match self.endpoint.client.checkpoint_evidence_v2(
            &operation.binding.ws_id,
            generation,
            operation.checkpoint_id.as_str(),
            digest_bytes(&operation.digest)?,
        ) {
            Ok(Some(evidence))
                if matches!(
                    &evidence.outcome,
                    GuardedCheckpointOutcomeV2::Created { .. }
                ) =>
            {
                Ok(ApprovalCheckpointReconcileResult::Created {
                    evidence: Self::approval_evidence(&operation, evidence)?,
                })
            }
            Ok(Some(_)) => Ok(ApprovalCheckpointReconcileResult::NotApplied),
            Ok(None) => Ok(ApprovalCheckpointReconcileResult::Unknown {
                reason: bounded_text(
                    "No exact durable checkpoint evidence exists for this approval",
                )?,
            }),
            Err(_) => Ok(ApprovalCheckpointReconcileResult::Unknown {
                reason: bounded_text("Exact approval checkpoint evidence could not be recovered")?,
            }),
        }
    }
}
