use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::{fs, os::unix::fs::PermissionsExt};

use cosh_gateway_contracts::capability::{CapabilityRequest, CapabilityScope, OperationDescriptor};
use cosh_gateway_contracts::common::{
    BoundedName, BoundedOpaque, BoundedText, Digest, IdempotencyKey, RuntimeBindingRef,
    RuntimeSelector,
};
use cosh_gateway_contracts::external::{ExternalRef, ExternalRefKind};
use cosh_gateway_contracts::ids::{
    AgentSessionId, ApprovalId, InstallationId, RequestId, RuntimeBindingId, RuntimeInstanceId,
    TurnId,
};
use cosh_gateway_contracts::profile::GatewayCapabilityProfile;
use cosh_gateway_contracts::runtime::ToolSummary;
use tempfile::TempDir;

use super::*;
use crate::daemon::{
    actor_id_for_uid, actor_ref_for_uid, now_ms, CancelTask, LaunchReadiness,
    PreRuntimeBaselineView, PreRuntimeCheckpointReconcileRequest, RetryTask, SubmitLaunch,
    SubmitTask,
};
use crate::runtime::{
    AcpRuntimeProfileId, InstalledAcpRuntimePortFactory, LocalOsActorResolver,
    ScheduledAgentRuntimeFactory, TrustedWorkspaceResolver,
};

struct CheckpointDriver {
    prepares: Arc<AtomicUsize>,
    creates: Arc<AtomicUsize>,
    reconciles: Arc<AtomicUsize>,
    create_result: PreRuntimeCheckpointCreateResult,
    reconcile_result: PreRuntimeCheckpointReconcileResult,
}

impl PreRuntimeCheckpointDriver for CheckpointDriver {
    fn prepare_baseline(
        &mut self,
        _request: &PreRuntimeCheckpointRequest,
    ) -> Result<PreRuntimeCheckpointBinding, ContractError> {
        let preparation = self.prepares.fetch_add(1, Ordering::Relaxed);
        Ok(PreRuntimeCheckpointBinding {
            provider_workspace_id: BoundedOpaque::new("test-workspace").unwrap(),
            provider_generation: BoundedOpaque::new(
                if preparation == 0 { "07" } else { "08" }.repeat(32),
            )
            .unwrap(),
            operation_digest: sha256_digest(b"test-checkpoint-operation"),
        })
    }

    fn create_baseline(
        &mut self,
        _request: &PreRuntimeCheckpointRequest,
        _binding: &PreRuntimeCheckpointBinding,
    ) -> Result<PreRuntimeCheckpointCreateResult, ContractError> {
        self.creates.fetch_add(1, Ordering::Relaxed);
        Ok(self.create_result.clone())
    }

    fn reconcile_baseline(
        &mut self,
        _request: &PreRuntimeCheckpointReconcileRequest,
        binding: &PreRuntimeCheckpointBinding,
    ) -> Result<PreRuntimeCheckpointReconcileResult, ContractError> {
        assert_eq!(binding.provider_generation.as_str(), "07".repeat(32));
        self.reconciles.fetch_add(1, Ordering::Relaxed);
        Ok(self.reconcile_result.clone())
    }
}

struct ApprovalBarrierDriver {
    prepares: Arc<AtomicUsize>,
    creates: Arc<AtomicUsize>,
    reconciles: Arc<AtomicUsize>,
    prepare_result: ApprovalCheckpointPrepareResult,
    create_result: ApprovalCheckpointCreateResult,
    reconcile_result: ApprovalCheckpointReconcileResult,
}

impl PreRuntimeCheckpointDriver for ApprovalBarrierDriver {
    fn prepare_baseline(
        &mut self,
        _request: &PreRuntimeCheckpointRequest,
    ) -> Result<PreRuntimeCheckpointBinding, ContractError> {
        Err(runtime_lost_error("unexpected_baseline", "unexpected baseline").unwrap())
    }

    fn create_baseline(
        &mut self,
        _request: &PreRuntimeCheckpointRequest,
        _binding: &PreRuntimeCheckpointBinding,
    ) -> Result<PreRuntimeCheckpointCreateResult, ContractError> {
        unreachable!("approval barrier tests do not create a pre-Runtime baseline")
    }

    fn reconcile_baseline(
        &mut self,
        _request: &PreRuntimeCheckpointReconcileRequest,
        _binding: &PreRuntimeCheckpointBinding,
    ) -> Result<PreRuntimeCheckpointReconcileResult, ContractError> {
        unreachable!("approval barrier tests do not reconcile a pre-Runtime baseline")
    }

    fn prepare_approval_checkpoint(
        &mut self,
        _request: &ApprovalCheckpointRequest,
    ) -> Result<ApprovalCheckpointPrepareResult, ContractError> {
        self.prepares.fetch_add(1, Ordering::Relaxed);
        Ok(self.prepare_result.clone())
    }

    fn create_approval_checkpoint(
        &mut self,
        request: &ApprovalCheckpointRequest,
        _binding: &PreRuntimeCheckpointBinding,
    ) -> Result<ApprovalCheckpointCreateResult, ContractError> {
        self.creates.fetch_add(1, Ordering::Relaxed);
        let mut result = self.create_result.clone();
        if let ApprovalCheckpointCreateResult::Created { evidence } = &mut result {
            evidence.checkpoint_id = request.checkpoint_id.clone();
        }
        Ok(result)
    }

    fn reconcile_approval_checkpoint(
        &mut self,
        request: &ApprovalCheckpointRequest,
        _binding: &PreRuntimeCheckpointBinding,
    ) -> Result<ApprovalCheckpointReconcileResult, ContractError> {
        self.reconciles.fetch_add(1, Ordering::Relaxed);
        let mut result = self.reconcile_result.clone();
        if let ApprovalCheckpointReconcileResult::Created { evidence } = &mut result {
            evidence.checkpoint_id = request.checkpoint_id.clone();
        }
        Ok(result)
    }
}

fn approval_binding() -> PreRuntimeCheckpointBinding {
    PreRuntimeCheckpointBinding {
        provider_workspace_id: BoundedOpaque::new("approval-workspace").unwrap(),
        provider_generation: BoundedOpaque::new("09".repeat(32)).unwrap(),
        operation_digest: sha256_digest(b"approval-checkpoint-operation"),
    }
}

fn approval_evidence(checkpoint_id: CheckpointId) -> ApprovalCheckpointEvidence {
    ApprovalCheckpointEvidence {
        checkpoint_id,
        provider_generation: BoundedOpaque::new("09".repeat(32)).unwrap(),
        evidence_digest: sha256_digest(b"approval-checkpoint-evidence"),
    }
}

fn launch_catalog(workspace: WorkspaceRef) -> TaskLaunchCatalog {
    TaskLaunchCatalog::new(
        workspace,
        LaunchReadiness::Ready,
        LaunchReadiness::Ready,
        LaunchReadiness::Ready,
    )
}

#[test]
fn runtime_start_v4_rejects_a_launch_runtime_that_disagrees_with_its_route() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let workspace = WorkspaceRef {
        scope_digest: sha256_digest(b"runtime-route-workspace"),
        display_name: None,
    };
    let catalog = launch_catalog(workspace.clone());
    let mut coordinator = TaskCoordinator::open_for_launch_catalog(
        &database,
        Some(installation.clone()),
        catalog.clone(),
    )
    .unwrap();
    let actor = actor_ref_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit_launch_admitted(
            &actor,
            &catalog,
            SubmitLaunch {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("tampered-v4-route").unwrap(),
                launch: TaskLaunchSpecV1::new(
                    BoundedText::new("inspect workspace").unwrap(),
                    TaskRuntime::Core,
                    workspace,
                    CheckpointPolicy::Off,
                    ApprovalPolicy::AllowAll,
                ),
            },
        )
        .unwrap();
    let mut payload = coordinator
        .store
        .peek_ready_outbox(
            &runtime_start_delivery_kind(),
            now_ms().unwrap().saturating_add(1),
        )
        .unwrap()
        .unwrap()
        .payload;
    payload["launch"]["runtime"] = serde_json::json!("codex");

    assert!(matches!(
        decode_runtime_start_intent(payload, &catalog),
        Err(GatewayDaemonError::Protocol(message))
            if message.contains("exact Runtime route")
    ));
}

#[test]
fn checkpoint_possibly_applied_reconciles_without_recreating_or_starting_runtime() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let workspace = WorkspaceRef {
        scope_digest: sha256_digest(b"checkpoint-unknown-workspace"),
        display_name: None,
    };
    let catalog = launch_catalog(workspace.clone());
    let mut coordinator = TaskCoordinator::open_for_launch_catalog(
        &database,
        Some(installation.clone()),
        catalog.clone(),
    )
    .unwrap();
    let actor = actor_ref_for_uid(&installation, 1000).unwrap();
    let view = coordinator
        .submit_launch_admitted(
            &actor,
            &catalog,
            SubmitLaunch {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("checkpoint-unknown").unwrap(),
                launch: TaskLaunchSpecV1::new(
                    BoundedText::new("inspect workspace").unwrap(),
                    TaskRuntime::Core,
                    workspace.clone(),
                    CheckpointPolicy::On,
                    ApprovalPolicy::AllowAll,
                ),
            },
        )
        .unwrap();
    let task_id = view.task_id.clone();
    let run_id = view.active_run_id.clone().unwrap();
    assert_eq!(
        view.baseline.as_ref().unwrap().state,
        PreRuntimeBaselineState::Pending
    );

    let prepares = Arc::new(AtomicUsize::new(0));
    let creates = Arc::new(AtomicUsize::new(0));
    let reconciles = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(AtomicUsize::new(0));
    let error = ContractError::new(
        "checkpoint_transport_lost",
        ErrorCategory::RuntimeUnavailable,
        false,
        "checkpoint response was lost",
    )
    .unwrap();
    let mut scheduler = TaskScheduler::open_for_launch_catalog(
        &database,
        Some(installation),
        BoundedOpaque::new("checkpoint-unknown-worker").unwrap(),
        catalog,
        NeverStartFactory(Arc::clone(&starts)),
    )
    .unwrap()
    .with_pre_runtime_checkpoint_driver(Box::new(CheckpointDriver {
        prepares: Arc::clone(&prepares),
        creates: Arc::clone(&creates),
        reconciles: Arc::clone(&reconciles),
        create_result: PreRuntimeCheckpointCreateResult::PossiblyApplied { error },
        reconcile_result: PreRuntimeCheckpointReconcileResult::Unknown {
            reason: BoundedText::new("no exact checkpoint evidence").unwrap(),
        },
    }));
    let now = now_ms().unwrap().saturating_add(1);
    assert_eq!(scheduler.tick(now).unwrap(), SchedulerTick::Idle);
    assert!(matches!(
        scheduler.tick(now + 1).unwrap(),
        SchedulerTick::Progressed(TaskView {
            state: TaskState::Suspended,
            baseline: Some(PreRuntimeBaselineView {
                state: PreRuntimeBaselineState::Unknown,
                ..
            }),
            ..
        })
    ));
    assert_eq!(creates.load(Ordering::Relaxed), 1);
    assert_eq!(prepares.load(Ordering::Relaxed), 1);
    assert_eq!(reconciles.load(Ordering::Relaxed), 1);
    assert_eq!(starts.load(Ordering::Relaxed), 0);
    assert_eq!(scheduler.tick(now + 2).unwrap(), SchedulerTick::Idle);
    assert_eq!(creates.load(Ordering::Relaxed), 1);
    let events = scheduler
        .coordinator
        .events(&actor.actor_id, &task_id, None, 10)
        .unwrap();
    assert!(events.events.iter().any(|event| matches!(
        event.event,
        TaskEvent::RunSuspended {
            reason: SuspensionCode::OperatorRequired,
            ..
        }
    )));
    let retry = scheduler.coordinator.retry_admitted(
        &actor,
        &GatewayCapabilityProfile::task_only_v1().governed_target(),
        &workspace,
        &RuntimeSelector {
            runtime: BoundedName::new("core").unwrap(),
            profile: Some(BoundedName::new("gateway-brokered-v1").unwrap()),
        },
        RetryTask {
            request_id: RequestId::new(),
            idempotency_key: IdempotencyKey::new("retry-uncertain-checkpoint").unwrap(),
            task_id,
            previous_run_id: run_id,
            expected_revision: None,
        },
    );
    assert!(matches!(
        retry,
        Err(GatewayDaemonError::Protocol(message))
            if message.contains("uncertain checkpoint creation")
    ));
}

#[test]
fn auto_skips_known_no_effect_but_on_fails_closed_before_runtime() {
    for (policy, expected_state, should_start, provider_skipped) in [
        (
            CheckpointPolicy::Auto,
            PreRuntimeBaselineState::Skipped,
            true,
            false,
        ),
        (
            CheckpointPolicy::On,
            PreRuntimeBaselineState::Failed,
            false,
            false,
        ),
        (
            CheckpointPolicy::Auto,
            PreRuntimeBaselineState::Skipped,
            true,
            true,
        ),
        (
            CheckpointPolicy::On,
            PreRuntimeBaselineState::Failed,
            false,
            true,
        ),
    ] {
        let root = TempDir::new().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let database = root.path().join("gateway.db");
        let installation = InstallationId::new();
        let workspace = WorkspaceRef {
            scope_digest: sha256_digest(format!("checkpoint-{policy:?}").as_bytes()),
            display_name: None,
        };
        let catalog = launch_catalog(workspace.clone());
        let mut coordinator = TaskCoordinator::open_for_launch_catalog(
            &database,
            Some(installation.clone()),
            catalog.clone(),
        )
        .unwrap();
        let actor = actor_ref_for_uid(&installation, 1000).unwrap();
        let submitted = coordinator
            .submit_launch_admitted(
                &actor,
                &catalog,
                SubmitLaunch {
                    request_id: RequestId::new(),
                    idempotency_key: IdempotencyKey::new(format!("checkpoint-{policy:?}")).unwrap(),
                    launch: TaskLaunchSpecV1::new(
                        BoundedText::new("inspect workspace").unwrap(),
                        TaskRuntime::Core,
                        workspace,
                        policy,
                        ApprovalPolicy::AllowAll,
                    ),
                },
            )
            .unwrap();
        let task_id = submitted.task_id;
        let starts = Arc::new(AtomicUsize::new(0));
        let mut scheduler = TaskScheduler::open_for_launch_catalog(
            &database,
            Some(installation),
            BoundedOpaque::new(format!("checkpoint-{policy:?}-worker")).unwrap(),
            catalog,
            NeverStartFactory(Arc::clone(&starts)),
        )
        .unwrap();
        let skip_reason = BoundedText::new(
            "Empty workspace checkpoint was not created; add a file and submit a new Task",
        )
        .unwrap();
        let creates = Arc::new(AtomicUsize::new(0));
        if provider_skipped {
            scheduler = scheduler.with_pre_runtime_checkpoint_driver(Box::new(CheckpointDriver {
                prepares: Arc::new(AtomicUsize::new(0)),
                creates: Arc::clone(&creates),
                reconciles: Arc::new(AtomicUsize::new(0)),
                create_result: PreRuntimeCheckpointCreateResult::KnownNoEffect {
                    reason: skip_reason.clone(),
                },
                reconcile_result: PreRuntimeCheckpointReconcileResult::NotApplied,
            }));
        }
        let now = now_ms().unwrap().saturating_add(1);
        assert!(matches!(
            scheduler.tick(now).unwrap(),
            SchedulerTick::Progressed(TaskView {
                state: task_state,
                baseline: Some(PreRuntimeBaselineView {
                    state: baseline_state,
                    ..
                }),
                ..
            }) if baseline_state == expected_state
                && task_state == if policy == CheckpointPolicy::On {
                    TaskState::Failed
                } else {
                    TaskState::Queued
                }
        ));
        let next = scheduler.tick(now + 1);
        if should_start {
            assert!(matches!(next.unwrap(), SchedulerTick::Settled(_)));
            assert_eq!(starts.load(Ordering::Relaxed), 1);
        } else {
            assert_eq!(next.unwrap(), SchedulerTick::Idle);
            assert_eq!(starts.load(Ordering::Relaxed), 0);
            let events = scheduler
                .coordinator
                .events(&actor.actor_id, &task_id, None, 10)
                .unwrap();
            assert!(events
                .events
                .iter()
                .any(|event| matches!(event.event, TaskEvent::RunFailed { .. })));
            assert!(events
                .events
                .iter()
                .any(|event| matches!(event.event, TaskEvent::TaskFailed { .. })));
            if provider_skipped {
                assert!(events.events.iter().any(|event| matches!(
                    &event.event,
                    TaskEvent::TaskFailed { error }
                        if error.safe_message.as_str() == skip_reason.as_str()
                            && !error.retryable
                )));
            }
        }
        assert_eq!(
            creates.load(Ordering::Relaxed),
            usize::from(provider_skipped)
        );
    }
}

fn submission(key: &str) -> SubmitTask {
    SubmitTask {
        request_id: RequestId::new(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        intent: BoundedText::new("inspect service").unwrap(),
        target: GatewayCapabilityProfile::task_only_v1().governed_target(),
        runtime: RuntimeSelector {
            runtime: BoundedName::new("core").unwrap(),
            profile: Some(BoundedName::new("gateway-brokered-v1").unwrap()),
        },
    }
}

fn delegated_submission(key: &str) -> SubmitTask {
    SubmitTask {
        request_id: RequestId::new(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        intent: BoundedText::new("update dependencies and run tests").unwrap(),
        target: GatewayCapabilityProfile::delegated_acp_v1().governed_target(),
        runtime: RuntimeSelector {
            runtime: BoundedName::new("acp").unwrap(),
            profile: Some(BoundedName::new("codex").unwrap()),
        },
    }
}

struct NeverStartFactory(Arc<AtomicUsize>);

impl RuntimeFactory for NeverStartFactory {
    fn open(&mut self, _run: &ScheduledRun) -> Result<StartedRuntime, ContractError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(runtime_lost_error("unexpected_start", "Runtime must not be started twice").unwrap())
    }
}

struct UpdateFactory;

impl RuntimeFactory for UpdateFactory {
    fn open(&mut self, run: &ScheduledRun) -> Result<StartedRuntime, ContractError> {
        Ok(StartedRuntime {
            binding: runtime_binding(run),
            handle: Box::new(UpdateHandle),
        })
    }
}

struct UpdateHandle;

impl RuntimeHandle for UpdateHandle {
    fn begin(&mut self) -> Result<(), ContractError> {
        Ok(())
    }

    fn poll(&mut self) -> RuntimePoll {
        RuntimePoll::Update {
            sequence: 2,
            update: RuntimeUpdate::Progress {
                summary: BoundedText::new("progress").unwrap(),
            },
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
        Ok(())
    }
}

struct ShutdownProbeFactory(Arc<AtomicUsize>);

impl RuntimeFactory for ShutdownProbeFactory {
    fn open(&mut self, run: &ScheduledRun) -> Result<StartedRuntime, ContractError> {
        Ok(StartedRuntime {
            binding: runtime_binding(run),
            handle: Box::new(ShutdownProbeHandle(Arc::clone(&self.0))),
        })
    }
}

struct ShutdownProbeHandle(Arc<AtomicUsize>);

impl RuntimeHandle for ShutdownProbeHandle {
    fn begin(&mut self) -> Result<(), ContractError> {
        Ok(())
    }

    fn poll(&mut self) -> RuntimePoll {
        RuntimePoll::Pending
    }

    fn shutdown(&mut self, _reason: CancelReason) -> Result<(), ContractError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn resolve_provider_permission(
        &mut self,
        _permission: &RuntimePermissionRef,
        _decision: RuntimePermissionDecision,
    ) -> Result<(), ContractError> {
        Ok(())
    }
}

struct RejectingShutdownFactory {
    opens: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
}

impl RuntimeFactory for RejectingShutdownFactory {
    fn open(&mut self, run: &ScheduledRun) -> Result<StartedRuntime, ContractError> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        Ok(StartedRuntime {
            binding: runtime_binding(run),
            handle: Box::new(RejectingShutdownHandle(Arc::clone(&self.shutdowns))),
        })
    }
}

struct RejectingShutdownHandle(Arc<AtomicUsize>);

impl RuntimeHandle for RejectingShutdownHandle {
    fn begin(&mut self) -> Result<(), ContractError> {
        Ok(())
    }

    fn poll(&mut self) -> RuntimePoll {
        RuntimePoll::Pending
    }

    fn shutdown(&mut self, _reason: CancelReason) -> Result<(), ContractError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(runtime_lost_error(
            "runtime_shutdown_unacknowledged",
            "Runtime shutdown was not acknowledged",
        )
        .unwrap())
    }

    fn resolve_provider_permission(
        &mut self,
        _permission: &RuntimePermissionRef,
        _decision: RuntimePermissionDecision,
    ) -> Result<(), ContractError> {
        Ok(())
    }
}

fn runtime_binding(run: &ScheduledRun) -> RuntimeBindingRef {
    RuntimeBindingRef {
        binding_id: RuntimeBindingId::new(),
        task_id: run.task_id.clone(),
        run_id: run.run_id.clone(),
        agent_session_id: AgentSessionId::new(),
        runtime_instance_id: RuntimeInstanceId::new(),
        runtime_generation: run.lease_generation,
        external_session: ExternalRef {
            kind: ExternalRefKind::AcpSession,
            authority: BoundedName::new("scheduler-test").unwrap(),
            scope_digest: Digest::parse("a".repeat(64)).unwrap(),
            value: BoundedOpaque::new("session-hash").unwrap(),
        },
    }
}

struct PermissionFactory {
    decisions: Arc<Mutex<Vec<RuntimePermissionDecision>>>,
    expires_at_ms: u64,
    abandon_before_decision: bool,
    core_native: bool,
}

impl RuntimeFactory for PermissionFactory {
    fn open(&mut self, run: &ScheduledRun) -> Result<StartedRuntime, ContractError> {
        let binding = runtime_binding(run);
        let request = CapabilityRequest {
            request_id: RequestId::new(),
            task_id: run.task_id.clone(),
            run_id: run.run_id.clone(),
            actor: run.actor.clone(),
            target: run.target.clone(),
            operation: OperationDescriptor {
                namespace: BoundedName::new("process").unwrap(),
                name: BoundedName::new("spawn").unwrap(),
                arguments_digest: test_digest(),
            },
            operation_digest: test_digest(),
            requested_scope: CapabilityScope {
                resource: BoundedName::new("process").unwrap(),
                access: BoundedName::new("execute").unwrap(),
            },
            input_digest: test_digest(),
            expires_at_ms: self.expires_at_ms,
        };
        let permission = RuntimePermissionRef {
            binding_id: binding.binding_id.clone(),
            runtime_generation: binding.runtime_generation,
            event_sequence: 2,
            run_id: run.run_id.clone(),
            turn_id: TurnId::new(),
            tool_use_id: None,
            request_id: request.request_id.clone(),
            callback: (!self.core_native).then(|| {
                cosh_gateway_contracts::runtime::ProviderPermissionCallbackV2 {
                    provider_session_digest: test_digest(),
                    provider_request_id_digest: test_digest(),
                    provider_tool_call_id_digest: test_digest(),
                    ordered_option_set_digest: test_digest(),
                    callback_payload_digest: test_digest(),
                    normalized_operation_digest: request.operation_digest.clone(),
                }
            }),
            core_callback: self.core_native.then(|| {
                cosh_gateway_contracts::runtime::CorePermissionCallbackV1 {
                    private_request_id_digest: test_digest(),
                    private_tool_use_id_digest: test_digest(),
                    callback_payload_digest: test_digest(),
                    normalized_operation_digest: request.operation_digest.clone(),
                }
            }),
        };
        Ok(StartedRuntime {
            binding,
            handle: Box::new(PermissionHandle {
                permission,
                request,
                emitted: false,
                abandon_before_decision: self.abandon_before_decision,
                abandonment_emitted: false,
                terminal_emitted: false,
                decisions: Arc::clone(&self.decisions),
            }),
        })
    }
}

struct PermissionHandle {
    permission: RuntimePermissionRef,
    request: CapabilityRequest,
    emitted: bool,
    abandon_before_decision: bool,
    abandonment_emitted: bool,
    terminal_emitted: bool,
    decisions: Arc<Mutex<Vec<RuntimePermissionDecision>>>,
}

impl RuntimeHandle for PermissionHandle {
    fn begin(&mut self) -> Result<(), ContractError> {
        Ok(())
    }

    fn poll(&mut self) -> RuntimePoll {
        if self.emitted && self.abandon_before_decision && !self.abandonment_emitted {
            self.abandonment_emitted = true;
            RuntimePoll::PermissionAbandoned {
                sequence: self.permission.event_sequence + 1,
                permission: self.permission.clone(),
            }
        } else if self.abandonment_emitted && !self.terminal_emitted {
            self.terminal_emitted = true;
            RuntimePoll::Cancelled {
                cause: RuntimeCancellationCause::ProviderPermissionAbandoned {
                    permission: self.permission.clone(),
                },
            }
        } else if self.emitted
            && self.permission.callback.is_some()
            && !self.terminal_emitted
            && self
                .decisions
                .lock()
                .unwrap()
                .last()
                .is_some_and(|decision| matches!(decision, RuntimePermissionDecision::Deny { .. }))
        {
            self.terminal_emitted = true;
            RuntimePoll::Cancelled {
                cause: RuntimeCancellationCause::ProviderPermissionDenied {
                    permission: self.permission.clone(),
                },
            }
        } else if self.emitted {
            RuntimePoll::Pending
        } else {
            self.emitted = true;
            RuntimePoll::PermissionRequested {
                permission: self.permission.clone(),
                request: Box::new(self.request.clone()),
                summary: ToolSummary {
                    name: BoundedName::new("shell").unwrap(),
                    summary: BoundedText::new("Run the inspected shell command").unwrap(),
                },
            }
        }
    }

    fn shutdown(&mut self, _reason: CancelReason) -> Result<(), ContractError> {
        Ok(())
    }

    fn resolve_provider_permission(
        &mut self,
        permission: &RuntimePermissionRef,
        decision: RuntimePermissionDecision,
    ) -> Result<(), ContractError> {
        assert_eq!(permission, &self.permission);
        self.decisions.lock().unwrap().push(decision);
        Ok(())
    }
}

fn test_digest() -> Digest {
    Digest::parse("a".repeat(64)).unwrap()
}

fn pending_permission_with_barrier(
    policy: CheckpointPolicy,
    driver: ApprovalBarrierDriver,
    core_native: bool,
) -> (
    TempDir,
    TaskScheduler<PermissionFactory>,
    ActorId,
    Arc<Mutex<Vec<RuntimePermissionDecision>>>,
    ApprovalId,
    u64,
) {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator = TaskCoordinator::open(&database, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor, submission("approval-checkpoint-barrier"))
        .unwrap();
    let decisions = Arc::new(Mutex::new(Vec::new()));
    let mut scheduler = TaskScheduler::open(
        &database,
        Some(installation),
        BoundedOpaque::new("approval-checkpoint-worker").unwrap(),
        PermissionFactory {
            decisions: Arc::clone(&decisions),
            expires_at_ms: i64::MAX as u64,
            abandon_before_decision: false,
            core_native,
        },
    )
    .unwrap()
    .with_pre_runtime_checkpoint_driver(Box::new(driver));
    let now = now_ms().unwrap().saturating_add(1);
    assert!(matches!(
        scheduler.tick(now).unwrap(),
        SchedulerTick::Started(_)
    ));
    scheduler
        .active
        .as_mut()
        .unwrap()
        .scheduled
        .launch
        .checkpoint = policy;
    assert!(matches!(
        scheduler.tick(now + 1).unwrap(),
        SchedulerTick::Progressed(TaskView {
            state: TaskState::WaitingApproval,
            ..
        })
    ));
    let approval_id = scheduler
        .active
        .as_ref()
        .unwrap()
        .pending_permission
        .as_ref()
        .unwrap()
        .approval
        .approval_id
        .clone();
    let decision_at = scheduler
        .coordinator
        .store
        .load_approval_record(&approval_id)
        .unwrap()
        .updated_at_ms
        .saturating_add(1);
    (root, scheduler, actor, decisions, approval_id, decision_at)
}

fn approval_barrier_driver(
    prepare_result: ApprovalCheckpointPrepareResult,
    create_result: ApprovalCheckpointCreateResult,
    reconcile_result: ApprovalCheckpointReconcileResult,
) -> (
    ApprovalBarrierDriver,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let prepares = Arc::new(AtomicUsize::new(0));
    let creates = Arc::new(AtomicUsize::new(0));
    let reconciles = Arc::new(AtomicUsize::new(0));
    (
        ApprovalBarrierDriver {
            prepares: Arc::clone(&prepares),
            creates: Arc::clone(&creates),
            reconciles: Arc::clone(&reconciles),
            prepare_result,
            create_result,
            reconcile_result,
        },
        prepares,
        creates,
        reconciles,
    )
}

#[test]
fn approval_checkpoint_is_created_before_runtime_allow_dispatch() {
    let binding = approval_binding();
    let (driver, prepares, creates, reconciles) = approval_barrier_driver(
        ApprovalCheckpointPrepareResult::Prepared(binding),
        ApprovalCheckpointCreateResult::Created {
            evidence: approval_evidence(CheckpointId::new()),
        },
        ApprovalCheckpointReconcileResult::Unknown {
            reason: BoundedText::new("unexpected reconcile").unwrap(),
        },
    );
    let (_root, mut scheduler, actor, decisions, approval_id, now) =
        pending_permission_with_barrier(CheckpointPolicy::On, driver, false);
    scheduler
        .resolve_approval(
            &actor,
            IdempotencyKey::new("approval-checkpoint-created").unwrap(),
            &approval_id,
            ApprovalDecision::Approve,
            now,
        )
        .unwrap();
    assert_eq!(prepares.load(Ordering::Relaxed), 1);
    assert_eq!(creates.load(Ordering::Relaxed), 1);
    assert_eq!(reconciles.load(Ordering::Relaxed), 0);
    assert_eq!(
        decisions.lock().unwrap().as_slice(),
        [RuntimePermissionDecision::ProviderNativeAllowOnce]
    );
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_approval_checkpoint_record(&approval_id)
            .unwrap()
            .state,
        ApprovalCheckpointState::Created
    );
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_provider_permission_dispatch_record(&approval_id)
            .unwrap()
            .state,
        ProviderPermissionDispatchState::Written
    );
}

#[test]
fn core_approval_checkpoint_is_created_before_runtime_native_allow_dispatch() {
    let binding = approval_binding();
    let (driver, prepares, creates, reconciles) = approval_barrier_driver(
        ApprovalCheckpointPrepareResult::Prepared(binding),
        ApprovalCheckpointCreateResult::Created {
            evidence: approval_evidence(CheckpointId::new()),
        },
        ApprovalCheckpointReconcileResult::Unknown {
            reason: BoundedText::new("unexpected reconcile").unwrap(),
        },
    );
    let (_root, mut scheduler, actor, decisions, approval_id, now) =
        pending_permission_with_barrier(CheckpointPolicy::On, driver, true);

    scheduler
        .resolve_approval(
            &actor,
            IdempotencyKey::new("core-approval-checkpoint-created").unwrap(),
            &approval_id,
            ApprovalDecision::Approve,
            now,
        )
        .unwrap();

    assert_eq!(prepares.load(Ordering::Relaxed), 1);
    assert_eq!(creates.load(Ordering::Relaxed), 1);
    assert_eq!(reconciles.load(Ordering::Relaxed), 0);
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_approval_checkpoint_record(&approval_id)
            .unwrap()
            .state,
        ApprovalCheckpointState::Created
    );
    assert_eq!(
        decisions.lock().unwrap().as_slice(),
        [RuntimePermissionDecision::RuntimeNativeAllowOnce]
    );
}

#[test]
fn approval_checkpoint_off_skips_driver_and_auto_skips_unavailable() {
    for (policy, expected_prepares, expected_state) in [
        (CheckpointPolicy::Off, 0, None),
        (
            CheckpointPolicy::Auto,
            1,
            Some(ApprovalCheckpointState::Skipped),
        ),
    ] {
        let (driver, prepares, creates, _) = approval_barrier_driver(
            ApprovalCheckpointPrepareResult::Unavailable {
                reason: BoundedText::new("checkpoint unavailable").unwrap(),
            },
            ApprovalCheckpointCreateResult::Unavailable {
                reason: BoundedText::new("checkpoint unavailable").unwrap(),
            },
            ApprovalCheckpointReconcileResult::NotApplied,
        );
        let (_root, mut scheduler, actor, decisions, approval_id, now) =
            pending_permission_with_barrier(policy, driver, false);
        scheduler
            .resolve_approval(
                &actor,
                IdempotencyKey::new(format!("approval-checkpoint-{policy:?}")).unwrap(),
                &approval_id,
                ApprovalDecision::Approve,
                now,
            )
            .unwrap();
        assert_eq!(prepares.load(Ordering::Relaxed), expected_prepares);
        assert_eq!(creates.load(Ordering::Relaxed), 0);
        assert_eq!(decisions.lock().unwrap().len(), 1);
        match expected_state {
            Some(state) => assert_eq!(
                scheduler
                    .coordinator
                    .store
                    .load_approval_checkpoint_record(&approval_id)
                    .unwrap()
                    .state,
                state
            ),
            None => assert!(matches!(
                scheduler
                    .coordinator
                    .store
                    .load_approval_checkpoint_record(&approval_id),
                Err(StoreError::LedgerNotFound { .. })
            )),
        }
    }
}

#[test]
fn approval_checkpoint_on_unavailable_keeps_approval_pending_without_dispatch() {
    let (driver, _, creates, _) = approval_barrier_driver(
        ApprovalCheckpointPrepareResult::Unavailable {
            reason: BoundedText::new("checkpoint unavailable").unwrap(),
        },
        ApprovalCheckpointCreateResult::Unavailable {
            reason: BoundedText::new("checkpoint unavailable").unwrap(),
        },
        ApprovalCheckpointReconcileResult::NotApplied,
    );
    let (_root, mut scheduler, actor, decisions, approval_id, now) =
        pending_permission_with_barrier(CheckpointPolicy::On, driver, false);
    assert!(matches!(
        scheduler.resolve_approval(
            &actor,
            IdempotencyKey::new("approval-checkpoint-on-failed").unwrap(),
            &approval_id,
            ApprovalDecision::Approve,
            now,
        ),
        Err(GatewayDaemonError::Protocol(message))
            if message == "approval checkpoint barrier did not authorize Runtime Permission"
    ));
    assert_eq!(creates.load(Ordering::Relaxed), 0);
    assert!(decisions.lock().unwrap().is_empty());
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_approval_record(&approval_id)
            .unwrap()
            .state,
        ApprovalState::Pending
    );
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_approval_checkpoint_record(&approval_id)
            .unwrap()
            .state,
        ApprovalCheckpointState::Failed
    );
    assert!(scheduler
        .coordinator
        .store
        .load_provider_permission_dispatch_record(&approval_id)
        .is_err());
}

#[test]
fn started_approval_checkpoint_replay_reconciles_without_create() {
    let binding = approval_binding();
    let (driver, prepares, creates, reconciles) = approval_barrier_driver(
        ApprovalCheckpointPrepareResult::Prepared(binding.clone()),
        ApprovalCheckpointCreateResult::PossiblyApplied {
            error: runtime_lost_error("unexpected_create", "unexpected create").unwrap(),
        },
        ApprovalCheckpointReconcileResult::Created {
            evidence: approval_evidence(CheckpointId::new()),
        },
    );
    let (_root, mut scheduler, actor, decisions, approval_id, now) =
        pending_permission_with_barrier(CheckpointPolicy::On, driver, false);
    let active = scheduler.active.as_ref().unwrap();
    let fence = RuntimeExecutionFence {
        binding_id: active.binding.binding_id.clone(),
        runtime_generation: active.binding.runtime_generation,
        lease_generation: active.lease.generation,
        lease_revision: active.lease.revision,
    };
    let checkpoint_id = CheckpointId::new();
    scheduler
        .coordinator
        .store
        .record_approval_checkpoint_intent(
            &approval_id,
            &active.scheduled.task_id,
            &active.scheduled.run_id,
            &checkpoint_id,
            CheckpointPolicy::On,
            &fence,
            now,
        )
        .unwrap();
    assert!(scheduler
        .coordinator
        .store
        .start_approval_checkpoint(&approval_id, &binding, now)
        .unwrap());
    scheduler
        .resolve_approval(
            &actor,
            IdempotencyKey::new("approval-checkpoint-reconcile").unwrap(),
            &approval_id,
            ApprovalDecision::Approve,
            now + 1,
        )
        .unwrap();
    assert_eq!(prepares.load(Ordering::Relaxed), 0);
    assert_eq!(creates.load(Ordering::Relaxed), 0);
    assert_eq!(reconciles.load(Ordering::Relaxed), 1);
    assert_eq!(decisions.lock().unwrap().len(), 1);
}

#[test]
fn started_not_applied_checkpoint_skips_auto_but_blocks_on() {
    for (policy, expected_state, expected_dispatches) in [
        (CheckpointPolicy::Auto, ApprovalCheckpointState::Skipped, 1),
        (CheckpointPolicy::On, ApprovalCheckpointState::Failed, 0),
    ] {
        let binding = approval_binding();
        let (driver, prepares, creates, reconciles) = approval_barrier_driver(
            ApprovalCheckpointPrepareResult::Prepared(binding.clone()),
            ApprovalCheckpointCreateResult::PossiblyApplied {
                error: runtime_lost_error("unexpected_create", "unexpected create").unwrap(),
            },
            ApprovalCheckpointReconcileResult::NotApplied,
        );
        let (_root, mut scheduler, actor, decisions, approval_id, now) =
            pending_permission_with_barrier(policy, driver, false);
        let active = scheduler.active.as_ref().unwrap();
        let fence = RuntimeExecutionFence {
            binding_id: active.binding.binding_id.clone(),
            runtime_generation: active.binding.runtime_generation,
            lease_generation: active.lease.generation,
            lease_revision: active.lease.revision,
        };
        scheduler
            .coordinator
            .store
            .record_approval_checkpoint_intent(
                &approval_id,
                &active.scheduled.task_id,
                &active.scheduled.run_id,
                &CheckpointId::new(),
                policy,
                &fence,
                now,
            )
            .unwrap();
        assert!(scheduler
            .coordinator
            .store
            .start_approval_checkpoint(&approval_id, &binding, now)
            .unwrap());

        let resolution = scheduler.resolve_approval(
            &actor,
            IdempotencyKey::new(format!("approval-checkpoint-not-applied-{policy:?}")).unwrap(),
            &approval_id,
            ApprovalDecision::Approve,
            now + 1,
        );
        if policy == CheckpointPolicy::Auto {
            assert!(resolution.is_ok());
        } else {
            assert!(matches!(
                resolution,
                Err(GatewayDaemonError::Protocol(message))
                    if message
                        == "approval checkpoint barrier did not authorize Runtime Permission"
            ));
        }
        assert_eq!(prepares.load(Ordering::Relaxed), 0);
        assert_eq!(creates.load(Ordering::Relaxed), 0);
        assert_eq!(reconciles.load(Ordering::Relaxed), 1);
        assert_eq!(decisions.lock().unwrap().len(), expected_dispatches);
        assert_eq!(
            scheduler
                .coordinator
                .store
                .load_approval_checkpoint_record(&approval_id)
                .unwrap()
                .state,
            expected_state
        );
    }
}

#[test]
fn invalid_start_intents_are_rejected_before_outbox_claim() {
    for case in ["profile-drift", "v2-target", "v2-runtime", "future-schema"] {
        let root = TempDir::new().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let database_path = root.path().join("gateway.db");
        let installation = InstallationId::new();
        let mut coordinator =
            TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
        let actor = actor_id_for_uid(&installation, 1000).unwrap();
        coordinator.submit(&actor, submission(case)).unwrap();
        let now = now_ms().unwrap().saturating_add(1);
        let candidate = coordinator
            .store
            .peek_ready_outbox(&runtime_start_delivery_kind(), now)
            .unwrap()
            .unwrap();
        assert_eq!(candidate.attempt, 0, "case {case}");
        let mut payload = candidate.payload;
        match case {
            "profile-drift" => {
                payload["capability_profile"]["manifest_digest"] =
                    serde_json::json!("b".repeat(64));
            }
            "v2-target" => {
                payload["schema_version"] = serde_json::json!(2);
                payload
                    .as_object_mut()
                    .unwrap()
                    .remove("capability_profile");
                payload.as_object_mut().unwrap().remove("launch");
                payload.as_object_mut().unwrap().remove("baseline_id");
                payload["target"]["identifier"] = serde_json::json!("another-target");
            }
            "v2-runtime" => {
                payload["schema_version"] = serde_json::json!(2);
                payload
                    .as_object_mut()
                    .unwrap()
                    .remove("capability_profile");
                payload.as_object_mut().unwrap().remove("launch");
                payload.as_object_mut().unwrap().remove("baseline_id");
                payload["runtime"] = serde_json::json!({"runtime": "acp", "profile": "codex"});
            }
            "future-schema" => payload["schema_version"] = serde_json::json!(5),
            _ => unreachable!(),
        }
        coordinator
            .store
            .replace_outbox_payload_for_test(&candidate.delivery_id, &payload)
            .unwrap();
        drop(coordinator);

        let starts = Arc::new(AtomicUsize::new(0));
        let mut scheduler = TaskScheduler::open(
            &database_path,
            Some(installation),
            BoundedOpaque::new(format!("invalid-start-{case}")).unwrap(),
            NeverStartFactory(Arc::clone(&starts)),
        )
        .unwrap();
        assert!(
            matches!(scheduler.tick(now), Err(GatewayDaemonError::Protocol(_))),
            "case {case}"
        );
        let candidate = scheduler
            .coordinator
            .store
            .peek_ready_outbox(&runtime_start_delivery_kind(), now)
            .unwrap()
            .unwrap();
        assert_eq!(candidate.attempt, 0, "case {case}");
        assert_eq!(starts.load(Ordering::Relaxed), 0, "case {case}");
    }
}

#[test]
fn exact_task_only_v2_intent_maps_to_current_profile() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor, submission("compatible-v2-start"))
        .unwrap();
    let now = now_ms().unwrap().saturating_add(1);
    let candidate = coordinator
        .store
        .peek_ready_outbox(&runtime_start_delivery_kind(), now)
        .unwrap()
        .unwrap();
    let mut payload = candidate.payload;
    payload["schema_version"] = serde_json::json!(2);
    payload
        .as_object_mut()
        .unwrap()
        .remove("capability_profile");
    payload.as_object_mut().unwrap().remove("launch");
    payload.as_object_mut().unwrap().remove("baseline_id");
    coordinator
        .store
        .replace_outbox_payload_for_test(&candidate.delivery_id, &payload)
        .unwrap();
    drop(coordinator);

    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("compatible-v2-worker").unwrap(),
        UpdateFactory,
    )
    .unwrap();
    assert!(matches!(
        scheduler.tick(now).unwrap(),
        SchedulerTick::Started(_)
    ));
    assert_eq!(
        scheduler
            .active
            .as_ref()
            .unwrap()
            .scheduled
            .capability_profile,
        GatewayCapabilityProfile::task_only_v1().identity()
    );
}

#[test]
fn exact_checkpoint_intent_starts_only_under_checkpoint_profile() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let profile = GatewayCapabilityProfile::workspace_checkpoint_v1();
    let mut coordinator = TaskCoordinator::open_for_capability_profile(
        &database_path,
        Some(installation.clone()),
        profile,
    )
    .unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(
            &actor,
            SubmitTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("checkpoint-start").unwrap(),
                intent: BoundedText::new("checkpoint workspace").unwrap(),
                target: profile.governed_target(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("core").unwrap(),
                    profile: Some(BoundedName::new("gateway-checkpoint-v1").unwrap()),
                },
            },
        )
        .unwrap();
    drop(coordinator);

    let mut scheduler = TaskScheduler::open_for_capability_profile(
        &database_path,
        Some(installation),
        BoundedOpaque::new("checkpoint-worker").unwrap(),
        profile,
        UpdateFactory,
    )
    .unwrap();
    assert!(matches!(
        scheduler.tick(now_ms().unwrap().saturating_add(1)).unwrap(),
        SchedulerTick::Started(_)
    ));
    assert_eq!(
        scheduler
            .active
            .as_ref()
            .unwrap()
            .scheduled
            .capability_profile,
        profile.identity()
    );
}

#[test]
fn stale_scheduler_generation_cannot_settle_taken_over_run() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut first = TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    first.submit(&actor, submission("lease-fence")).unwrap();
    let claimed_at = now_ms().unwrap().saturating_add(1);
    let stale = match first
        .claim_runtime_start(
            &BoundedOpaque::new("worker-a").unwrap(),
            claimed_at,
            claimed_at + 10,
        )
        .unwrap()
    {
        RuntimeStartClaim::Claimed { lease, .. } => lease,
        RuntimeStartClaim::Empty | RuntimeStartClaim::Recovered(_) => {
            panic!("first scheduler must claim the queued Run")
        }
    };

    let starts = Arc::new(AtomicUsize::new(0));
    let mut second = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-b").unwrap(),
        NeverStartFactory(Arc::clone(&starts)),
    )
    .unwrap();
    assert!(matches!(
        second.tick(claimed_at + 10).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Failed,
            ..
        })
    ));
    assert_eq!(starts.load(Ordering::Relaxed), 0);

    assert!(matches!(
        first.settle_succeeded(&stale, claimed_at + 11),
        Err(GatewayDaemonError::Store(
            StoreError::GenerationFenced { .. }
        ))
    ));
}

#[test]
fn runtime_event_sequence_overflow_fails_before_commit() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    let task = coordinator
        .submit(&actor, submission("sequence-overflow"))
        .unwrap();
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-sequence").unwrap(),
        UpdateFactory,
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();
    scheduler.active.as_mut().unwrap().next_event_sequence = u64::MAX;

    assert!(matches!(
        scheduler.tick(started_at + 1),
        Err(GatewayDaemonError::Protocol(message))
            if message.contains("sequence exceeds")
    ));
    assert_eq!(coordinator.get(&actor, &task.task_id).unwrap().revision, 4);
}

#[test]
fn scheduler_rejects_a_lease_that_cannot_cover_one_runtime_operation() {
    let error = TaskSchedulerConfig {
        lease_duration: Duration::from_secs(100),
        lease_renewal_margin: Duration::from_secs(30),
        runtime_operation_timeout: Duration::from_secs(70),
    }
    .validate()
    .unwrap_err();

    assert!(matches!(
        error,
        GatewayDaemonError::Protocol(message) if message.contains("must exceed")
    ));
}

#[test]
fn shutdown_preserves_an_already_observed_terminal_outcome() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor, submission("shutdown-terminal"))
        .unwrap();
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-shutdown-terminal").unwrap(),
        ShutdownProbeFactory(Arc::clone(&shutdowns)),
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();
    scheduler.active.as_mut().unwrap().terminal = Some(TerminalOutcome::Succeeded);

    assert!(matches!(
        scheduler
            .shutdown(now_ms().unwrap().saturating_add(1))
            .unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Succeeded,
            ..
        })
    ));
    assert_eq!(shutdowns.load(Ordering::Relaxed), 0);
}

#[test]
fn shutdown_preserves_an_earlier_abort_failure() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor, submission("shutdown-abort"))
        .unwrap();
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-shutdown-abort").unwrap(),
        ShutdownProbeFactory(Arc::clone(&shutdowns)),
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();
    scheduler.active.as_mut().unwrap().abort_error =
        Some(runtime_lost_error("earlier_failure", "The Runtime had already failed").unwrap());

    assert!(matches!(
        scheduler
            .shutdown(now_ms().unwrap().saturating_add(1))
            .unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Failed,
            ..
        })
    ));
    assert_eq!(shutdowns.load(Ordering::Relaxed), 1);
}

#[test]
fn unacknowledged_shutdown_fails_durably_and_restart_does_not_replay() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    let task = coordinator
        .submit(&actor, submission("shutdown-unacknowledged"))
        .unwrap();
    let opens = Arc::new(AtomicUsize::new(0));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation.clone()),
        BoundedOpaque::new("worker-shutdown-unacknowledged").unwrap(),
        RejectingShutdownFactory {
            opens: Arc::clone(&opens),
            shutdowns: Arc::clone(&shutdowns),
        },
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();

    assert!(matches!(
        scheduler.shutdown(started_at + 1).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Failed,
            ..
        })
    ));
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    assert_eq!(shutdowns.load(Ordering::Relaxed), 1);
    drop(scheduler);

    let restart_opens = Arc::new(AtomicUsize::new(0));
    let mut restarted = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-after-shutdown-failure").unwrap(),
        NeverStartFactory(Arc::clone(&restart_opens)),
    )
    .unwrap();
    assert_eq!(restarted.tick(started_at + 2).unwrap(), SchedulerTick::Idle);
    assert_eq!(restart_opens.load(Ordering::Relaxed), 0);
    assert_eq!(
        restarted
            .coordinator
            .get(&actor, &task.task_id)
            .unwrap()
            .state,
        TaskState::Failed
    );
}

#[test]
fn provider_approval_is_dispatched_once_and_delivered_replay_is_read_only() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor, submission("provider-approval"))
        .unwrap();
    let decisions = Arc::new(Mutex::new(Vec::new()));
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-approval").unwrap(),
        PermissionFactory {
            decisions: Arc::clone(&decisions),
            expires_at_ms: i64::MAX as u64,
            abandon_before_decision: false,
            core_native: false,
        },
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
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
        .pending_permission
        .as_ref()
        .unwrap()
        .approval
        .approval_id
        .clone();
    let decision_at = scheduler
        .coordinator
        .store
        .load_approval_record(&approval_id)
        .unwrap()
        .updated_at_ms
        .saturating_add(1);

    assert!(matches!(
        scheduler.resolve_approval_for_task(
            &actor,
            IdempotencyKey::new("wrong-task").unwrap(),
            &TaskId::new(),
            &approval_id,
            ApprovalDecision::Approve,
            decision_at,
        ),
        Err(GatewayDaemonError::Unauthorized)
    ));
    assert!(decisions.lock().unwrap().is_empty());

    assert!(matches!(
        scheduler.resolve_approval(
            &ActorId::new(),
            IdempotencyKey::new("wrong-actor").unwrap(),
            &approval_id,
            ApprovalDecision::Approve,
            started_at + 2,
        ),
        Err(GatewayDaemonError::Unauthorized)
    ));
    assert!(scheduler
        .resolve_approval(
            &actor,
            IdempotencyKey::new("wrong-approval").unwrap(),
            &ApprovalId::new(),
            ApprovalDecision::Approve,
            started_at + 2,
        )
        .is_err());
    assert!(decisions.lock().unwrap().is_empty());

    let replay_key = IdempotencyKey::new("resolve-provider-once").unwrap();
    assert!(matches!(
        scheduler
            .resolve_approval(
                &actor,
                replay_key.clone(),
                &approval_id,
                ApprovalDecision::Approve,
                decision_at,
            )
            .unwrap(),
        SchedulerTick::Progressed(TaskView {
            state: TaskState::Running,
            ..
        })
    ));
    assert_eq!(decisions.lock().unwrap().len(), 1);
    assert!(matches!(
        decisions.lock().unwrap().as_slice(),
        [RuntimePermissionDecision::ProviderNativeAllowOnce]
    ));
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_provider_permission_dispatch_record(&approval_id)
            .unwrap()
            .state,
        ProviderPermissionDispatchState::Written
    );

    let approved_task_id = scheduler
        .coordinator
        .store
        .load_approval_record(&approval_id)
        .unwrap()
        .task_id;
    scheduler.active.take();
    coordinator
        .submit(&actor, submission("another-active-run"))
        .unwrap();
    scheduler.tick(now_ms().unwrap().saturating_add(1)).unwrap();
    let replayed = scheduler
        .resolve_approval(
            &actor,
            replay_key,
            &approval_id,
            ApprovalDecision::Approve,
            now_ms().unwrap().saturating_add(1),
        )
        .unwrap();
    assert!(matches!(
        replayed,
        SchedulerTick::Progressed(TaskView {
            task_id,
            state: TaskState::Running,
            ..
        }) if task_id == approved_task_id
    ));
    assert_eq!(decisions.lock().unwrap().len(), 1);
    assert!(scheduler
        .resolve_approval(
            &actor,
            IdempotencyKey::new("change-delivered-decision").unwrap(),
            &approval_id,
            ApprovalDecision::Deny,
            now_ms().unwrap().saturating_add(1),
        )
        .is_err());
    assert_eq!(decisions.lock().unwrap().len(), 1);
}

#[test]
fn denied_provider_permission_accepts_only_its_matching_cancelled_terminal() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    let task = coordinator
        .submit(&actor, submission("provider-denial-cancel"))
        .unwrap();
    let decisions = Arc::new(Mutex::new(Vec::new()));
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-denial-cancel").unwrap(),
        PermissionFactory {
            decisions: Arc::clone(&decisions),
            expires_at_ms: i64::MAX as u64,
            abandon_before_decision: false,
            core_native: false,
        },
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();
    scheduler.tick(started_at + 1).unwrap();
    let approval_id = scheduler
        .active
        .as_ref()
        .unwrap()
        .pending_permission
        .as_ref()
        .unwrap()
        .approval
        .approval_id
        .clone();
    let decision_at = scheduler
        .coordinator
        .store
        .load_approval_record(&approval_id)
        .unwrap()
        .updated_at_ms
        .saturating_add(1);

    assert!(matches!(
        scheduler
            .resolve_approval(
                &actor,
                IdempotencyKey::new("deny-provider-once").unwrap(),
                &approval_id,
                ApprovalDecision::Deny,
                decision_at,
            )
            .unwrap(),
        SchedulerTick::Progressed(TaskView {
            state: TaskState::Running,
            ..
        })
    ));
    assert!(matches!(
        scheduler.tick(decision_at.saturating_add(1)).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Failed,
            ..
        })
    ));
    assert!(matches!(
        decisions.lock().unwrap().as_slice(),
        [RuntimePermissionDecision::Deny { .. }]
    ));
    let (events, _) = scheduler
        .coordinator
        .store
        .load_task_events_for_owner(&task.task_id, &actor, None, 64)
        .unwrap();
    let failure = events.into_iter().find_map(|event| match event.event {
        TaskEvent::TaskFailed { error } => Some(error),
        _ => None,
    });
    assert_eq!(
        failure.expect("denial must settle durably").code.as_str(),
        "provider_permission_denied"
    );
}

#[test]
fn denied_core_permission_keeps_the_runtime_active_without_expected_cancellation() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor, submission("core-denial-continues"))
        .unwrap();
    let decisions = Arc::new(Mutex::new(Vec::new()));
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-core-denial").unwrap(),
        PermissionFactory {
            decisions: Arc::clone(&decisions),
            expires_at_ms: i64::MAX as u64,
            abandon_before_decision: false,
            core_native: true,
        },
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();
    scheduler.tick(started_at + 1).unwrap();
    let approval_id = scheduler
        .active
        .as_ref()
        .unwrap()
        .pending_permission
        .as_ref()
        .unwrap()
        .approval
        .approval_id
        .clone();
    let decision_at = scheduler
        .coordinator
        .store
        .load_approval_record(&approval_id)
        .unwrap()
        .updated_at_ms
        .saturating_add(1);

    assert!(matches!(
        scheduler
            .resolve_approval(
                &actor,
                IdempotencyKey::new("deny-core-once").unwrap(),
                &approval_id,
                ApprovalDecision::Deny,
                decision_at,
            )
            .unwrap(),
        SchedulerTick::Progressed(TaskView {
            state: TaskState::Running,
            ..
        })
    ));
    assert!(scheduler
        .active
        .as_ref()
        .unwrap()
        .expected_provider_terminal
        .is_none());
    assert!(matches!(
        decisions.lock().unwrap().as_slice(),
        [RuntimePermissionDecision::Deny { safe_message, .. }]
            if safe_message.as_str() == "The Runtime operation was denied"
    ));
    assert!(!matches!(
        scheduler.tick(decision_at.saturating_add(1)).unwrap(),
        SchedulerTick::Settled(_)
    ));
    assert!(scheduler.active.is_some());
}

#[test]
fn provider_cancel_atomically_abandons_pending_approval_before_terminal() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    let task = coordinator
        .submit(&actor, submission("provider-abandons-permission"))
        .unwrap();
    let decisions = Arc::new(Mutex::new(Vec::new()));
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-provider-abandon").unwrap(),
        PermissionFactory {
            decisions: Arc::clone(&decisions),
            expires_at_ms: i64::MAX as u64,
            abandon_before_decision: true,
            core_native: false,
        },
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();
    scheduler.tick(started_at + 1).unwrap();
    let approval_id = scheduler
        .active
        .as_ref()
        .unwrap()
        .pending_permission
        .as_ref()
        .unwrap()
        .approval
        .approval_id
        .clone();

    assert!(matches!(
        scheduler.tick(now_ms().unwrap().saturating_add(1)).unwrap(),
        SchedulerTick::Progressed(TaskView {
            state: TaskState::Running,
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
        ApprovalState::Cancelled
    );
    assert!(decisions.lock().unwrap().is_empty());
    assert!(scheduler
        .coordinator
        .store
        .load_provider_permission_dispatch_record(&approval_id)
        .is_err());
    assert!(matches!(
        scheduler.tick(now_ms().unwrap().saturating_add(1)).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Failed,
            ..
        })
    ));

    let (events, _) = scheduler
        .coordinator
        .store
        .load_task_events_for_owner(&task.task_id, &actor, None, 64)
        .unwrap();
    let requested = events
        .iter()
        .position(|event| matches!(event.event, TaskEvent::ApprovalRequested { .. }))
        .unwrap();
    let abandoned = events
        .iter()
        .position(|event| matches!(event.event, TaskEvent::ApprovalAbandoned { .. }))
        .unwrap();
    let failed = events
        .iter()
        .position(|event| matches!(event.event, TaskEvent::TaskFailed { .. }))
        .unwrap();
    assert!(requested < abandoned && abandoned < failed);
}

#[test]
fn delegated_acp_profile_durably_allows_provider_callbacks_once() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let profile = GatewayCapabilityProfile::delegated_acp_v1();
    let mut coordinator = TaskCoordinator::open_for_capability_profile(
        &database_path,
        Some(installation.clone()),
        profile,
    )
    .unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor, delegated_submission("delegated-provider-approval"))
        .unwrap();
    let decisions = Arc::new(Mutex::new(Vec::new()));
    let mut scheduler = TaskScheduler::open_for_capability_profile(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-delegated-approval").unwrap(),
        profile,
        PermissionFactory {
            decisions: Arc::clone(&decisions),
            expires_at_ms: i64::MAX as u64,
            abandon_before_decision: false,
            core_native: false,
        },
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);

    assert!(matches!(
        scheduler.tick(started_at).unwrap(),
        SchedulerTick::Started(_)
    ));
    assert!(matches!(
        scheduler.tick(started_at + 1).unwrap(),
        SchedulerTick::Progressed(TaskView {
            state: TaskState::Running,
            ..
        })
    ));
    assert!(scheduler
        .active
        .as_ref()
        .unwrap()
        .pending_permission
        .is_none());
    assert_eq!(
        decisions.lock().unwrap().as_slice(),
        [RuntimePermissionDecision::ProviderNativeAllowOnce]
    );
    let task_id = scheduler.active.as_ref().unwrap().scheduled.task_id.clone();
    let (events, _) = scheduler
        .coordinator
        .store
        .load_task_events_for_owner(&task_id, &actor, None, 64)
        .unwrap();
    let approval_id = events
        .into_iter()
        .find_map(|event| match event.event {
            TaskEvent::ApprovalRequested { approval } => Some(approval.approval_id),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_provider_permission_dispatch_record(&approval_id)
            .unwrap()
            .state,
        ProviderPermissionDispatchState::Written
    );
}

#[test]
fn delegated_acp_task_runs_to_a_durable_result_through_the_real_bridge() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let workspace_path = root.path().join("workspace");
    fs::create_dir(&workspace_path).unwrap();
    let response_path = root.path().join("permission-response.json");
    let adapter_path = root.path().join("codex-acp");
    fs::write(
        &adapter_path,
        format!(
            r#"#!/bin/sh
step=0
while IFS= read -r line; do
    step=$((step + 1))
    case "$step" in
        1) printf '%s\n' '{{"jsonrpc":"2.0","id":"cosh-acp-1","result":{{"protocolVersion":1,"agentCapabilities":{{}},"agentInfo":{{"name":"@agentclientprotocol/codex-acp","title":"Codex","version":"1.6.2"}},"_meta":{{"jetbrains":{{"air":{{"version":1,"capabilities":["sessionFailure","agentFileChangeReport"]}}}}}}}}}}' ;;
        2) printf '%s\n' '{{"jsonrpc":"2.0","id":"cosh-acp-2","result":{{"sessionId":"delegated-session"}}}}' ;;
        3)
            printf '%s\n' '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"delegated-session","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"started "}}}}}}}}'
            printf '%s\n' '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"delegated-session","update":{{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","title":"Read workspace","status":"in_progress"}}}}}}'
            printf '%s\n' '{{"jsonrpc":"2.0","id":41,"method":"session/request_permission","params":{{"sessionId":"delegated-session","toolCall":{{"toolCallId":"tool-1","status":"pending"}},"options":[{{"optionId":"allow","name":"Allow once","kind":"allow_once"}},{{"optionId":"always","name":"Always","kind":"allow_always"}}]}}}}' ;;
        4)
            printf '%s\n' "$line" > '{}'
            printf '%s\n' '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"delegated-session","update":{{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"completed"}}}}}}'
            i=0
            while [ "$i" -lt 64 ]; do
                printf '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"delegated-session","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"chunk-%s|"}}}}}}}}\n' "$i"
                i=$((i + 1))
            done
            printf '%s\n' '{{"jsonrpc":"2.0","id":"cosh-acp-3","result":{{"stopReason":"end_turn"}}}}' ;;
    esac
done
"#,
            response_path.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&adapter_path, fs::Permissions::from_mode(0o700)).unwrap();

    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let profile = GatewayCapabilityProfile::delegated_acp_v1();
    let target = profile.governed_target();
    let actor_resolver = LocalOsActorResolver::new(installation.clone(), 1000);
    let actor_ref = actor_resolver.actor_ref().clone();
    let actor = actor_ref.actor_id.clone();
    let workspace = TrustedWorkspaceResolver::new(target, &workspace_path).unwrap();
    let workspace_ref = workspace.workspace_ref().clone();
    let factory = InstalledAcpRuntimePortFactory::new(
        installation.clone(),
        actor_resolver,
        workspace,
        BTreeMap::from([(AcpRuntimeProfileId::Codex, adapter_path)]),
        BTreeMap::new(),
    )
    .unwrap();
    let mut coordinator = TaskCoordinator::open_for_capability_profile(
        &database_path,
        Some(installation.clone()),
        profile,
    )
    .unwrap();
    let task = coordinator
        .submit_admitted(
            &actor_ref,
            &workspace_ref,
            delegated_submission("real-acp-delegation"),
        )
        .unwrap();
    let mut scheduler = TaskScheduler::open_for_capability_profile(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-real-acp").unwrap(),
        profile,
        ScheduledAgentRuntimeFactory::new(factory),
    )
    .unwrap();

    let mut settled = None;
    for _ in 0..200 {
        let tick = scheduler.tick(now_ms().unwrap()).unwrap();
        if let SchedulerTick::Settled(view) = tick {
            settled = Some(view);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let settled = settled.expect("delegated ACP Task must settle");
    if settled.state != TaskState::Succeeded {
        let (events, _) = scheduler
            .coordinator
            .store
            .load_task_events_for_owner(&task.task_id, &actor, None, 64)
            .unwrap();
        panic!("delegated ACP Task failed: {events:#?}");
    }
    assert_eq!(settled.state, TaskState::Succeeded);
    let permission_response = fs::read_to_string(&response_path).unwrap();
    assert!(permission_response.contains("\"optionId\":\"allow\""));
    assert!(!permission_response.contains("always"));

    let mut events = Vec::new();
    let mut after_revision = None;
    loop {
        let (page, revision) = scheduler
            .coordinator
            .store
            .load_task_events_for_owner(&task.task_id, &actor, after_revision, 64)
            .unwrap();
        let Some(last) = page.last() else {
            break;
        };
        after_revision = Some(last.revision);
        events.extend(page);
        if after_revision == Some(revision) {
            break;
        }
    }
    let progress = events
        .iter()
        .filter_map(|event| match &event.event {
            TaskEvent::RuntimeEventRecorded {
                update: RuntimeUpdate::Progress { summary },
                ..
            } => Some(summary.as_str()),
            _ => None,
        })
        .collect::<String>();
    let chunks = (0..64)
        .map(|index| format!("chunk-{index}|"))
        .collect::<String>();
    assert_eq!(progress, format!("started {chunks}"));
    assert!(!events.iter().any(|event| matches!(
        event.event,
        TaskEvent::RunFailed { .. } | TaskEvent::TaskFailed { .. }
    )));
    let requested = events
        .iter()
        .position(|event| matches!(event.event, TaskEvent::ApprovalRequested { .. }))
        .expect("approval request must be durable before response dispatch");
    let resolved = events
        .iter()
        .position(|event| matches!(event.event, TaskEvent::ApprovalResolved { .. }))
        .expect("automatic allow-once resolution must be durable");
    let succeeded = events
        .iter()
        .position(|event| matches!(event.event, TaskEvent::TaskSucceeded))
        .expect("Task success must be durable");
    assert!(requested < resolved && resolved < succeeded);
    let approval_id = events
        .iter()
        .find_map(|event| match &event.event {
            TaskEvent::ApprovalRequested { approval } => Some(approval.approval_id.clone()),
            _ => None,
        })
        .expect("provider approval identity must be durable");
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_provider_permission_dispatch_record(&approval_id)
            .unwrap()
            .state,
        ProviderPermissionDispatchState::Written
    );
    let response_before_replay = fs::read(&response_path).unwrap();
    let replayed = scheduler
        .resolve_approval(
            &actor,
            IdempotencyKey::new(format!(
                "runtime-permission-allow-once-{}",
                approval_id.as_str()
            ))
            .unwrap(),
            &approval_id,
            ApprovalDecision::Approve,
            now_ms().unwrap(),
        )
        .unwrap();
    assert!(matches!(
        replayed,
        SchedulerTick::Progressed(TaskView {
            state: TaskState::Succeeded,
            ..
        })
    ));
    assert_eq!(fs::read(&response_path).unwrap(), response_before_replay);
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_provider_permission_dispatch_record(&approval_id)
            .unwrap()
            .state,
        ProviderPermissionDispatchState::Written
    );
    assert!(events
        .iter()
        .any(|event| matches!(event.event, TaskEvent::TaskSucceeded)));
}

#[test]
fn expired_provider_approval_fails_closed_instead_of_renewing_forever() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor, submission("approval-expiry"))
        .unwrap();
    let decisions = Arc::new(Mutex::new(Vec::new()));
    let started_at = now_ms().unwrap().saturating_add(1);
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-expiry").unwrap(),
        PermissionFactory {
            decisions: Arc::clone(&decisions),
            expires_at_ms: started_at + 10_000,
            abandon_before_decision: false,
            core_native: false,
        },
    )
    .unwrap();
    scheduler.tick(started_at).unwrap();
    scheduler.tick(started_at + 1).unwrap();
    let approval_id = scheduler
        .active
        .as_ref()
        .unwrap()
        .pending_permission
        .as_ref()
        .unwrap()
        .approval
        .approval_id
        .clone();

    assert!(matches!(
        scheduler.tick(started_at + 10_000).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Failed,
            ..
        })
    ));
    assert!(decisions.lock().unwrap().is_empty());
    assert!(scheduler.active.is_none());
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_approval_record(&approval_id)
            .unwrap()
            .state,
        ApprovalState::Expired
    );
}

#[test]
fn resolving_at_provider_approval_deadline_expires_without_dispatch() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor, submission("approval-resolve-expiry"))
        .unwrap();
    let decisions = Arc::new(Mutex::new(Vec::new()));
    let started_at = now_ms().unwrap().saturating_add(1);
    let expires_at_ms = started_at + 10_000;
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-resolve-expiry").unwrap(),
        PermissionFactory {
            decisions: Arc::clone(&decisions),
            expires_at_ms,
            abandon_before_decision: false,
            core_native: false,
        },
    )
    .unwrap();
    scheduler.tick(started_at).unwrap();
    scheduler.tick(started_at + 1).unwrap();
    let approval_id = scheduler
        .active
        .as_ref()
        .unwrap()
        .pending_permission
        .as_ref()
        .unwrap()
        .approval
        .approval_id
        .clone();

    assert!(matches!(
        scheduler.resolve_approval(
            &actor,
            IdempotencyKey::new("late-provider-approval").unwrap(),
            &approval_id,
            ApprovalDecision::Approve,
            expires_at_ms,
        ),
        Err(GatewayDaemonError::Protocol(message))
            if message == "approval is no longer resolvable"
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
    assert!(scheduler
        .coordinator
        .store
        .load_provider_permission_dispatch_record(&approval_id)
        .is_err());
    assert!(matches!(
        scheduler.resolve_approval(
            &actor,
            IdempotencyKey::new("late-provider-approval").unwrap(),
            &approval_id,
            ApprovalDecision::Approve,
            expires_at_ms + 1,
        ),
        Err(GatewayDaemonError::Protocol(message))
            if message == "approval is no longer resolvable"
    ));
    assert!(decisions.lock().unwrap().is_empty());
    let task = scheduler
        .coordinator
        .store
        .load_task(
            &scheduler
                .coordinator
                .store
                .load_approval_record(&approval_id)
                .unwrap()
                .task_id,
        )
        .unwrap();
    assert_eq!(task.state(), TaskState::Failed);
}

#[test]
fn durable_cancellation_takes_priority_over_pending_approval() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    let task = coordinator
        .submit(&actor, submission("approval-cancel"))
        .unwrap();
    let run_id = task.active_run_id.clone().unwrap();
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-cancel").unwrap(),
        PermissionFactory {
            decisions: Arc::new(Mutex::new(Vec::new())),
            expires_at_ms: i64::MAX as u64,
            abandon_before_decision: false,
            core_native: false,
        },
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();
    scheduler.tick(started_at + 1).unwrap();
    coordinator
        .cancel(
            &actor,
            CancelTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("cancel-pending-approval").unwrap(),
                task_id: task.task_id,
                run_id,
                expected_revision: None,
            },
        )
        .unwrap();

    assert!(matches!(
        scheduler.tick(started_at + 2).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Cancelled,
            ..
        })
    ));
}
