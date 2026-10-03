use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use cosh_gateway_contracts::capability::{
    BrokeredOperation, CapabilityScope, OperationDescriptor, WorkspaceCheckpointCreateV1,
};
use cosh_gateway_contracts::common::{
    BoundedName, BoundedOpaque, BoundedText, RuntimeSelector, TargetRef,
};
use cosh_gateway_contracts::external::{ExternalRef, ExternalRefKind};
use cosh_gateway_contracts::ids::{
    AgentSessionId, CheckpointId, ExecutionId, InstallationId, PermitId, RequestId,
    RuntimeBindingId, RuntimeInstanceId, TurnId,
};
use cosh_gateway_contracts::runtime::{
    BrokeredExecutionOutcome, BrokeredOperationResult, RuntimePermissionDecision,
    RuntimePermissionRef, WorkspaceCheckpointCreateV1Outcome, WorkspaceCheckpointCreateV1Result,
};
use tempfile::TempDir;

use super::*;
use crate::daemon::{actor_id_for_uid, now_ms, SubmitTask};
use crate::{
    capability::{
        BoundExecutionOperation, DurableApprovalOutcome, DurableApprovalResolution,
        ExecutionTarget, ExecutionTargetOutcome, GovernedExecutionCoordinator,
        GovernedExecutionError, GovernedExecutionResult, SecurityAuditError, SecurityAuditGate,
    },
    storage::{ExecutionClaim, ExecutionState, SecurityAuditProof, SqliteTaskStore, StoreError},
};

const LOGICAL_CLOCK_HEADROOM_MS: u64 = 60_000;

#[derive(Default)]
struct RuntimeWrites {
    acknowledgements: Vec<BrokeredRequestAcknowledgement>,
    results: Vec<BrokeredExecutionDelivery>,
}

struct BrokeredFactory {
    writes: Arc<Mutex<RuntimeWrites>>,
    expires_at_ms: u64,
    fail_acknowledgement: bool,
    fail_result: bool,
}

impl RuntimeFactory for BrokeredFactory {
    fn open(&mut self, run: &ScheduledRun) -> Result<StartedRuntime, ContractError> {
        let binding = RuntimeBindingRef {
            binding_id: RuntimeBindingId::new(),
            task_id: run.task_id.clone(),
            run_id: run.run_id.clone(),
            agent_session_id: AgentSessionId::new(),
            runtime_instance_id: RuntimeInstanceId::new(),
            runtime_generation: run.lease_generation,
            external_session: ExternalRef {
                kind: ExternalRefKind::AcpSession,
                authority: BoundedName::new("brokered-scheduler-test").unwrap(),
                scope_digest: test_digest(),
                value: BoundedOpaque::new("session-hash").unwrap(),
            },
        };
        let operation =
            BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
                checkpoint_id: CheckpointId::new(),
            });
        let request = CapabilityRequest {
            request_id: RequestId::new(),
            task_id: run.task_id.clone(),
            run_id: run.run_id.clone(),
            actor: run.actor.clone(),
            target: run.target.clone(),
            operation: OperationDescriptor {
                namespace: BoundedName::new("workspace").unwrap(),
                name: BoundedName::new("checkpoint_create").unwrap(),
                arguments_digest: test_digest(),
            },
            operation_digest: test_digest(),
            requested_scope: CapabilityScope {
                resource: BoundedName::new("workspace").unwrap(),
                access: BoundedName::new("checkpoint").unwrap(),
            },
            input_digest: test_digest(),
            expires_at_ms: self.expires_at_ms,
        };
        let brokered = BrokeredExecutionRef {
            binding_id: binding.binding_id.clone(),
            runtime_generation: binding.runtime_generation,
            event_sequence: 2,
            run_id: run.run_id.clone(),
            turn_id: TurnId::new(),
            tool_use_id: None,
            request_id: request.request_id.clone(),
            operation: operation.clone(),
        };
        Ok(StartedRuntime {
            binding,
            handle: Box::new(BrokeredHandle {
                brokered,
                request,
                operation,
                emitted: false,
                writes: Arc::clone(&self.writes),
                fail_acknowledgement: self.fail_acknowledgement,
                fail_result: self.fail_result,
            }),
        })
    }
}

struct BrokeredHandle {
    brokered: BrokeredExecutionRef,
    request: CapabilityRequest,
    operation: BrokeredOperation,
    emitted: bool,
    writes: Arc<Mutex<RuntimeWrites>>,
    fail_acknowledgement: bool,
    fail_result: bool,
}

impl RuntimeHandle for BrokeredHandle {
    fn begin(&mut self) -> Result<(), ContractError> {
        Ok(())
    }

    fn poll(&mut self) -> RuntimePoll {
        if self.emitted {
            RuntimePoll::Pending
        } else {
            self.emitted = true;
            RuntimePoll::BrokeredExecutionRequested {
                brokered: self.brokered.clone(),
                request: Box::new(self.request.clone()),
                operation: self.operation.clone(),
                summary: ToolSummary {
                    name: BoundedName::new("workspace_checkpoint_create").unwrap(),
                    summary: BoundedText::new("Create a governed workspace checkpoint").unwrap(),
                },
            }
        }
    }

    fn shutdown(&mut self, _reason: CancelReason) -> Result<(), ContractError> {
        Ok(())
    }

    fn resolve_provider_permission(
        &mut self,
        _permission: &RuntimePermissionRef,
        _decision: RuntimePermissionDecision,
    ) -> Result<(), ContractError> {
        Err(runtime_handle_unsupported("provider permission"))
    }

    fn acknowledge_brokered_request(
        &mut self,
        brokered: &BrokeredExecutionRef,
        acknowledgement: BrokeredRequestAcknowledgement,
    ) -> Result<(), ContractError> {
        assert_eq!(brokered, &self.brokered);
        self.writes
            .lock()
            .map_err(|error| test_contract_error(format!("permit issuance failed: {error}")))?
            .acknowledgements
            .push(acknowledgement);
        if self.fail_acknowledgement {
            Err(test_contract_error("acknowledgement transport failed"))
        } else {
            Ok(())
        }
    }

    fn deliver_brokered_result(
        &mut self,
        brokered: &BrokeredExecutionRef,
        delivery: BrokeredExecutionDelivery,
    ) -> Result<(), ContractError> {
        assert_eq!(brokered, &self.brokered);
        self.writes.lock().unwrap().results.push(delivery);
        if self.fail_result {
            Err(test_contract_error("result transport failed"))
        } else {
            Ok(())
        }
    }
}

struct DenyingDriver {
    resolutions: Arc<Mutex<Vec<(u64, u64)>>>,
}

impl BrokeredExecutionDriver for DenyingDriver {
    fn plan_approval(
        &mut self,
        context: BrokeredApprovalContext<'_>,
    ) -> Result<BrokeredApprovalPlan, ContractError> {
        Ok(BrokeredApprovalPlan {
            approval: ApprovalRequest {
                approval_id: ApprovalId::new(),
                request_id: context.request.request_id.clone(),
                task_id: context.request.task_id.clone(),
                run_id: context.request.run_id.clone(),
                summary: BoundedText::new("Policy-authorized checkpoint approval").unwrap(),
                expires_at_ms: context.request.expires_at_ms,
            },
            target_identity_digest: test_digest(),
            provider_binding: None,
        })
    }

    fn resolve(
        &mut self,
        store: &mut SqliteTaskStore,
        context: BrokeredResolutionContext<'_>,
    ) -> Result<BrokeredResolution, ContractError> {
        assert_eq!(context.decision, ApprovalDecision::Deny);
        self.resolutions
            .lock()
            .unwrap()
            .push((context.lease.generation, context.lease.revision));
        let command = LedgerCommand {
            actor_id: context.approval.actor_id.clone(),
            idempotency_key: context.idempotency_key.clone(),
            command_digest: digest_json(&(
                "deny_brokered_test",
                &context.approval.approval_id,
                context.approval.revision,
            ))
            .map_err(|_| test_contract_error("could not digest denial command"))?,
            committed_at_ms: context.now_ms,
        };
        let resolved = store
            .resolve_approval(
                &command,
                &context.approval.approval_id,
                context.approval.revision,
                crate::storage::ApprovalResolution::Decide(ApprovalDecision::Deny),
            )
            .map_err(|_| test_contract_error("storage denied test approval"))?;
        let record = match resolved {
            LedgerOutcome::Applied(record) | LedgerOutcome::Replayed(record) => record,
        };
        assert_eq!(record.state, ApprovalState::Denied);
        Ok(BrokeredResolution {
            source: BrokeredResolutionSource::ApprovalDenied {
                approval_id: context.approval.approval_id.clone(),
            },
            delivery: BrokeredExecutionDelivery {
                request_id: context.request.request.request_id.clone(),
                outcome: BrokeredExecutionOutcome::Denied {
                    code: DenialCode::ApprovalDenied,
                    safe_message: BoundedText::new("The brokered operation was denied").unwrap(),
                },
            },
        })
    }
}

struct UncertainDriver {
    target_calls: Arc<Mutex<usize>>,
}

#[derive(Clone, Copy)]
enum ConclusiveDriverMode {
    CompletionUnknownAfterCommit,
    WrongSuccessBeforePrepare,
    FabricatedSuccessAfterFailure,
}

struct ConclusiveDriver {
    target_calls: Arc<Mutex<usize>>,
    resolve_calls: Arc<Mutex<usize>>,
    mode: ConclusiveDriverMode,
}

impl BrokeredExecutionDriver for ConclusiveDriver {
    fn plan_approval(
        &mut self,
        context: BrokeredApprovalContext<'_>,
    ) -> Result<BrokeredApprovalPlan, ContractError> {
        Ok(BrokeredApprovalPlan {
            approval: ApprovalRequest {
                approval_id: ApprovalId::new(),
                request_id: context.request.request_id.clone(),
                task_id: context.request.task_id.clone(),
                run_id: context.request.run_id.clone(),
                summary: context.summary.summary.clone(),
                expires_at_ms: context.request.expires_at_ms,
            },
            target_identity_digest: test_digest(),
            provider_binding: None,
        })
    }

    fn resolve(
        &mut self,
        store: &mut SqliteTaskStore,
        context: BrokeredResolutionContext<'_>,
    ) -> Result<BrokeredResolution, ContractError> {
        assert_eq!(context.decision, ApprovalDecision::Approve);
        *self.resolve_calls.lock().unwrap() += 1;
        let (permit_id, execution_id) =
            stable_test_execution_ids(&context.request.request.request_id)?;
        let resolution_command = conclusive_driver_command(
            &context.approval.actor_id,
            "resolve",
            &context.approval.approval_id,
            context.now_ms,
        )?;
        let permit_command = conclusive_driver_command(
            &context.approval.actor_id,
            "permit",
            &context.approval.approval_id,
            context.now_ms,
        )?;
        let expected_revision = match context.approval.state {
            ApprovalState::Pending => context.approval.revision,
            ApprovalState::Approved => context.approval.revision.checked_sub(1).unwrap(),
            other => panic!("unexpected approval state: {other:?}"),
        };
        let approval = ApprovalRequest {
            approval_id: context.approval.approval_id.clone(),
            request_id: context.approval.request_id.clone(),
            task_id: context.approval.task_id.clone(),
            run_id: context.approval.run_id.clone(),
            summary: BoundedText::new("Create a governed workspace checkpoint").unwrap(),
            expires_at_ms: context.approval.expires_at_ms,
        };
        let permit = match DurableApprovalCoordinator::new(store)
            .resolve_once(
                &context.request.request,
                &approval,
                DurableApprovalResolution {
                    resolution_command: &resolution_command,
                    permit_command: &permit_command,
                    expected_revision,
                    decision: ApprovalDecision::Approve,
                    policy_revision: 1,
                    policy_valid_until_ms: context.request.request.expires_at_ms,
                    permit_id,
                    execution_id: execution_id.clone(),
                },
            )
            .unwrap_or_else(|error| panic!("approval resolution failed: {error:?}"))
        {
            DurableApprovalOutcome::Permit(permit) => permit,
            DurableApprovalOutcome::NotPermitted(_) => {
                return Err(test_contract_error("conclusive test approval was denied"));
            }
        };
        let authority = permit.permit;
        let operation = TestBoundOperation {
            target: authority.target.clone(),
            target_identity_digest: authority.target_identity_digest.clone(),
            runtime_fence: authority.runtime_fence.clone(),
            operation_digest: authority.operation_digest.clone(),
            input_digest: authority.input_digest.clone(),
        };
        let claim = ExecutionClaim {
            permit_id: authority.permit_id,
            execution_id: execution_id.clone(),
            task_id: authority.task_id,
            run_id: authority.run_id,
            target: authority.target,
            target_identity_digest: authority.target_identity_digest,
            runtime_fence: authority.runtime_fence,
            operation_digest: authority.operation_digest,
            input_digest: authority.input_digest,
            policy_revision: authority.policy_revision,
            lease: context.lease.clone(),
        };
        let succeeded = !matches!(
            self.mode,
            ConclusiveDriverMode::FabricatedSuccessAfterFailure
        );
        let typed_result = succeeded.then(|| durable_success_result(&context.request.operation));
        let mut target = ConclusiveTarget {
            calls: Arc::clone(&self.target_calls),
            outcome: ExecutionTargetOutcome::Conclusive {
                succeeded,
                receipt_digest: test_digest(),
                safe_detail: None,
                typed_result,
            },
        };
        let mut audit = TestAudit {
            persisted_at_ms: context.now_ms,
        };
        let actor_id = context.approval.actor_id.clone();
        let approval_id = context.approval.approval_id.clone();
        let execution = GovernedExecutionCoordinator::new(store).execute(
            &conclusive_driver_command(&actor_id, "claim", &approval_id, context.now_ms)?,
            |_| {
                conclusive_driver_command(&actor_id, "start", &approval_id, context.now_ms).map_err(
                    |error| GovernedExecutionError::CommandBuild {
                        execution_id: execution_id.clone(),
                        stage: crate::capability::ExecutionCommandBuildStage::Start,
                        message: error.safe_message,
                    },
                )
            },
            || {
                conclusive_driver_command(&actor_id, "terminal", &approval_id, context.now_ms)
                    .map_err(|error| GovernedExecutionError::CommandBuild {
                        execution_id: execution_id.clone(),
                        stage: crate::capability::ExecutionCommandBuildStage::Terminal,
                        message: error.safe_message,
                    })
            },
            &claim,
            &operation,
            &mut target,
            &mut audit,
        );
        if *self.resolve_calls.lock().unwrap() == 1 {
            assert!(
                execution.is_ok(),
                "unexpected conclusive execution: {execution:?}"
            );
        }
        let mapped_execution = if matches!(
            self.mode,
            ConclusiveDriverMode::CompletionUnknownAfterCommit
        ) && execution.is_ok()
        {
            Err(GovernedExecutionError::CompletionUnknown {
                execution_id: execution_id.clone(),
                source: StoreError::LedgerConflict {
                    message: "simulated response loss after commit".to_owned(),
                },
            })
        } else {
            execution
        };
        let mut resolution =
            map_test_execution(store, context, execution_id.clone(), mapped_execution)
                .unwrap_or_else(|error| panic!("conclusive execution mapping failed: {error:?}"));
        match self.mode {
            ConclusiveDriverMode::WrongSuccessBeforePrepare
                if *self.resolve_calls.lock().unwrap() == 1 =>
            {
                resolution.delivery.outcome = BrokeredExecutionOutcome::Succeeded {
                    execution_id,
                    result: fabricated_success_result(),
                };
            }
            ConclusiveDriverMode::FabricatedSuccessAfterFailure => {
                resolution.delivery.outcome = BrokeredExecutionOutcome::Succeeded {
                    execution_id,
                    result: fabricated_success_result(),
                };
            }
            _ => {}
        }
        Ok(resolution)
    }
}

impl BrokeredExecutionDriver for UncertainDriver {
    fn plan_approval(
        &mut self,
        context: BrokeredApprovalContext<'_>,
    ) -> Result<BrokeredApprovalPlan, ContractError> {
        Ok(BrokeredApprovalPlan {
            approval: ApprovalRequest {
                approval_id: ApprovalId::new(),
                request_id: context.request.request_id.clone(),
                task_id: context.request.task_id.clone(),
                run_id: context.request.run_id.clone(),
                summary: context.summary.summary.clone(),
                expires_at_ms: context.request.expires_at_ms,
            },
            target_identity_digest: test_digest(),
            provider_binding: None,
        })
    }

    fn resolve(
        &mut self,
        store: &mut SqliteTaskStore,
        context: BrokeredResolutionContext<'_>,
    ) -> Result<BrokeredResolution, ContractError> {
        assert_eq!(context.decision, ApprovalDecision::Approve);
        let permit_id = PermitId::new();
        let execution_id = ExecutionId::new();
        let resolution_command = driver_command(
            &context.approval.actor_id,
            "resolve",
            &context.approval.approval_id,
            context.now_ms,
        )?;
        let permit_command = driver_command(
            &context.approval.actor_id,
            "permit",
            &context.approval.approval_id,
            context.now_ms,
        )?;
        let approval = ApprovalRequest {
            approval_id: context.approval.approval_id.clone(),
            request_id: context.approval.request_id.clone(),
            task_id: context.approval.task_id.clone(),
            run_id: context.approval.run_id.clone(),
            summary: BoundedText::new("Create a governed workspace checkpoint").unwrap(),
            expires_at_ms: context.approval.expires_at_ms,
        };
        let permit = match DurableApprovalCoordinator::new(store)
            .resolve_once(
                &context.request.request,
                &approval,
                DurableApprovalResolution {
                    resolution_command: &resolution_command,
                    permit_command: &permit_command,
                    expected_revision: context.approval.revision,
                    decision: ApprovalDecision::Approve,
                    policy_revision: 1,
                    policy_valid_until_ms: context.request.request.expires_at_ms,
                    permit_id,
                    execution_id: execution_id.clone(),
                },
            )
            .unwrap()
        {
            DurableApprovalOutcome::Permit(permit) => permit,
            DurableApprovalOutcome::NotPermitted(_) => {
                return Err(test_contract_error("uncertain test approval was denied"))
            }
        };
        let authority = permit.permit;
        let operation = TestBoundOperation {
            target: authority.target.clone(),
            target_identity_digest: authority.target_identity_digest.clone(),
            runtime_fence: authority.runtime_fence.clone(),
            operation_digest: authority.operation_digest.clone(),
            input_digest: authority.input_digest.clone(),
        };
        let claim = ExecutionClaim {
            permit_id: authority.permit_id,
            execution_id: execution_id.clone(),
            task_id: authority.task_id,
            run_id: authority.run_id,
            target: authority.target,
            target_identity_digest: authority.target_identity_digest,
            runtime_fence: authority.runtime_fence,
            operation_digest: authority.operation_digest,
            input_digest: authority.input_digest,
            policy_revision: authority.policy_revision,
            lease: context.lease.clone(),
        };
        let claim_command = driver_command(
            &context.approval.actor_id,
            "claim",
            &context.approval.approval_id,
            context.now_ms + 1,
        )?;
        let actor_id = context.approval.actor_id.clone();
        let approval_id = context.approval.approval_id.clone();
        let mut target = UnknownTarget {
            calls: Arc::clone(&self.target_calls),
        };
        let mut audit = TestAudit {
            persisted_at_ms: context.now_ms + 2,
        };
        let execution = GovernedExecutionCoordinator::new(store).execute(
            &claim_command,
            |_| Ok(driver_command(&actor_id, "start", &approval_id, context.now_ms + 2).unwrap()),
            || {
                Ok(
                    driver_command(&actor_id, "uncertain", &approval_id, context.now_ms + 3)
                        .unwrap(),
                )
            },
            &claim,
            &operation,
            &mut target,
            &mut audit,
        );
        assert!(matches!(
            execution,
            Err(GovernedExecutionError::OutcomeUnknown { .. })
        ));
        assert_eq!(
            store
                .load_execution_record(&execution_id)
                .map_err(|_| test_contract_error("uncertain execution was not durable"))?
                .state,
            crate::storage::ExecutionState::Uncertain
        );
        Ok(BrokeredResolution {
            source: BrokeredResolutionSource::Execution {
                execution_id: execution_id.clone(),
            },
            delivery: BrokeredExecutionDelivery {
                request_id: context.request.request.request_id.clone(),
                outcome: BrokeredExecutionOutcome::Uncertain {
                    execution_id,
                    error: test_contract_error("checkpoint outcome is uncertain"),
                },
            },
        })
    }
}

struct TestBoundOperation {
    target: TargetRef,
    target_identity_digest: Digest,
    runtime_fence: RuntimeExecutionFence,
    operation_digest: Digest,
    input_digest: Digest,
}

impl BoundExecutionOperation for TestBoundOperation {
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

struct UnknownTarget {
    calls: Arc<Mutex<usize>>,
}

impl ExecutionTarget<TestBoundOperation> for UnknownTarget {
    fn execute(&mut self, _operation: &TestBoundOperation) -> ExecutionTargetOutcome {
        *self.calls.lock().unwrap() += 1;
        ExecutionTargetOutcome::Unknown {
            safe_detail: Some(BoundedText::new("checkpoint transport disconnected").unwrap()),
        }
    }
}

struct ConclusiveTarget {
    calls: Arc<Mutex<usize>>,
    outcome: ExecutionTargetOutcome,
}

impl ExecutionTarget<TestBoundOperation> for ConclusiveTarget {
    fn execute(&mut self, _operation: &TestBoundOperation) -> ExecutionTargetOutcome {
        *self.calls.lock().unwrap() += 1;
        self.outcome.clone()
    }
}

struct TestAudit {
    persisted_at_ms: u64,
}

impl SecurityAuditGate<TestBoundOperation> for TestAudit {
    fn persist_start(
        &mut self,
        _execution: &crate::storage::ExecutionRecord,
        _operation: &TestBoundOperation,
    ) -> Result<SecurityAuditProof, SecurityAuditError> {
        Ok(SecurityAuditProof {
            proof_digest: test_digest(),
            persisted_at_ms: self.persisted_at_ms,
        })
    }
}

fn driver_command(
    actor_id: &ActorId,
    operation: &str,
    approval_id: &ApprovalId,
    now_ms: u64,
) -> Result<LedgerCommand, ContractError> {
    Ok(LedgerCommand {
        actor_id: actor_id.clone(),
        idempotency_key: IdempotencyKey::new(format!(
            "uncertain-test-{operation}-{}",
            approval_id.as_str()
        ))
        .map_err(|_| test_contract_error("invalid uncertain test command key"))?,
        command_digest: digest_json(&(operation, approval_id, now_ms))
            .map_err(|_| test_contract_error("could not digest uncertain test command"))?,
        committed_at_ms: now_ms,
    })
}

fn conclusive_driver_command(
    actor_id: &ActorId,
    operation: &str,
    approval_id: &ApprovalId,
    now_ms: u64,
) -> Result<LedgerCommand, ContractError> {
    Ok(LedgerCommand {
        actor_id: actor_id.clone(),
        idempotency_key: IdempotencyKey::new(format!(
            "conclusive-test-{operation}-{}",
            approval_id.as_str()
        ))
        .map_err(|_| test_contract_error("invalid conclusive test command key"))?,
        command_digest: digest_json(&("conclusive-test-v1", operation, approval_id))
            .map_err(|_| test_contract_error("could not digest conclusive test command"))?,
        committed_at_ms: now_ms,
    })
}

fn stable_test_execution_ids(
    request_id: &RequestId,
) -> Result<(PermitId, ExecutionId), ContractError> {
    let body = request_id
        .as_str()
        .strip_prefix("req_")
        .ok_or_else(|| test_contract_error("test request identity is invalid"))?;
    let permit_id = PermitId::parse(format!("prm_{body}"))
        .map_err(|_| test_contract_error("test permit identity is invalid"))?;
    let execution_id = ExecutionId::parse(format!("exe_{body}"))
        .map_err(|_| test_contract_error("test execution identity is invalid"))?;
    Ok((permit_id, execution_id))
}

fn map_test_execution(
    store: &mut SqliteTaskStore,
    context: BrokeredResolutionContext<'_>,
    execution_id: ExecutionId,
    execution: Result<GovernedExecutionResult, GovernedExecutionError>,
) -> Result<BrokeredResolution, ContractError> {
    let outcome = match execution {
        Ok(result) if result.succeeded => BrokeredExecutionOutcome::Succeeded {
            execution_id: execution_id.clone(),
            result: result
                .typed_result
                .ok_or_else(|| test_contract_error("successful test execution omitted result"))?,
        },
        Ok(_) | Err(GovernedExecutionError::SecurityAuditKnownNoEffect { .. }) => {
            BrokeredExecutionOutcome::Failed {
                execution_id: execution_id.clone(),
                error: test_contract_error("test execution failed before an external effect"),
            }
        }
        Err(GovernedExecutionError::CompletionUnknown { .. })
        | Err(GovernedExecutionError::OutcomeUnknown { .. }) => {
            let record = store
                .load_execution_record(&execution_id)
                .map_err(|_| test_contract_error("test execution state is unavailable"))?;
            match record.state {
                ExecutionState::Succeeded => {
                    let result = store
                        .load_brokered_execution_result(&execution_id)
                        .map_err(|_| test_contract_error("test execution result is unavailable"))?;
                    BrokeredExecutionOutcome::Succeeded {
                        execution_id: execution_id.clone(),
                        result: result.result,
                    }
                }
                ExecutionState::Uncertain => BrokeredExecutionOutcome::Uncertain {
                    execution_id: execution_id.clone(),
                    error: test_contract_error("test execution outcome is uncertain"),
                },
                _ => BrokeredExecutionOutcome::Failed {
                    execution_id: execution_id.clone(),
                    error: test_contract_error("test execution failed without an external effect"),
                },
            }
        }
        Err(_) => BrokeredExecutionOutcome::Failed {
            execution_id: execution_id.clone(),
            error: test_contract_error("test execution could not be completed"),
        },
    };
    Ok(BrokeredResolution {
        source: BrokeredResolutionSource::Execution { execution_id },
        delivery: BrokeredExecutionDelivery {
            request_id: context.request.request.request_id.clone(),
            outcome,
        },
    })
}

fn durable_success_result(operation: &BrokeredOperation) -> BrokeredOperationResult {
    let BrokeredOperation::WorkspaceCheckpointCreateV1(operation) = operation;
    BrokeredOperationResult::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1Result {
        checkpoint_id: operation.checkpoint_id.clone(),
        outcome: WorkspaceCheckpointCreateV1Outcome::Created {
            snapshot_id: BoundedOpaque::new("durable-snapshot").unwrap(),
        },
    })
}

fn fabricated_success_result() -> BrokeredOperationResult {
    BrokeredOperationResult::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1Result {
        checkpoint_id: CheckpointId::new(),
        outcome: WorkspaceCheckpointCreateV1Outcome::Created {
            snapshot_id: BoundedOpaque::new("fabricated-snapshot").unwrap(),
        },
    })
}

struct ConclusiveSchedulerFixture {
    _root: TempDir,
    scheduler: TaskScheduler<BrokeredFactory>,
    actor_id: ActorId,
    approval_id: ApprovalId,
    started_at: u64,
    writes: Arc<Mutex<RuntimeWrites>>,
    target_calls: Arc<Mutex<usize>>,
    resolve_calls: Arc<Mutex<usize>>,
}

#[test]
fn default_task_only_driver_rejects_before_approval() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    let task = coordinator
        .submit(&actor_id, submission("task-only-rejects-side-effect"))
        .unwrap();
    drop(coordinator);
    let writes = Arc::new(Mutex::new(RuntimeWrites::default()));
    let started_at = now_ms().unwrap().saturating_add(1);
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("task-only-rejecting-worker").unwrap(),
        BrokeredFactory {
            writes: Arc::clone(&writes),
            expires_at_ms: started_at + 10_000,
            fail_acknowledgement: false,
            fail_result: false,
        },
    )
    .unwrap();
    assert!(matches!(
        scheduler.tick(started_at).unwrap(),
        SchedulerTick::Started(_)
    ));
    assert!(matches!(
        scheduler.tick(started_at + 1).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Failed,
            ..
        })
    ));
    let events = scheduler
        .coordinator
        .events(&actor_id, &task.task_id, None, 64)
        .unwrap();
    assert!(!events
        .events
        .iter()
        .any(|event| matches!(event.event, TaskEvent::ApprovalRequested { .. })));
    let writes = writes.lock().unwrap();
    assert!(writes.acknowledgements.is_empty());
    assert!(writes.results.is_empty());
}

fn conclusive_scheduler(
    mode: ConclusiveDriverMode,
    submission_key: &str,
) -> ConclusiveSchedulerFixture {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor_id, submission(submission_key))
        .unwrap();
    let writes = Arc::new(Mutex::new(RuntimeWrites::default()));
    let target_calls = Arc::new(Mutex::new(0));
    let resolve_calls = Arc::new(Mutex::new(0));
    let started_at = now_ms().unwrap().saturating_add(1);
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new(format!("worker-{submission_key}")).unwrap(),
        BrokeredFactory {
            writes: Arc::clone(&writes),
            expires_at_ms: started_at + 10_000,
            fail_acknowledgement: false,
            fail_result: false,
        },
    )
    .unwrap()
    .with_brokered_execution_driver(Box::new(ConclusiveDriver {
        target_calls: Arc::clone(&target_calls),
        resolve_calls: Arc::clone(&resolve_calls),
        mode,
    }));
    assert!(matches!(
        scheduler.tick(started_at).unwrap(),
        SchedulerTick::Started(_)
    ));
    assert!(matches!(
        scheduler.tick(started_at + 1).unwrap(),
        SchedulerTick::Progressed(TaskView {
            state: TaskState::WaitingApproval,
            ..
        })
    ));
    let approval_id = scheduler
        .active
        .as_ref()
        .unwrap()
        .pending_brokered
        .as_ref()
        .unwrap()
        .approval
        .approval_id
        .clone();
    ConclusiveSchedulerFixture {
        _root: root,
        scheduler,
        actor_id,
        approval_id,
        started_at,
        writes,
        target_calls,
        resolve_calls,
    }
}

#[test]
fn completion_unknown_after_commit_recovers_durable_success_without_retry() {
    let ConclusiveSchedulerFixture {
        _root,
        mut scheduler,
        actor_id,
        approval_id,
        started_at,
        writes,
        target_calls,
        resolve_calls,
    } = conclusive_scheduler(
        ConclusiveDriverMode::CompletionUnknownAfterCommit,
        "completion-unknown-after-commit",
    );
    let request_id = scheduler
        .active
        .as_ref()
        .unwrap()
        .pending_brokered
        .as_ref()
        .unwrap()
        .brokered
        .request_id
        .clone();
    let key = IdempotencyKey::new("approve-completion-unknown").unwrap();

    let tick = scheduler
        .resolve_approval(
            &actor_id,
            key.clone(),
            &approval_id,
            ApprovalDecision::Approve,
            started_at + 750,
        )
        .unwrap();
    assert!(
        matches!(
            tick,
            SchedulerTick::Progressed(TaskView {
                state: TaskState::Running,
                ..
            })
        ),
        "unexpected completion recovery tick: {tick:?}; dispatch={:?}; writes={:?}; target={}; resolve={}",
        scheduler
            .coordinator
            .store
            .load_brokered_runtime_dispatch_record(
                &request_id,
                BrokeredRuntimeDispatchKind::Result,
        ),
        writes.lock().unwrap().results,
        *target_calls.lock().unwrap(),
        *resolve_calls.lock().unwrap(),
    );
    assert_eq!(*target_calls.lock().unwrap(), 1);
    assert_eq!(*resolve_calls.lock().unwrap(), 1);
    let writes = writes.lock().unwrap();
    assert_eq!(writes.results.len(), 1);
    let (execution_id, result) = match &writes.results[0].outcome {
        BrokeredExecutionOutcome::Succeeded {
            execution_id,
            result,
        } => (execution_id.clone(), result.clone()),
        outcome => panic!("unexpected recovered outcome: {outcome:?}"),
    };
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_execution_record(&execution_id)
            .unwrap()
            .state,
        ExecutionState::Succeeded
    );
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_brokered_execution_result(&execution_id)
            .unwrap()
            .result,
        result
    );
    drop(writes);

    assert!(matches!(
        scheduler
            .resolve_approval(
                &actor_id,
                key,
                &approval_id,
                ApprovalDecision::Approve,
                started_at + 751,
            )
            .unwrap(),
        SchedulerTick::Progressed(_)
    ));
    assert_eq!(*target_calls.lock().unwrap(), 1);
    assert_eq!(*resolve_calls.lock().unwrap(), 1);
}

#[test]
fn success_is_recovered_after_result_prepare_rejection_without_target_retry() {
    let ConclusiveSchedulerFixture {
        _root,
        mut scheduler,
        actor_id,
        approval_id,
        started_at,
        writes,
        target_calls,
        resolve_calls,
    } = conclusive_scheduler(
        ConclusiveDriverMode::WrongSuccessBeforePrepare,
        "result-prepare-retry",
    );
    let key = IdempotencyKey::new("approve-result-prepare-retry").unwrap();

    let first = scheduler.resolve_approval(
        &actor_id,
        key.clone(),
        &approval_id,
        ApprovalDecision::Approve,
        started_at + 750,
    );
    assert!(first.is_err(), "unexpected first prepare result: {first:?}");
    assert_eq!(*target_calls.lock().unwrap(), 1);
    assert!(writes.lock().unwrap().results.is_empty());
    scheduler
        .active
        .as_mut()
        .unwrap()
        .pending_brokered
        .as_mut()
        .unwrap()
        .resolution = None;

    let tick = scheduler
        .resolve_approval(
            &actor_id,
            key,
            &approval_id,
            ApprovalDecision::Approve,
            started_at + 751,
        )
        .unwrap();
    assert!(matches!(
        tick,
        SchedulerTick::Progressed(TaskView {
            state: TaskState::Running,
            ..
        })
    ));
    assert_eq!(*resolve_calls.lock().unwrap(), 2);
    assert_eq!(*target_calls.lock().unwrap(), 1);
    let writes = writes.lock().unwrap();
    assert_eq!(writes.results.len(), 1);
    assert!(matches!(
        writes.results[0].outcome,
        BrokeredExecutionOutcome::Succeeded { .. }
    ));
}

#[test]
fn fabricated_success_for_failed_execution_is_rejected_before_runtime_write() {
    let ConclusiveSchedulerFixture {
        _root,
        mut scheduler,
        actor_id,
        approval_id,
        started_at,
        writes,
        target_calls,
        resolve_calls,
    } = conclusive_scheduler(
        ConclusiveDriverMode::FabricatedSuccessAfterFailure,
        "fabricated-success",
    );
    let request_id = scheduler
        .active
        .as_ref()
        .unwrap()
        .pending_brokered
        .as_ref()
        .unwrap()
        .brokered
        .request_id
        .clone();
    let (_, execution_id) = stable_test_execution_ids(&request_id).unwrap();
    let key = IdempotencyKey::new("approve-fabricated-success").unwrap();

    let first = scheduler.resolve_approval(
        &actor_id,
        key.clone(),
        &approval_id,
        ApprovalDecision::Approve,
        started_at + 750,
    );
    assert!(
        first.is_err(),
        "unexpected fabricated prepare result: {first:?}"
    );
    assert_eq!(*target_calls.lock().unwrap(), 1);
    assert_eq!(*resolve_calls.lock().unwrap(), 1);
    assert!(writes.lock().unwrap().results.is_empty());
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_execution_record(&execution_id)
            .unwrap()
            .state,
        ExecutionState::Failed
    );
    assert!(matches!(
        scheduler
            .coordinator
            .store
            .load_brokered_runtime_dispatch_record(
                &request_id,
                BrokeredRuntimeDispatchKind::Result,
            ),
        Err(StoreError::LedgerNotFound { .. })
    ));

    assert!(scheduler
        .resolve_approval(
            &actor_id,
            key,
            &approval_id,
            ApprovalDecision::Approve,
            started_at + 751,
        )
        .is_err());
    assert_eq!(*target_calls.lock().unwrap(), 1);
    assert_eq!(*resolve_calls.lock().unwrap(), 1);
    assert!(writes.lock().unwrap().results.is_empty());
}

#[test]
fn durable_denial_is_acknowledged_and_delivered_once_across_lease_renewal() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor_id, submission("brokered-deny"))
        .unwrap();
    let writes = Arc::new(Mutex::new(RuntimeWrites::default()));
    let resolutions = Arc::new(Mutex::new(Vec::new()));
    // The test drives logical lease boundaries; keep wall-clock progress below
    // that logical anchor even when parallel CI stalls this test thread.
    let started_at = now_ms().unwrap().saturating_add(LOGICAL_CLOCK_HEADROOM_MS);
    let mut scheduler = TaskScheduler::open_with_config(
        &database_path,
        Some(installation.clone()),
        BoundedOpaque::new("worker-brokered-deny").unwrap(),
        BrokeredFactory {
            writes: Arc::clone(&writes),
            expires_at_ms: started_at + 10_000,
            fail_acknowledgement: false,
            fail_result: false,
        },
        TaskSchedulerConfig {
            lease_duration: Duration::from_millis(1_000),
            lease_renewal_margin: Duration::from_millis(300),
            runtime_operation_timeout: Duration::from_millis(500),
        },
    )
    .unwrap()
    .with_brokered_execution_driver(Box::new(DenyingDriver {
        resolutions: Arc::clone(&resolutions),
    }));
    assert!(matches!(
        scheduler.tick(started_at).unwrap(),
        SchedulerTick::Started(_)
    ));
    assert!(matches!(
        scheduler.tick(started_at + 1).unwrap(),
        SchedulerTick::Progressed(TaskView {
            state: TaskState::WaitingApproval,
            ..
        })
    ));
    let approval_id = scheduler
        .active
        .as_ref()
        .unwrap()
        .pending_brokered
        .as_ref()
        .unwrap()
        .approval
        .approval_id
        .clone();
    assert_eq!(
        scheduler
            .active
            .as_ref()
            .unwrap()
            .pending_brokered
            .as_ref()
            .unwrap()
            .approval
            .summary
            .as_str(),
        "Policy-authorized checkpoint approval"
    );
    assert_eq!(writes.lock().unwrap().acknowledgements.len(), 1);

    assert!(matches!(
        scheduler
            .resolve_approval(
                &actor_id,
                IdempotencyKey::new("deny-brokered").unwrap(),
                &approval_id,
                ApprovalDecision::Deny,
                started_at + 750,
            )
            .unwrap(),
        SchedulerTick::Progressed(TaskView {
            state: TaskState::Running,
            ..
        })
    ));
    assert_eq!(writes.lock().unwrap().results.len(), 1);
    let lease_observations = resolutions.lock().unwrap();
    assert_eq!(lease_observations.len(), 1);
    assert_eq!(lease_observations[0].0, 1);
    assert!(lease_observations[0].1 > 1);
    drop(lease_observations);

    assert!(matches!(
        scheduler.shutdown(started_at + 751).unwrap(),
        SchedulerTick::Settled(_)
    ));
    drop(scheduler);
    coordinator
        .submit(&actor_id, submission("brokered-replay-other-run"))
        .unwrap();
    let mut replacement = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-brokered-replacement").unwrap(),
        BrokeredFactory {
            writes: Arc::clone(&writes),
            expires_at_ms: started_at + 20_000,
            fail_acknowledgement: false,
            fail_result: false,
        },
    )
    .unwrap()
    .with_brokered_execution_driver(Box::new(DenyingDriver {
        resolutions: Arc::clone(&resolutions),
    }));
    assert!(matches!(
        replacement.tick(started_at + 752).unwrap(),
        SchedulerTick::Started(_)
    ));
    assert!(matches!(
        replacement
            .resolve_approval(
                &actor_id,
                IdempotencyKey::new("deny-brokered").unwrap(),
                &approval_id,
                ApprovalDecision::Deny,
                started_at + 753,
            )
            .unwrap(),
        SchedulerTick::Progressed(_)
    ));
    assert_eq!(writes.lock().unwrap().results.len(), 1);
    assert_eq!(resolutions.lock().unwrap().len(), 1);
}

#[test]
fn brokered_expiry_is_durable_and_never_delivers_a_result() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor_id, submission("brokered-expiry"))
        .unwrap();
    let writes = Arc::new(Mutex::new(RuntimeWrites::default()));
    let resolutions = Arc::new(Mutex::new(Vec::new()));
    let started_at = now_ms().unwrap().saturating_add(1);
    let expires_at_ms = started_at + 10_000;
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-brokered-expiry").unwrap(),
        BrokeredFactory {
            writes: Arc::clone(&writes),
            expires_at_ms,
            fail_acknowledgement: false,
            fail_result: false,
        },
    )
    .unwrap()
    .with_brokered_execution_driver(Box::new(DenyingDriver {
        resolutions: Arc::clone(&resolutions),
    }));
    scheduler.tick(started_at).unwrap();
    scheduler.tick(started_at + 1).unwrap();
    let approval_id = scheduler
        .active
        .as_ref()
        .unwrap()
        .pending_brokered
        .as_ref()
        .unwrap()
        .approval
        .approval_id
        .clone();

    assert!(matches!(
        scheduler.tick(expires_at_ms).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Failed,
            ..
        })
    ));
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_approval_record(&approval_id)
            .unwrap()
            .state,
        ApprovalState::Expired
    );
    assert_eq!(writes.lock().unwrap().acknowledgements.len(), 1);
    assert!(writes.lock().unwrap().results.is_empty());
    assert!(resolutions.lock().unwrap().is_empty());
}

#[test]
fn failed_acknowledgement_is_written_once_then_fails_closed() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor_id, submission("brokered-ack-failure"))
        .unwrap();
    let writes = Arc::new(Mutex::new(RuntimeWrites::default()));
    let started_at = now_ms().unwrap().saturating_add(1);
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-brokered-ack-failure").unwrap(),
        BrokeredFactory {
            writes: Arc::clone(&writes),
            expires_at_ms: started_at + 10_000,
            fail_acknowledgement: true,
            fail_result: false,
        },
    )
    .unwrap()
    .with_brokered_execution_driver(Box::new(DenyingDriver {
        resolutions: Arc::new(Mutex::new(Vec::new())),
    }));
    scheduler.tick(started_at).unwrap();

    assert!(matches!(
        scheduler.tick(started_at + 1).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Failed,
            ..
        })
    ));
    assert_eq!(writes.lock().unwrap().acknowledgements.len(), 1);
    assert!(writes.lock().unwrap().results.is_empty());
    assert!(scheduler.active.is_none());
    assert!(matches!(
        scheduler.tick(started_at + 2).unwrap(),
        SchedulerTick::Idle
    ));
    assert_eq!(writes.lock().unwrap().acknowledgements.len(), 1);
}

#[test]
fn uncertain_result_transport_loss_preserves_suspended_task() {
    assert_uncertain_result_failure(true, false);
}

#[test]
fn uncertain_result_receipt_loss_preserves_suspended_task() {
    assert_uncertain_result_failure(false, true);
}

fn assert_uncertain_result_failure(fail_result: bool, fail_completion: bool) {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor_id, submission("brokered-uncertain"))
        .unwrap();
    let writes = Arc::new(Mutex::new(RuntimeWrites::default()));
    let target_calls = Arc::new(Mutex::new(0));
    // Keep the logical approval timeline ahead of wall-clock timestamps that
    // storage may create while a loaded parallel test runner stalls this test.
    let started_at = now_ms().unwrap().saturating_add(LOGICAL_CLOCK_HEADROOM_MS);
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-brokered-uncertain").unwrap(),
        BrokeredFactory {
            writes: Arc::clone(&writes),
            expires_at_ms: started_at + 10_000,
            fail_acknowledgement: false,
            fail_result,
        },
    )
    .unwrap()
    .with_brokered_execution_driver(Box::new(UncertainDriver {
        target_calls: Arc::clone(&target_calls),
    }));
    scheduler.tick(started_at).unwrap();
    scheduler.tick(started_at + 1).unwrap();
    let active = scheduler.active.as_ref().unwrap();
    let binding_id = active.binding.binding_id.clone();
    let run_id = active.scheduled.run_id.clone();
    let approval_id = active
        .pending_brokered
        .as_ref()
        .unwrap()
        .approval
        .approval_id
        .clone();
    scheduler.fail_next_brokered_result_completion = fail_completion;

    let resolved = scheduler
        .resolve_approval(
            &actor_id,
            IdempotencyKey::new("approve-uncertain-brokered").unwrap(),
            &approval_id,
            ApprovalDecision::Approve,
            started_at + 750,
        )
        .unwrap();
    assert!(
        matches!(
            resolved,
            SchedulerTick::Settled(TaskView {
                state: TaskState::Suspended,
                ..
            })
        ),
        "unexpected uncertain settlement: {resolved:?}; target_calls={}; result_writes={}",
        *target_calls.lock().unwrap(),
        writes.lock().unwrap().results.len(),
    );
    assert_eq!(*target_calls.lock().unwrap(), 1);
    assert_eq!(writes.lock().unwrap().results.len(), 1);
    assert!(scheduler.active.is_none());
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_runtime_binding_record(&binding_id)
            .unwrap()
            .state,
        crate::storage::RuntimeBindingState::Closed
    );
    assert!(
        scheduler
            .coordinator
            .store
            .load_run_lease(&run_id)
            .unwrap()
            .expires_at_ms
            <= started_at + 754
    );
    let request_id = writes.lock().unwrap().results[0].request_id.clone();
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_brokered_runtime_dispatch_record(
                &request_id,
                BrokeredRuntimeDispatchKind::Result,
            )
            .unwrap()
            .state,
        BrokeredRuntimeDispatchState::Unknown
    );
    assert!(scheduler
        .resolve_approval(
            &actor_id,
            IdempotencyKey::new("approve-uncertain-brokered").unwrap(),
            &approval_id,
            ApprovalDecision::Approve,
            started_at + 755,
        )
        .is_err());
    assert_eq!(writes.lock().unwrap().results.len(), 1);
    assert_eq!(*target_calls.lock().unwrap(), 1);
}

fn submission(key: &str) -> SubmitTask {
    SubmitTask {
        request_id: RequestId::new(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        intent: BoundedText::new("create a checkpoint").unwrap(),
        target: GatewayCapabilityProfile::task_only_v1().governed_target(),
        runtime: RuntimeSelector {
            runtime: BoundedName::new("core").unwrap(),
            profile: Some(BoundedName::new("gateway-brokered-v1").unwrap()),
        },
    }
}

fn test_digest() -> Digest {
    Digest::parse("a".repeat(64)).unwrap()
}

fn test_contract_error(message: impl Into<String>) -> ContractError {
    ContractError::new(
        "brokered_scheduler_test",
        ErrorCategory::Internal,
        false,
        message,
    )
    .unwrap()
}
