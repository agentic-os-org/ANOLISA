impl CheckpointDriver {
    pub(crate) fn admit(
        profile: GatewayCapabilityProfile,
        socket_path: PathBuf,
        registration_path: &Path,
        audit_path: PathBuf,
        owner_uid: u32,
    ) -> Result<Self, CheckpointAdmissionError> {
        cosh_gateway::capability::SealedCapabilityProviderRegistry::admit(
            profile,
            &[CapabilityProviderId::WsCkpt],
        )
        .map_err(|_| CheckpointAdmissionError::Profile)?;
        let endpoint = CheckpointEndpoint::admit(socket_path, registration_path, owner_uid)?;
        let audit_file = open_audit_file(&audit_path, owner_uid)?;
        Ok(Self {
            endpoint,
            profile,
            target: profile.governed_target(),
            audit_file,
        })
    }

    fn target_digest(&self, binding: &CheckpointBinding) -> Result<Digest, ContractError> {
        digest_parts(&[
            TARGET_DOMAIN,
            self.profile.id().as_str().as_bytes(),
            self.profile.manifest_digest().as_str().as_bytes(),
            CapabilityProviderId::WsCkpt.as_str().as_bytes(),
            self.target.kind.as_str().as_bytes(),
            self.target.authority.as_str().as_bytes(),
            self.target.identifier.as_str().as_bytes(),
            self.endpoint.socket_path.as_os_str().as_encoded_bytes(),
            &self.endpoint.socket_identity.0.to_le_bytes(),
            &self.endpoint.socket_identity.1.to_le_bytes(),
            &self.endpoint.socket_identity.2.to_le_bytes(),
            binding.ws_id.as_bytes(),
            binding.registered_path.as_bytes(),
            &binding.generation,
            &binding.owner_uid.to_le_bytes(),
        ])
    }

    fn decode_binding(
        record: &cosh_gateway::storage::BrokeredRequestRecord,
    ) -> Result<CheckpointBinding, ContractError> {
        let encoded = record
            .provider_binding
            .as_ref()
            .ok_or_else(|| checkpoint_error("checkpoint_binding_missing", false))?;
        let binding: CheckpointBinding = serde_json::from_str(encoded.as_str())
            .map_err(|_| checkpoint_error("checkpoint_binding_invalid", false))?;
        if binding.version != BINDING_VERSION {
            return Err(checkpoint_error("checkpoint_binding_invalid", false));
        }
        Ok(binding)
    }
}
impl BrokeredExecutionDriver for CheckpointDriver {
    fn plan_approval(
        &mut self,
        context: BrokeredApprovalContext<'_>,
    ) -> Result<BrokeredApprovalPlan, ContractError> {
        let BrokeredOperation::WorkspaceCheckpointCreateV1(operation) = context.operation;
        if context.request.target != self.target
            || operation.checkpoint_id.as_str().is_empty()
            || context.request.operation.name.as_str() != "checkpoint_create"
        {
            return Err(checkpoint_error("checkpoint_request_invalid", false));
        }
        let binding = self.endpoint.resolve_binding()?;
        let target_identity_digest = self.target_digest(&binding)?;
        let provider_binding = BoundedOpaque::new(
            serde_json::to_string(&binding)
                .map_err(|_| checkpoint_error("checkpoint_binding_invalid", false))?,
        )
        .map_err(|_| checkpoint_error("checkpoint_binding_invalid", false))?;
        let summary = BoundedText::new(format!(
            "Create checkpoint {} for {}",
            operation.checkpoint_id.as_str(),
            binding.registered_path
        ))
        .map_err(|_| checkpoint_error("checkpoint_summary_invalid", false))?;
        Ok(BrokeredApprovalPlan {
            approval: ApprovalRequest {
                approval_id: ApprovalId::new(),
                request_id: context.request.request_id.clone(),
                task_id: context.request.task_id.clone(),
                run_id: context.request.run_id.clone(),
                summary,
                expires_at_ms: context.request.expires_at_ms,
            },
            target_identity_digest,
            provider_binding: Some(provider_binding),
        })
    }

    fn resolve(
        &mut self,
        store: &mut SqliteTaskStore,
        context: BrokeredResolutionContext<'_>,
    ) -> Result<BrokeredResolution, ContractError> {
        let approval = ApprovalRequest {
            approval_id: context.approval.approval_id.clone(),
            request_id: context.approval.request_id.clone(),
            task_id: context.approval.task_id.clone(),
            run_id: context.approval.run_id.clone(),
            summary: BoundedText::new("Governed workspace checkpoint")
                .map_err(|_| checkpoint_error("checkpoint_internal", false))?,
            expires_at_ms: context.approval.expires_at_ms,
        };
        let permit_id = PermitId::new();
        let execution_id = ExecutionId::new();
        let resolution = DurableApprovalResolution {
            resolution_command: &ledger_command(
                &context.approval.actor_id,
                context.idempotency_key.clone(),
                "checkpoint_approval_resolution",
                &context.approval.approval_id,
                context.now_ms,
            )?,
            permit_command: &ledger_command(
                &context.approval.actor_id,
                internal_key("permit", context.approval.approval_id.as_str())?,
                "checkpoint_permit",
                &permit_id,
                context.now_ms,
            )?,
            expected_revision: context.approval.revision,
            decision: context.decision,
            policy_revision: POLICY_REVISION,
            policy_valid_until_ms: context.approval.expires_at_ms,
            permit_id,
            execution_id: execution_id.clone(),
        };
        let outcome = DurableApprovalCoordinator::new(store)
            .resolve_once(&context.request.request, &approval, resolution)
            .map_err(|_| checkpoint_error("checkpoint_approval_failed", false))?;
        let permit = match outcome {
            DurableApprovalOutcome::NotPermitted(record) => {
                return Ok(BrokeredResolution {
                    source: BrokeredResolutionSource::ApprovalDenied {
                        approval_id: record.approval_id,
                    },
                    delivery: BrokeredExecutionDelivery {
                        request_id: context.request.request.request_id.clone(),
                        outcome: BrokeredExecutionOutcome::Denied {
                            code: cosh_gateway_contracts::capability::DenialCode::ApprovalDenied,
                            safe_message: BoundedText::new("Checkpoint creation was denied")
                                .map_err(|_| checkpoint_error("checkpoint_internal", false))?,
                        },
                    },
                });
            }
            DurableApprovalOutcome::Permit(record) => record.permit,
        };

        let binding = Self::decode_binding(context.request)?;
        let current = self.endpoint.resolve_binding()?;
        if current.ws_id != binding.ws_id
            || current.registered_path != binding.registered_path
            || current.generation != binding.generation
            || current.owner_uid != binding.owner_uid
            || self.target_digest(&current)? != context.request.target_identity_digest
        {
            return Err(checkpoint_error("checkpoint_binding_changed", false));
        }
        let operation = CheckpointOperation::new(&permit, context.request, binding)?;
        let claim = ExecutionClaim {
            permit_id: permit.permit_id.clone(),
            execution_id: permit.execution_id.clone(),
            task_id: permit.task_id.clone(),
            run_id: permit.run_id.clone(),
            target: permit.target.clone(),
            target_identity_digest: permit.target_identity_digest.clone(),
            runtime_fence: permit.runtime_fence.clone(),
            operation_digest: permit.operation_digest.clone(),
            input_digest: permit.input_digest.clone(),
            policy_revision: permit.policy_revision,
            lease: context.lease.clone(),
        };
        let actor = context.approval.actor_id.clone();
        let mut target = CheckpointTarget {
            client: &self.endpoint.client,
        };
        let mut audit = FileAuditGate::new(&mut self.audit_file);
        let start_key = internal_key("start", execution_id.as_str())?;
        let terminal_key = internal_key("terminal", execution_id.as_str())?;
        let executed = GovernedExecutionCoordinator::new(store).execute(
            &ledger_command(
                &actor,
                internal_key("claim", execution_id.as_str())?,
                "checkpoint_claim",
                &execution_id,
                context.now_ms,
            )?,
            |proof| {
                ledger_command(
                    &actor,
                    start_key,
                    "checkpoint_start",
                    &execution_id,
                    proof.persisted_at_ms,
                )
                .map_err(|error| GovernedExecutionError::CommandBuild {
                    execution_id: execution_id.clone(),
                    stage: cosh_gateway::capability::ExecutionCommandBuildStage::Start,
                    message: error.safe_message,
                })
            },
            || {
                ledger_command(
                    &actor,
                    terminal_key,
                    "checkpoint_terminal",
                    &execution_id,
                    current_time_ms().unwrap_or(context.now_ms),
                )
                .map_err(|error| GovernedExecutionError::CommandBuild {
                    execution_id: execution_id.clone(),
                    stage: cosh_gateway::capability::ExecutionCommandBuildStage::Terminal,
                    message: error.safe_message,
                })
            },
            &claim,
            &operation,
            &mut target,
            &mut audit,
        );
        let durable_outcome = match executed {
            Ok(result) if result.succeeded => BrokeredExecutionOutcome::Succeeded {
                execution_id: execution_id.clone(),
                result: result
                    .typed_result
                    .ok_or_else(|| checkpoint_error("checkpoint_result_missing", false))?,
            },
            Ok(_) => BrokeredExecutionOutcome::Failed {
                execution_id: execution_id.clone(),
                error: checkpoint_error("checkpoint_create_failed", false),
            },
            Err(
                GovernedExecutionError::OutcomeUnknown { .. }
                | GovernedExecutionError::CompletionUnknown { .. },
            ) => BrokeredExecutionOutcome::Uncertain {
                execution_id: execution_id.clone(),
                error: checkpoint_error("checkpoint_result_uncertain", false),
            },
            Err(_) => BrokeredExecutionOutcome::Failed {
                execution_id: execution_id.clone(),
                error: checkpoint_error("checkpoint_execution_failed", false),
            },
        };
        Ok(BrokeredResolution {
            source: BrokeredResolutionSource::Execution { execution_id },
            delivery: BrokeredExecutionDelivery {
                request_id: context.request.request.request_id.clone(),
                outcome: durable_outcome,
            },
        })
    }

    fn reconcile_started(
        &mut self,
        context: BrokeredRecoveryContext<'_>,
    ) -> ExecutionTargetOutcome {
        let binding = match Self::decode_binding(context.request) {
            Ok(binding) => binding,
            Err(error) => {
                return ExecutionTargetOutcome::Unknown {
                    safe_detail: Some(error.safe_message),
                }
            }
        };
        let expected_target = match self.target_digest(&binding) {
            Ok(digest) => digest,
            Err(error) => {
                return ExecutionTargetOutcome::Unknown {
                    safe_detail: Some(error.safe_message),
                }
            }
        };
        if binding.owner_uid != self.endpoint.owner_uid
            || context.execution.target_identity_digest.as_ref() != Some(&expected_target)
            || context.request.target_identity_digest != expected_target
        {
            return ExecutionTargetOutcome::Unknown {
                safe_detail: bounded("Checkpoint recovery binding did not match admission"),
            };
        }
        let operation =
            match CheckpointOperation::for_recovery(context.execution, context.request, binding) {
                Ok(operation) => operation,
                Err(error) => {
                    return ExecutionTargetOutcome::Unknown {
                        safe_detail: Some(error.safe_message),
                    }
                }
            };
        let operation_digest = match digest_bytes(&operation.operation_digest) {
            Ok(value) => value,
            Err(error) => {
                return ExecutionTargetOutcome::Unknown {
                    safe_detail: Some(error.safe_message),
                }
            }
        };
        let generation = WorkspaceGenerationTokenV2::from_bytes(operation.binding.generation);
        match self.endpoint.client.checkpoint_evidence_v2(
            &operation.binding.ws_id,
            generation,
            operation.checkpoint_id.as_str(),
            operation_digest,
        ) {
            Ok(Some(evidence)) => evidence_outcome(&operation, evidence),
            Ok(None) | Err(_) => ExecutionTargetOutcome::Unknown {
                safe_detail: bounded("Checkpoint recovery found no exact durable evidence"),
            },
        }
    }
}

struct CheckpointOperation {
    target: TargetRef,
    target_identity_digest: Digest,
    runtime_fence: RuntimeExecutionFence,
    operation_digest: Digest,
    input_digest: Digest,
    checkpoint_id: CheckpointId,
    binding: CheckpointBinding,
}

impl CheckpointOperation {
    fn new(
        permit: &cosh_gateway_contracts::capability::ExecutionPermit,
        request: &cosh_gateway::storage::BrokeredRequestRecord,
        binding: CheckpointBinding,
    ) -> Result<Self, ContractError> {
        let BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
            checkpoint_id,
        }) = &request.operation;
        Ok(Self {
            target: permit.target.clone(),
            target_identity_digest: permit.target_identity_digest.clone(),
            runtime_fence: permit.runtime_fence.clone(),
            operation_digest: permit.operation_digest.clone(),
            input_digest: permit.input_digest.clone(),
            checkpoint_id: checkpoint_id.clone(),
            binding,
        })
    }

    fn for_recovery(
        execution: &ExecutionRecord,
        request: &cosh_gateway::storage::BrokeredRequestRecord,
        binding: CheckpointBinding,
    ) -> Result<Self, ContractError> {
        let BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
            checkpoint_id,
        }) = &request.operation;
        Ok(Self {
            target: execution.target.clone(),
            target_identity_digest: execution
                .target_identity_digest
                .clone()
                .ok_or_else(|| checkpoint_error("checkpoint_binding_missing", false))?,
            runtime_fence: execution
                .runtime_fence
                .clone()
                .ok_or_else(|| checkpoint_error("checkpoint_binding_missing", false))?,
            operation_digest: execution.operation_digest.clone(),
            input_digest: execution.input_digest.clone(),
            checkpoint_id: checkpoint_id.clone(),
            binding,
        })
    }
}

impl BoundExecutionOperation for CheckpointOperation {
    fn target(&self) -> &TargetRef {
        &self.target
    }
    fn target_identity_digest(&self) -> &Digest {
        &self.target_identity_digest
    }
    fn runtime_fence(&self) -> &RuntimeExecutionFence {
        &self.runtime_fence
    }
    fn operation_digest(&self) -> &Digest {
        &self.operation_digest
    }
    fn input_digest(&self) -> &Digest {
        &self.input_digest
    }
}

struct CheckpointTarget<'a> {
    client: &'a CkptClient,
}

impl ExecutionTarget<CheckpointOperation> for CheckpointTarget<'_> {
    fn execute(&mut self, operation: &CheckpointOperation) -> ExecutionTargetOutcome {
        let operation_digest = match digest_bytes(&operation.operation_digest) {
            Ok(value) => value,
            Err(error) => return known_failure(operation, error.safe_message.as_str()),
        };
        let generation = WorkspaceGenerationTokenV2::from_bytes(operation.binding.generation);
        match self.client.guarded_create_v2(
            &operation.binding.ws_id,
            generation,
            operation.checkpoint_id.as_str(),
            operation_digest,
            Some("COSH governed Task checkpoint"),
            None,
            false,
        ) {
            Ok(evidence) => evidence_outcome(operation, evidence),
            Err(failure) if failure.effect == CkptRequestEffect::KnownNoEffect => known_failure(
                operation,
                "Checkpoint daemon rejected the operation before backend execution",
            ),
            Err(_) => match self.client.checkpoint_evidence_v2(
                &operation.binding.ws_id,
                generation,
                operation.checkpoint_id.as_str(),
                operation_digest,
            ) {
                Ok(Some(evidence)) => evidence_outcome(operation, evidence),
                Ok(None) | Err(_) => ExecutionTargetOutcome::Unknown {
                    safe_detail: bounded(
                        "Checkpoint outcome could not be proven from exact evidence",
                    ),
                },
            },
        }
    }
}
