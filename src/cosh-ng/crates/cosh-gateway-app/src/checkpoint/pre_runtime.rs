impl PreRuntimeCheckpointAdapter {
    pub(crate) fn admit(
        socket_path: PathBuf,
        registration_path: &Path,
        workspace: WorkspaceRef,
        owner_uid: u32,
    ) -> Result<Self, CheckpointAdmissionError> {
        cosh_gateway::capability::SealedCapabilityProviderRegistry::admit(
            GatewayCapabilityProfile::workspace_checkpoint_v1(),
            &[CapabilityProviderId::WsCkpt],
        )
        .map_err(|_| CheckpointAdmissionError::Profile)?;
        Ok(Self {
            endpoint: CheckpointEndpoint::admit(socket_path, registration_path, owner_uid)?,
            workspace,
        })
    }

    fn prepare_operation(
        &self,
        request: &PreRuntimeCheckpointRequest,
    ) -> Result<PreRuntimeCheckpointOperation, ContractError> {
        if request.workspace != self.workspace {
            return Err(pre_runtime_checkpoint_error(
                "checkpoint_workspace_mismatch",
                ErrorCategory::InvalidRequest,
                false,
                "The checkpoint workspace does not match Gateway admission",
            ));
        }
        let binding = self.endpoint.resolve_binding()?;
        let digest = digest_parts(&[
            PRE_RUNTIME_OPERATION_DOMAIN,
            request.baseline_id.as_str().as_bytes(),
            request.task_id.as_str().as_bytes(),
            request.run_id.as_str().as_bytes(),
            request.workspace.scope_digest.as_str().as_bytes(),
            binding.ws_id.as_bytes(),
            binding.registered_path.as_bytes(),
            &binding.generation,
            &binding.owner_uid.to_le_bytes(),
        ])?;
        Ok(PreRuntimeCheckpointOperation {
            baseline_id: request.baseline_id.clone(),
            binding,
            digest,
        })
    }

    fn operation_from_binding(
        &self,
        request: &PreRuntimeCheckpointRequest,
        durable: &PreRuntimeCheckpointBinding,
    ) -> Result<PreRuntimeCheckpointOperation, ContractError> {
        if request.workspace != self.workspace {
            return Err(pre_runtime_checkpoint_error(
                "checkpoint_workspace_mismatch",
                ErrorCategory::InvalidRequest,
                false,
                "The checkpoint workspace does not match Gateway admission",
            ));
        }
        self.endpoint.verify_socket_unchanged()?;
        let binding = CheckpointBinding {
            version: BINDING_VERSION,
            ws_id: durable.provider_workspace_id.as_str().to_owned(),
            registered_path: self.endpoint.registration_path.clone(),
            generation: generation_bytes(&durable.provider_generation)?,
            owner_uid: self.endpoint.owner_uid,
        };
        let digest = digest_parts(&[
            PRE_RUNTIME_OPERATION_DOMAIN,
            request.baseline_id.as_str().as_bytes(),
            request.task_id.as_str().as_bytes(),
            request.run_id.as_str().as_bytes(),
            request.workspace.scope_digest.as_str().as_bytes(),
            binding.ws_id.as_bytes(),
            binding.registered_path.as_bytes(),
            &binding.generation,
            &binding.owner_uid.to_le_bytes(),
        ])?;
        if digest != durable.operation_digest {
            return Err(pre_runtime_checkpoint_error(
                "checkpoint_binding_mismatch",
                ErrorCategory::InvalidRequest,
                false,
                "The durable checkpoint binding does not match this Task",
            ));
        }
        Ok(PreRuntimeCheckpointOperation {
            baseline_id: request.baseline_id.clone(),
            binding,
            digest,
        })
    }

    fn evidence(
        operation: &PreRuntimeCheckpointOperation,
        evidence: GuardedCheckpointEvidenceV2,
    ) -> Result<PreRuntimeCheckpointEvidence, ContractError> {
        if evidence.ws_id != operation.binding.ws_id
            || evidence.generation.into_bytes() != operation.binding.generation
            || evidence.checkpoint_id != operation.baseline_id.as_str()
            || evidence.operation_digest != digest_bytes(&operation.digest)?
            || evidence.caller_uid != operation.binding.owner_uid
            || evidence.registered_path != operation.binding.registered_path
        {
            return Err(pre_runtime_checkpoint_error(
                "checkpoint_evidence_mismatch",
                ErrorCategory::Internal,
                false,
                "Checkpoint evidence did not match the admitted workspace",
            ));
        }
        let encoded = serde_json::to_vec(&evidence).map_err(|_| {
            pre_runtime_checkpoint_error(
                "checkpoint_evidence_invalid",
                ErrorCategory::Internal,
                false,
                "Checkpoint evidence could not be encoded safely",
            )
        })?;
        let provider_generation = BoundedOpaque::new(hex_bytes(&operation.binding.generation))
            .map_err(|_| {
                pre_runtime_checkpoint_error(
                    "checkpoint_generation_invalid",
                    ErrorCategory::Internal,
                    false,
                    "Checkpoint generation evidence was invalid",
                )
            })?;
        Ok(PreRuntimeCheckpointEvidence {
            baseline_id: operation.baseline_id.clone(),
            provider_generation,
            evidence_digest: digest_parts(&[PRE_RUNTIME_EVIDENCE_DOMAIN, &encoded])?,
        })
    }

    fn prepare_approval_operation(
        &self,
        request: &ApprovalCheckpointRequest,
    ) -> Result<ApprovalCheckpointOperation, ContractError> {
        if request.workspace != self.workspace {
            return Err(pre_runtime_checkpoint_error(
                "checkpoint_workspace_mismatch",
                ErrorCategory::InvalidRequest,
                false,
                "The checkpoint workspace does not match Gateway admission",
            ));
        }
        let binding = self.endpoint.resolve_binding()?;
        let fence = serde_json::to_vec(&request.runtime_fence)
            .map_err(|_| checkpoint_error("checkpoint_runtime_fence_invalid", false))?;
        let digest = digest_parts(&[
            APPROVAL_OPERATION_DOMAIN,
            request.checkpoint_id.as_str().as_bytes(),
            request.approval_id.as_str().as_bytes(),
            request.task_id.as_str().as_bytes(),
            request.run_id.as_str().as_bytes(),
            request.workspace.scope_digest.as_str().as_bytes(),
            &fence,
            binding.ws_id.as_bytes(),
            binding.registered_path.as_bytes(),
            &binding.generation,
            &binding.owner_uid.to_le_bytes(),
        ])?;
        Ok(ApprovalCheckpointOperation {
            checkpoint_id: request.checkpoint_id.clone(),
            binding,
            digest,
        })
    }

    fn approval_operation_from_binding(
        &self,
        request: &ApprovalCheckpointRequest,
        durable: &PreRuntimeCheckpointBinding,
    ) -> Result<ApprovalCheckpointOperation, ContractError> {
        if request.workspace != self.workspace {
            return Err(checkpoint_error("checkpoint_workspace_mismatch", false));
        }
        self.endpoint.verify_socket_unchanged()?;
        let binding = CheckpointBinding {
            version: BINDING_VERSION,
            ws_id: durable.provider_workspace_id.as_str().to_owned(),
            registered_path: self.endpoint.registration_path.clone(),
            generation: generation_bytes(&durable.provider_generation)?,
            owner_uid: self.endpoint.owner_uid,
        };
        let fence = serde_json::to_vec(&request.runtime_fence)
            .map_err(|_| checkpoint_error("checkpoint_runtime_fence_invalid", false))?;
        let digest = digest_parts(&[
            APPROVAL_OPERATION_DOMAIN,
            request.checkpoint_id.as_str().as_bytes(),
            request.approval_id.as_str().as_bytes(),
            request.task_id.as_str().as_bytes(),
            request.run_id.as_str().as_bytes(),
            request.workspace.scope_digest.as_str().as_bytes(),
            &fence,
            binding.ws_id.as_bytes(),
            binding.registered_path.as_bytes(),
            &binding.generation,
            &binding.owner_uid.to_le_bytes(),
        ])?;
        if digest != durable.operation_digest {
            return Err(checkpoint_error("checkpoint_binding_mismatch", false));
        }
        Ok(ApprovalCheckpointOperation {
            checkpoint_id: request.checkpoint_id.clone(),
            binding,
            digest,
        })
    }

    fn approval_evidence(
        operation: &ApprovalCheckpointOperation,
        evidence: GuardedCheckpointEvidenceV2,
    ) -> Result<ApprovalCheckpointEvidence, ContractError> {
        if evidence.ws_id != operation.binding.ws_id
            || evidence.generation.into_bytes() != operation.binding.generation
            || evidence.checkpoint_id != operation.checkpoint_id.as_str()
            || evidence.operation_digest != digest_bytes(&operation.digest)?
            || evidence.caller_uid != operation.binding.owner_uid
            || evidence.registered_path != operation.binding.registered_path
        {
            return Err(checkpoint_error("checkpoint_evidence_mismatch", false));
        }
        let encoded = serde_json::to_vec(&evidence)
            .map_err(|_| checkpoint_error("checkpoint_evidence_invalid", false))?;
        Ok(ApprovalCheckpointEvidence {
            checkpoint_id: operation.checkpoint_id.clone(),
            provider_generation: BoundedOpaque::new(hex_bytes(&operation.binding.generation))
                .map_err(|_| checkpoint_error("checkpoint_generation_invalid", false))?,
            evidence_digest: digest_parts(&[APPROVAL_EVIDENCE_DOMAIN, &encoded])?,
        })
    }
}
