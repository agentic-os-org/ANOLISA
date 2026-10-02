use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use cosh_gateway_contracts::external::{ExternalRef, ExternalRefKind};
use cosh_gateway_contracts::ids::{
    AgentSessionId, PermitId, RequestId, RuntimeBindingId, RuntimeInstanceId, ToolUseId, TurnId,
};
use cosh_gateway_contracts::runtime::{RuntimeInputOption, RuntimeInputSelections};
use cosh_gateway_contracts::task::UncertaintyCode;

use super::*;
use crate::daemon::{actor_id_for_uid, now_ms, SubmitTask};

const LOGICAL_CLOCK_HEADROOM_MS: u64 = 60_000;

#[derive(Default)]
struct InputProbe {
    request: Option<RuntimeInputRequest>,
    responses: Vec<RuntimeInputResponse>,
    response_runs: Vec<RunId>,
    opens: usize,
    shutdowns: usize,
}

struct InputFactory {
    probe: Arc<Mutex<InputProbe>>,
    fail_dispatch: bool,
}

impl RuntimeFactory for InputFactory {
    fn open(&mut self, run: &ScheduledRun) -> Result<StartedRuntime, ContractError> {
        self.probe.lock().unwrap().opens += 1;
        let request = RuntimeInputRequest::new(
            InputRequestId::new(),
            run.run_id.clone(),
            TurnId::new(),
            Some(ToolUseId::new()),
            BoundedText::new("Choose a branch").unwrap(),
            vec![
                RuntimeInputOption::new(BoundedText::new("main").unwrap(), None),
                RuntimeInputOption::new(BoundedText::new("release").unwrap(), None),
            ],
            false,
            false,
        )
        .unwrap();
        self.probe.lock().unwrap().request = Some(request.clone());
        let binding = RuntimeBindingRef {
            binding_id: RuntimeBindingId::new(),
            task_id: run.task_id.clone(),
            run_id: run.run_id.clone(),
            agent_session_id: AgentSessionId::new(),
            runtime_instance_id: RuntimeInstanceId::new(),
            runtime_generation: run.lease_generation,
            external_session: ExternalRef {
                kind: ExternalRefKind::ProviderSession,
                authority: BoundedName::new("input-test").unwrap(),
                scope_digest: Digest::parse("a".repeat(64)).unwrap(),
                value: BoundedOpaque::new("provider-session").unwrap(),
            },
        };
        Ok(StartedRuntime {
            binding,
            handle: Box::new(InputHandle {
                probe: Arc::clone(&self.probe),
                request,
                run_id: run.run_id.clone(),
                emitted: false,
                fail_dispatch: self.fail_dispatch,
            }),
        })
    }
}

struct InputHandle {
    probe: Arc<Mutex<InputProbe>>,
    request: RuntimeInputRequest,
    run_id: RunId,
    emitted: bool,
    fail_dispatch: bool,
}

impl RuntimeHandle for InputHandle {
    fn begin(&mut self) -> Result<(), ContractError> {
        Ok(())
    }

    fn poll(&mut self) -> RuntimePoll {
        if self.emitted {
            if self
                .probe
                .lock()
                .unwrap()
                .response_runs
                .contains(&self.run_id)
            {
                RuntimePoll::Succeeded
            } else {
                RuntimePoll::Pending
            }
        } else {
            self.emitted = true;
            RuntimePoll::InputRequested {
                sequence: 2,
                request: self.request.clone(),
            }
        }
    }

    fn shutdown(&mut self, _reason: CancelReason) -> Result<(), ContractError> {
        self.probe.lock().unwrap().shutdowns += 1;
        Ok(())
    }

    fn resolve_provider_permission(
        &mut self,
        _permission: &RuntimePermissionRef,
        _decision: RuntimePermissionDecision,
    ) -> Result<(), ContractError> {
        Err(runtime_handle_unsupported("provider permission"))
    }

    fn resolve_input(
        &mut self,
        request: &RuntimeInputRequest,
        response: RuntimeInputResponse,
    ) -> Result<(), ContractError> {
        assert_eq!(request, &self.request);
        let mut probe = self.probe.lock().unwrap();
        probe.responses.push(response);
        probe.response_runs.push(self.run_id.clone());
        drop(probe);
        if self.fail_dispatch {
            Err(runtime_lost_error(
                "input_transport_failed",
                "The Runtime input transport failed",
            )
            .unwrap())
        } else {
            Ok(())
        }
    }
}

fn submission(key: &str) -> SubmitTask {
    SubmitTask {
        request_id: RequestId::new(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        intent: BoundedText::new("ask for input").unwrap(),
        target: GatewayCapabilityProfile::task_only_v1().governed_target(),
        runtime: RuntimeSelector {
            runtime: BoundedName::new("core").unwrap(),
            profile: Some(BoundedName::new("gateway-brokered-v1").unwrap()),
        },
    }
}

fn setup(
    fail_dispatch: bool,
) -> (
    tempfile::TempDir,
    TaskScheduler<InputFactory>,
    ActorId,
    TaskView,
    Arc<Mutex<InputProbe>>,
    u64,
) {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    let mut coordinator = TaskCoordinator::open(&database, Some(installation.clone())).unwrap();
    let task = coordinator
        .submit(&actor_id, submission("input-task"))
        .unwrap();
    drop(coordinator);
    let probe = Arc::new(Mutex::new(InputProbe::default()));
    let mut scheduler = TaskScheduler::open(
        &database,
        Some(installation),
        BoundedOpaque::new("input-worker").unwrap(),
        InputFactory {
            probe: Arc::clone(&probe),
            fail_dispatch,
        },
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    assert!(matches!(
        scheduler.tick(started_at).unwrap(),
        SchedulerTick::Started(_)
    ));
    let waiting = match scheduler.tick(started_at + 1).unwrap() {
        SchedulerTick::Progressed(view) => view,
        other => panic!("input request must make durable progress: {other:?}"),
    };
    assert_eq!(waiting.state, TaskState::WaitingInput);
    (root, scheduler, actor_id, task, probe, started_at)
}

fn selected_main() -> RuntimeInputResponse {
    RuntimeInputResponse::Options {
        selections: RuntimeInputSelections::new(vec![0]).unwrap(),
    }
}

#[test]
fn input_response_is_durable_single_use_and_delivered_replay_never_writes() {
    let (_root, mut scheduler, actor_id, task, probe, _started_at) = setup(false);
    let request = probe.lock().unwrap().request.clone().unwrap();
    let waiting = scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    let resolved_at = scheduler
        .coordinator
        .store
        .load_runtime_input_request(request.request_id())
        .unwrap()
        .updated_at_ms
        + 1;
    let response = selected_main();
    let key = IdempotencyKey::new("append-once").unwrap();
    let result = scheduler
        .resolve_input(
            &actor_id,
            key.clone(),
            &task.task_id,
            request.request_id(),
            response.clone(),
            Some(waiting.revision()),
            resolved_at,
        )
        .unwrap();
    assert!(matches!(
        result,
        SchedulerTick::Progressed(TaskView {
            state: TaskState::Running,
            ..
        })
    ));
    assert_eq!(probe.lock().unwrap().responses, vec![response.clone()]);
    let delivered = scheduler
        .coordinator
        .store
        .load_runtime_input_dispatch(request.request_id())
        .unwrap();
    assert_eq!(delivered.state, RuntimeInputDispatchState::Delivered);

    assert!(matches!(
        scheduler.tick(resolved_at + 1).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Succeeded,
            ..
        })
    ));
    assert!(scheduler.active.is_none());

    let second_task = scheduler
        .coordinator
        .submit(&actor_id, submission("second-input-task"))
        .unwrap();
    let second_started_at = now_ms().unwrap().saturating_add(10);
    assert!(matches!(
        scheduler.tick(second_started_at).unwrap(),
        SchedulerTick::Started(_)
    ));
    assert!(matches!(
        scheduler.tick(second_started_at + 1).unwrap(),
        SchedulerTick::Progressed(TaskView {
            state: TaskState::WaitingInput,
            ..
        })
    ));
    assert_eq!(
        scheduler.active.as_ref().unwrap().scheduled.task_id,
        second_task.task_id
    );

    let replayed = scheduler
        .resolve_input(
            &actor_id,
            key,
            &task.task_id,
            request.request_id(),
            response,
            Some(waiting.revision()),
            second_started_at + 2,
        )
        .unwrap();
    let SchedulerTick::Progressed(replayed) = replayed else {
        panic!("delivered input replay must return its original Task");
    };
    assert_eq!(replayed.task_id, task.task_id);
    assert_eq!(probe.lock().unwrap().responses.len(), 1);
    assert_eq!(probe.lock().unwrap().response_runs.len(), 1);
    assert_eq!(
        scheduler.active.as_ref().unwrap().scheduled.task_id,
        second_task.task_id
    );
}

#[test]
fn input_resolution_rejects_wrong_binding_response_and_idempotency_conflicts() {
    let (_root, mut scheduler, actor_id, task, probe, _started_at) = setup(false);
    let request = probe.lock().unwrap().request.clone().unwrap();
    let waiting = scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    let resolved_at = scheduler
        .coordinator
        .store
        .load_runtime_input_request(request.request_id())
        .unwrap()
        .updated_at_ms
        + 1;

    let wrong_actor = ActorId::new();
    assert!(scheduler
        .resolve_input(
            &wrong_actor,
            IdempotencyKey::new("wrong-actor").unwrap(),
            &task.task_id,
            request.request_id(),
            selected_main(),
            Some(waiting.revision()),
            resolved_at,
        )
        .is_err());
    assert!(scheduler
        .resolve_input(
            &actor_id,
            IdempotencyKey::new("wrong-task").unwrap(),
            &TaskId::new(),
            request.request_id(),
            selected_main(),
            Some(waiting.revision()),
            resolved_at,
        )
        .is_err());
    assert!(scheduler
        .resolve_input(
            &actor_id,
            IdempotencyKey::new("wrong-request").unwrap(),
            &task.task_id,
            &InputRequestId::new(),
            selected_main(),
            Some(waiting.revision()),
            resolved_at,
        )
        .is_err());
    assert!(scheduler
        .resolve_input(
            &actor_id,
            IdempotencyKey::new("wrong-response").unwrap(),
            &task.task_id,
            request.request_id(),
            RuntimeInputResponse::Text {
                text: BoundedText::new("not allowed").unwrap(),
            },
            Some(waiting.revision()),
            resolved_at,
        )
        .is_err());

    let key = IdempotencyKey::new("conflicting-input-key").unwrap();
    scheduler
        .resolve_input(
            &actor_id,
            key.clone(),
            &task.task_id,
            request.request_id(),
            selected_main(),
            Some(waiting.revision()),
            resolved_at,
        )
        .unwrap();
    assert!(scheduler
        .resolve_input(
            &actor_id,
            key,
            &task.task_id,
            request.request_id(),
            RuntimeInputResponse::Options {
                selections: RuntimeInputSelections::new(vec![1]).unwrap(),
            },
            Some(waiting.revision()),
            resolved_at + 1,
        )
        .is_err());
    assert_eq!(probe.lock().unwrap().responses.len(), 1);
}

#[test]
fn transport_failure_marks_dispatch_unknown_and_suspends_without_retry() {
    let (_root, mut scheduler, actor_id, task, probe, _started_at) = setup(true);
    let request = probe.lock().unwrap().request.clone().unwrap();
    let waiting = scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    let resolved_at = scheduler
        .coordinator
        .store
        .load_runtime_input_request(request.request_id())
        .unwrap()
        .updated_at_ms
        + 1;
    let result = scheduler
        .resolve_input(
            &actor_id,
            IdempotencyKey::new("append-fails").unwrap(),
            &task.task_id,
            request.request_id(),
            selected_main(),
            Some(waiting.revision()),
            resolved_at,
        )
        .unwrap();
    assert!(matches!(
        result,
        SchedulerTick::Settled(TaskView {
            state: TaskState::Suspended,
            ..
        })
    ));
    assert_eq!(probe.lock().unwrap().responses.len(), 1);
    assert_eq!(probe.lock().unwrap().shutdowns, 1);
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_runtime_input_dispatch(request.request_id())
            .unwrap()
            .state,
        RuntimeInputDispatchState::Unknown
    );
}

#[test]
fn completion_failure_after_runtime_acceptance_is_unknown_and_never_retried() {
    let (_root, mut scheduler, actor_id, task, probe, _started_at) = setup(false);
    let request = probe.lock().unwrap().request.clone().unwrap();
    let waiting = scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    let resolved_at = scheduler
        .coordinator
        .store
        .load_runtime_input_request(request.request_id())
        .unwrap()
        .updated_at_ms
        + 1;
    scheduler.fail_next_input_dispatch_completion_for_test();

    assert!(matches!(
        scheduler
            .resolve_input(
                &actor_id,
                IdempotencyKey::new("append-completion-fails").unwrap(),
                &task.task_id,
                request.request_id(),
                selected_main(),
                Some(waiting.revision()),
                resolved_at,
            )
            .unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Suspended,
            ..
        })
    ));
    assert_eq!(probe.lock().unwrap().responses.len(), 1);
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_runtime_input_dispatch(request.request_id())
            .unwrap()
            .state,
        RuntimeInputDispatchState::Unknown
    );
}

#[test]
fn replay_of_started_dispatch_never_writes_and_converges_unknown() {
    let (_root, mut scheduler, actor_id, task, probe, _started_at) = setup(false);
    let request = probe.lock().unwrap().request.clone().unwrap();
    let waiting = scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    let resolved_at = scheduler
        .coordinator
        .store
        .load_runtime_input_request(request.request_id())
        .unwrap()
        .updated_at_ms
        + 1;
    let response = selected_main();
    let key = IdempotencyKey::new("append-crash-window").unwrap();
    let command = LedgerCommand {
        actor_id: actor_id.clone(),
        idempotency_key: key.clone(),
        command_digest: digest_json(&(
            "resolve_runtime_input",
            &task.task_id,
            request.request_id(),
            &response,
            Some(waiting.revision()),
        ))
        .unwrap(),
        committed_at_ms: resolved_at,
    };
    let prepared = match scheduler
        .coordinator
        .store
        .resolve_runtime_input(
            &command,
            request.request_id(),
            waiting.revision(),
            &response,
        )
        .unwrap()
    {
        LedgerOutcome::Applied(record) => record,
        LedgerOutcome::Replayed(_) => panic!("first resolution must apply"),
    };
    let lease = scheduler.active.as_ref().unwrap().lease.clone();
    let start = runtime_input_command(
        &actor_id,
        "start",
        request.request_id(),
        prepared.revision,
        resolved_at,
    )
    .unwrap();
    assert!(matches!(
        scheduler
            .coordinator
            .store
            .start_runtime_input_dispatch(
                &start,
                request.request_id(),
                &prepared.response_digest,
                prepared.revision,
                &lease,
            )
            .unwrap(),
        LedgerOutcome::Applied(RuntimeInputDispatchRecord {
            state: RuntimeInputDispatchState::Started,
            ..
        })
    ));

    assert!(matches!(
        scheduler
            .resolve_input(
                &actor_id,
                key,
                &task.task_id,
                request.request_id(),
                response,
                Some(waiting.revision()),
                resolved_at + 1,
            )
            .unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Suspended,
            ..
        })
    ));
    assert!(probe.lock().unwrap().responses.is_empty());
}

#[test]
fn pending_input_expiry_and_cancellation_are_fail_closed() {
    let (_root, mut expiring, _actor_id, _task, probe, _started_at) = setup(false);
    let request = probe.lock().unwrap().request.clone().unwrap();
    let expires_at_ms = expiring
        .coordinator
        .store
        .load_runtime_input_request(request.request_id())
        .unwrap()
        .expires_at_ms;
    let mut renewal_at = expiring
        .coordinator
        .store
        .load_runtime_input_request(request.request_id())
        .unwrap()
        .updated_at_ms
        + 100_000;
    while renewal_at < expires_at_ms {
        assert_eq!(expiring.tick(renewal_at).unwrap(), SchedulerTick::Idle);
        renewal_at = renewal_at.saturating_add(100_000);
    }
    assert!(matches!(
        expiring.tick(expires_at_ms).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Suspended,
            ..
        })
    ));
    assert_eq!(
        expiring
            .coordinator
            .store
            .load_runtime_input_request(request.request_id())
            .unwrap()
            .state,
        RuntimeInputRequestState::Expired
    );

    let (_root, mut cancelling, actor_id, task, probe, _started_at) = setup(false);
    let request = probe.lock().unwrap().request.clone().unwrap();
    let waiting = cancelling
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    cancelling
        .coordinator
        .cancel(
            &actor_id,
            crate::daemon::CancelTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("cancel-input").unwrap(),
                task_id: task.task_id.clone(),
                run_id: waiting.active_run_id().unwrap().clone(),
                expected_revision: Some(waiting.revision()),
            },
        )
        .unwrap();
    assert!(matches!(
        cancelling
            .tick(now_ms().unwrap().saturating_add(1))
            .unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Cancelled,
            ..
        })
    ));
    assert_eq!(
        cancelling
            .coordinator
            .store
            .load_runtime_input_request(request.request_id())
            .unwrap()
            .state,
        RuntimeInputRequestState::Cancelled
    );

    let (_root, mut shutting_down, _actor_id, task, probe, started_at) = setup(false);
    let request = probe.lock().unwrap().request.clone().unwrap();
    let shutdown_at = shutting_down
        .coordinator
        .store
        .load_runtime_input_request(request.request_id())
        .unwrap()
        .updated_at_ms
        .saturating_add(1)
        .max(started_at + 2);
    assert!(matches!(
        shutting_down.shutdown(shutdown_at).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Cancelled,
            ..
        })
    ));
    assert_eq!(
        shutting_down
            .coordinator
            .store
            .load_runtime_input_request(request.request_id())
            .unwrap()
            .state,
        RuntimeInputRequestState::Cancelled
    );
    assert!(matches!(
        shutting_down
            .coordinator
            .store
            .load_runtime_input_dispatch(request.request_id()),
        Err(StoreError::LedgerNotFound { .. })
    ));
    assert_eq!(
        shutting_down
            .coordinator
            .store
            .load_task(&task.task_id)
            .unwrap()
            .state(),
        TaskState::Cancelled
    );
}

#[derive(Clone, Copy)]
enum InputCrashState {
    Pending,
    Prepared,
    Started,
}

#[test]
fn expired_run_takeover_converges_every_input_crash_window_without_runtime_io() {
    for (index, crash_state) in [
        InputCrashState::Pending,
        InputCrashState::Prepared,
        InputCrashState::Started,
    ]
    .into_iter()
    .enumerate()
    {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let database = root.path().join("gateway.db");
        let installation = InstallationId::new();
        let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
        let mut coordinator = TaskCoordinator::open(&database, Some(installation.clone())).unwrap();
        let task = coordinator
            .submit(&actor_id, submission(&format!("crash-input-{index}")))
            .unwrap();
        drop(coordinator);
        let probe = Arc::new(Mutex::new(InputProbe::default()));
        let config = TaskSchedulerConfig {
            lease_duration: Duration::from_millis(200),
            lease_renewal_margin: Duration::from_millis(50),
            runtime_operation_timeout: Duration::from_millis(50),
        };
        let mut first = TaskScheduler::open_with_config(
            &database,
            Some(installation.clone()),
            BoundedOpaque::new(format!("input-crash-worker-{index}")).unwrap(),
            InputFactory {
                probe: Arc::clone(&probe),
                fail_dispatch: false,
            },
            config,
        )
        .unwrap();
        let started_at = now_ms().unwrap().saturating_add(LOGICAL_CLOCK_HEADROOM_MS);
        first.tick(started_at).unwrap();
        first.tick(started_at + 1).unwrap();
        let request = probe.lock().unwrap().request.clone().unwrap();
        let waiting = first.coordinator.store.load_task(&task.task_id).unwrap();
        let recovery_at = first.active.as_ref().unwrap().lease_expires_at_ms;

        if !matches!(crash_state, InputCrashState::Pending) {
            let response = selected_main();
            let prepared_at = first
                .coordinator
                .store
                .load_runtime_input_request(request.request_id())
                .unwrap()
                .updated_at_ms
                .saturating_add(1);
            let command = LedgerCommand {
                actor_id: actor_id.clone(),
                idempotency_key: IdempotencyKey::new(format!("crash-prepare-{index}")).unwrap(),
                command_digest: digest_json(&(
                    "resolve_runtime_input",
                    &task.task_id,
                    request.request_id(),
                    &response,
                    Some(waiting.revision()),
                ))
                .unwrap(),
                committed_at_ms: prepared_at,
            };
            let prepared = match first
                .coordinator
                .store
                .resolve_runtime_input(
                    &command,
                    request.request_id(),
                    waiting.revision(),
                    &response,
                )
                .unwrap()
            {
                LedgerOutcome::Applied(record) => record,
                LedgerOutcome::Replayed(_) => panic!("first prepare must apply"),
            };
            if matches!(crash_state, InputCrashState::Started) {
                let start = runtime_input_command(
                    &actor_id,
                    "start",
                    request.request_id(),
                    prepared.revision,
                    prepared_at + 1,
                )
                .unwrap();
                assert!(matches!(
                    first
                        .coordinator
                        .store
                        .start_runtime_input_dispatch(
                            &start,
                            request.request_id(),
                            &prepared.response_digest,
                            prepared.revision,
                            &first.active.as_ref().unwrap().lease,
                        )
                        .unwrap(),
                    LedgerOutcome::Applied(RuntimeInputDispatchRecord {
                        state: RuntimeInputDispatchState::Started,
                        ..
                    })
                ));
            }
        }
        drop(first);

        let mut reopened = TaskScheduler::open_with_config(
            &database,
            Some(installation),
            BoundedOpaque::new(format!("input-recovery-worker-{index}")).unwrap(),
            InputFactory {
                probe: Arc::clone(&probe),
                fail_dispatch: false,
            },
            config,
        )
        .unwrap();
        assert!(matches!(
            reopened.tick(recovery_at).unwrap(),
            SchedulerTick::Settled(TaskView {
                state: TaskState::Suspended,
                ..
            })
        ));
        assert!(reopened.active.is_none());
        let probe = probe.lock().unwrap();
        assert_eq!(probe.opens, 1);
        assert!(probe.responses.is_empty());
        drop(probe);
        let released_lease = reopened
            .coordinator
            .store
            .load_run_lease(task.active_run_id.as_ref().unwrap())
            .unwrap();
        assert_eq!(released_lease.expires_at_ms, recovery_at);
        assert_eq!(released_lease.updated_at_ms, recovery_at);
        let request_record = reopened
            .coordinator
            .store
            .load_runtime_input_request(request.request_id())
            .unwrap();
        match crash_state {
            InputCrashState::Pending => {
                assert_eq!(request_record.state, RuntimeInputRequestState::Cancelled);
                assert!(matches!(
                    reopened
                        .coordinator
                        .store
                        .load_runtime_input_dispatch(request.request_id()),
                    Err(StoreError::LedgerNotFound { .. })
                ));
            }
            InputCrashState::Prepared | InputCrashState::Started => assert_eq!(
                reopened
                    .coordinator
                    .store
                    .load_runtime_input_dispatch(request.request_id())
                    .unwrap()
                    .state,
                RuntimeInputDispatchState::Unknown
            ),
        }
        let (events, _) = reopened
            .coordinator
            .store
            .load_task_events_for_owner(&task.task_id, &actor_id, None, 64)
            .unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event.event, TaskEvent::RunSuspended { .. })));
        assert!(!events.iter().any(|event| matches!(
            event.event,
            TaskEvent::RunFailed { .. } | TaskEvent::TaskFailed { .. }
        )));
    }
}

#[test]
fn expired_run_takeover_honors_waiting_input_cancellation_before_suspension() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    let mut coordinator = TaskCoordinator::open(&database, Some(installation.clone())).unwrap();
    let task = coordinator
        .submit(&actor_id, submission("cancel-waiting-input-crash"))
        .unwrap();
    drop(coordinator);
    let probe = Arc::new(Mutex::new(InputProbe::default()));
    let config = TaskSchedulerConfig {
        lease_duration: Duration::from_millis(200),
        lease_renewal_margin: Duration::from_millis(50),
        runtime_operation_timeout: Duration::from_millis(50),
    };
    let mut first = TaskScheduler::open_with_config(
        &database,
        Some(installation.clone()),
        BoundedOpaque::new("cancel-input-crash-worker").unwrap(),
        InputFactory {
            probe: Arc::clone(&probe),
            fail_dispatch: false,
        },
        config,
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(LOGICAL_CLOCK_HEADROOM_MS);
    first.tick(started_at).unwrap();
    first.tick(started_at + 1).unwrap();
    let request = probe.lock().unwrap().request.clone().unwrap();
    let waiting = first.coordinator.store.load_task(&task.task_id).unwrap();
    first
        .coordinator
        .cancel(
            &actor_id,
            crate::daemon::CancelTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("cancel-input-before-crash").unwrap(),
                task_id: task.task_id.clone(),
                run_id: waiting.active_run_id().unwrap().clone(),
                expected_revision: Some(waiting.revision()),
            },
        )
        .unwrap();
    let recovery_at = first.active.as_ref().unwrap().lease_expires_at_ms;
    drop(first);

    let mut reopened = TaskScheduler::open_with_config(
        &database,
        Some(installation),
        BoundedOpaque::new("cancel-input-recovery-worker").unwrap(),
        InputFactory {
            probe: Arc::clone(&probe),
            fail_dispatch: false,
        },
        config,
    )
    .unwrap();
    assert!(matches!(
        reopened.tick(recovery_at).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Cancelled,
            ..
        })
    ));
    assert_eq!(probe.lock().unwrap().opens, 1);
    assert!(probe.lock().unwrap().responses.is_empty());
    assert_eq!(
        reopened
            .coordinator
            .store
            .load_runtime_input_request(request.request_id())
            .unwrap()
            .state,
        RuntimeInputRequestState::Cancelled
    );
    assert!(matches!(
        reopened
            .coordinator
            .store
            .load_runtime_input_dispatch(request.request_id()),
        Err(StoreError::LedgerNotFound { .. })
    ));
}

#[test]
fn second_takeover_finishes_cancellation_after_first_recovery_crashes() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    let mut coordinator = TaskCoordinator::open(&database, Some(installation.clone())).unwrap();
    let task = coordinator
        .submit(&actor_id, submission("double-crash-input-cancel"))
        .unwrap();
    drop(coordinator);
    let probe = Arc::new(Mutex::new(InputProbe::default()));
    let config = TaskSchedulerConfig {
        lease_duration: Duration::from_millis(200),
        lease_renewal_margin: Duration::from_millis(50),
        runtime_operation_timeout: Duration::from_millis(50),
    };
    let mut first = TaskScheduler::open_with_config(
        &database,
        Some(installation.clone()),
        BoundedOpaque::new("double-crash-original-worker").unwrap(),
        InputFactory {
            probe: Arc::clone(&probe),
            fail_dispatch: false,
        },
        config,
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(LOGICAL_CLOCK_HEADROOM_MS);
    first.tick(started_at).unwrap();
    first.tick(started_at + 1).unwrap();
    let request = probe.lock().unwrap().request.clone().unwrap();
    let waiting = first.coordinator.store.load_task(&task.task_id).unwrap();
    let run_id = waiting.active_run_id().unwrap().clone();
    first
        .coordinator
        .cancel(
            &actor_id,
            crate::daemon::CancelTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("double-crash-cancel").unwrap(),
                task_id: task.task_id.clone(),
                run_id: run_id.clone(),
                expected_revision: Some(waiting.revision()),
            },
        )
        .unwrap();
    let first_recovery_at = first.active.as_ref().unwrap().lease_expires_at_ms;
    drop(first);

    let mut interrupted = TaskCoordinator::open(&database, Some(installation.clone())).unwrap();
    let second_expiry = first_recovery_at.saturating_add(200);
    let lease_command = LeaseCommand {
        command: LedgerCommand {
            actor_id: actor_id.clone(),
            idempotency_key: IdempotencyKey::new("double-crash-takeover-lease").unwrap(),
            command_digest: digest_json(&(
                "double_crash_takeover",
                &task.task_id,
                &run_id,
                second_expiry,
            ))
            .unwrap(),
            committed_at_ms: first_recovery_at,
        },
        task_id: task.task_id.clone(),
        run_id: run_id.clone(),
        lease_owner: BoundedOpaque::new("double-crash-interrupted-worker").unwrap(),
        expires_at_ms: second_expiry,
    };
    let lease = match interrupted.store.acquire_run_lease(&lease_command).unwrap() {
        LedgerOutcome::Applied(record) => LeaseClaim {
            task_id: record.task_id,
            run_id: record.run_id,
            lease_owner: record.lease_owner,
            generation: record.generation,
            revision: record.revision,
        },
        LedgerOutcome::Replayed(_) => panic!("first takeover must apply"),
    };
    interrupted
        .store
        .mark_runtime_bindings_lost_for_run(&run_id, first_recovery_at)
        .unwrap();
    let input_recovery = LedgerCommand {
        actor_id: actor_id.clone(),
        idempotency_key: IdempotencyKey::new("double-crash-input-recovery").unwrap(),
        command_digest: digest_json(&(
            "recover_runtime_input_dispatch_for_run",
            &task.task_id,
            &run_id,
            lease.generation,
        ))
        .unwrap(),
        committed_at_ms: first_recovery_at,
    };
    interrupted
        .store
        .recover_runtime_input_dispatch_for_run(&input_recovery, &run_id, &lease)
        .unwrap();
    let interrupted_task = interrupted.store.load_task(&task.task_id).unwrap();
    assert_eq!(interrupted_task.state(), TaskState::Suspended);
    assert!(interrupted_task.cancellation_requested());
    assert_eq!(
        interrupted
            .store
            .load_runtime_input_request(request.request_id())
            .unwrap()
            .state,
        RuntimeInputRequestState::Cancelled
    );
    drop(interrupted);

    let mut final_recovery = TaskScheduler::open_with_config(
        &database,
        Some(installation),
        BoundedOpaque::new("double-crash-final-worker").unwrap(),
        InputFactory {
            probe: Arc::clone(&probe),
            fail_dispatch: false,
        },
        config,
    )
    .unwrap();
    assert!(matches!(
        final_recovery.tick(second_expiry).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Cancelled,
            ..
        })
    ));
    assert_eq!(probe.lock().unwrap().opens, 1);
    assert!(probe.lock().unwrap().responses.is_empty());
}

#[test]
fn expired_running_run_takeover_honors_durable_cancellation() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    let mut coordinator = TaskCoordinator::open(&database, Some(installation.clone())).unwrap();
    let task = coordinator
        .submit(&actor_id, submission("cancel-running-crash"))
        .unwrap();
    drop(coordinator);
    let probe = Arc::new(Mutex::new(InputProbe::default()));
    let config = TaskSchedulerConfig {
        lease_duration: Duration::from_millis(200),
        lease_renewal_margin: Duration::from_millis(50),
        runtime_operation_timeout: Duration::from_millis(50),
    };
    let mut first = TaskScheduler::open_with_config(
        &database,
        Some(installation.clone()),
        BoundedOpaque::new("cancel-running-crash-worker").unwrap(),
        InputFactory {
            probe: Arc::clone(&probe),
            fail_dispatch: false,
        },
        config,
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(LOGICAL_CLOCK_HEADROOM_MS);
    first.tick(started_at).unwrap();
    let running = first.coordinator.store.load_task(&task.task_id).unwrap();
    first
        .coordinator
        .cancel(
            &actor_id,
            crate::daemon::CancelTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("cancel-running-before-crash").unwrap(),
                task_id: task.task_id.clone(),
                run_id: running.active_run_id().unwrap().clone(),
                expected_revision: Some(running.revision()),
            },
        )
        .unwrap();
    let recovery_at = first.active.as_ref().unwrap().lease_expires_at_ms;
    drop(first);

    let mut reopened = TaskScheduler::open_with_config(
        &database,
        Some(installation),
        BoundedOpaque::new("cancel-running-recovery-worker").unwrap(),
        InputFactory {
            probe: Arc::clone(&probe),
            fail_dispatch: false,
        },
        config,
    )
    .unwrap();
    assert!(matches!(
        reopened.tick(recovery_at).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Cancelled,
            ..
        })
    ));
    assert_eq!(probe.lock().unwrap().opens, 1);
    assert!(probe.lock().unwrap().responses.is_empty());
    let (events, _) = reopened
        .coordinator
        .store
        .load_task_events_for_owner(&task.task_id, &actor_id, None, 64)
        .unwrap();
    assert!(!events.iter().any(|event| matches!(
        event.event,
        TaskEvent::RunFailed { .. } | TaskEvent::TaskFailed { .. }
    )));
}

#[test]
fn safely_suspended_input_and_retryable_failure_can_be_explicitly_cancelled() {
    let (_root, mut input_scheduler, actor_id, task, probe, _started_at) = setup(true);
    let request = probe.lock().unwrap().request.clone().unwrap();
    let waiting = input_scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    let resolved_at = input_scheduler
        .coordinator
        .store
        .load_runtime_input_request(request.request_id())
        .unwrap()
        .updated_at_ms
        .saturating_add(1);
    assert!(matches!(
        input_scheduler
            .resolve_input(
                &actor_id,
                IdempotencyKey::new("suspend-before-cancel").unwrap(),
                &task.task_id,
                request.request_id(),
                selected_main(),
                Some(waiting.revision()),
                resolved_at,
            )
            .unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Suspended,
            ..
        })
    ));
    let suspended = input_scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    let cancelled = input_scheduler
        .coordinator
        .cancel(
            &actor_id,
            crate::daemon::CancelTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("cancel-suspended-input").unwrap(),
                task_id: task.task_id.clone(),
                run_id: suspended.active_run_id().unwrap().clone(),
                expected_revision: Some(suspended.revision()),
            },
        )
        .unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);

    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    let mut coordinator = TaskCoordinator::open(&database, Some(installation.clone())).unwrap();
    let task = coordinator
        .submit(&actor_id, submission("retryable-failure-cancel"))
        .unwrap();
    drop(coordinator);
    let probe = Arc::new(Mutex::new(InputProbe::default()));
    let mut scheduler = TaskScheduler::open(
        &database,
        Some(installation),
        BoundedOpaque::new("retryable-failure-worker").unwrap(),
        InputFactory {
            probe: Arc::clone(&probe),
            fail_dispatch: false,
        },
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();
    let failed_at = now_ms().unwrap().saturating_add(1).max(started_at + 1);
    assert!(matches!(
        scheduler
            .finish_failed(
                ContractError::new(
                    "retryable_runtime_failure",
                    ErrorCategory::RuntimeUnavailable,
                    true,
                    "Runtime is temporarily unavailable",
                )
                .unwrap(),
                failed_at,
            )
            .unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Suspended,
            ..
        })
    ));
    let failed = scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    let cancelled = scheduler
        .coordinator
        .cancel(
            &actor_id,
            crate::daemon::CancelTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("cancel-retryable-failure").unwrap(),
                task_id: task.task_id.clone(),
                run_id: failed.active_run_id().unwrap().clone(),
                expected_revision: Some(failed.revision()),
            },
        )
        .unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert_eq!(probe.lock().unwrap().opens, 1);
    assert!(probe.lock().unwrap().responses.is_empty());
}

#[test]
fn released_suspended_run_is_not_recovered_again_or_allowed_to_starve_queued_work() {
    let (_root, mut scheduler, actor_id, task, probe, _started_at) = setup(true);
    let request = probe.lock().unwrap().request.clone().unwrap();
    let waiting = scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    let resolved_at = scheduler
        .coordinator
        .store
        .load_runtime_input_request(request.request_id())
        .unwrap()
        .updated_at_ms
        .saturating_add(1);
    assert!(matches!(
        scheduler
            .resolve_input(
                &actor_id,
                IdempotencyKey::new("suspend-before-next-task").unwrap(),
                &task.task_id,
                request.request_id(),
                selected_main(),
                Some(waiting.revision()),
                resolved_at,
            )
            .unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Suspended,
            ..
        })
    ));
    let old_run_id = task.active_run_id.as_ref().unwrap();
    let released = scheduler
        .coordinator
        .store
        .load_run_lease(old_run_id)
        .unwrap();
    let queued = scheduler
        .coordinator
        .submit(&actor_id, submission("queued-after-suspended-release"))
        .unwrap();
    let far_future = released.expires_at_ms.saturating_add(1_000_000);
    assert!(matches!(
        scheduler.tick(far_future).unwrap(),
        SchedulerTick::Started(TaskView { task_id, .. }) if task_id == queued.task_id
    ));
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_run_lease(old_run_id)
            .unwrap()
            .generation,
        released.generation
    );
    assert_eq!(probe.lock().unwrap().opens, 2);
}

#[test]
fn uncertain_suspended_run_rejects_cancellation_without_partial_events() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    let mut coordinator = TaskCoordinator::open(&database, Some(installation.clone())).unwrap();
    let task = coordinator
        .submit(&actor_id, submission("uncertain-cancel"))
        .unwrap();
    drop(coordinator);
    let probe = Arc::new(Mutex::new(InputProbe::default()));
    let mut scheduler = TaskScheduler::open(
        &database,
        Some(installation),
        BoundedOpaque::new("uncertain-cancel-worker").unwrap(),
        InputFactory {
            probe,
            fail_dispatch: false,
        },
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();
    let running = scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    let run_id = running.active_run_id().unwrap().clone();
    let execution_id = ExecutionId::new();
    let committed_at_ms = now_ms().unwrap().saturating_add(1).max(started_at + 1);
    let planned = scheduler.coordinator.event(
        &actor_id,
        &task.task_id,
        Some(&run_id),
        running.revision().saturating_add(1),
        committed_at_ms,
        TaskEvent::ExecutionPlanned {
            execution_id: execution_id.clone(),
            permit_id: PermitId::new(),
        },
    );
    let uncertain = scheduler.coordinator.event(
        &actor_id,
        &task.task_id,
        Some(&run_id),
        running.revision().saturating_add(2),
        committed_at_ms,
        TaskEvent::ExecutionUncertain {
            execution_id,
            reason: UncertaintyCode::TransportLost,
        },
    );
    scheduler
        .coordinator
        .store
        .commit_task(&TaskCommit {
            actor_id: actor_id.clone(),
            idempotency_key: IdempotencyKey::new("make-execution-uncertain").unwrap(),
            command_digest: digest_json(&("make_execution_uncertain", &task.task_id)).unwrap(),
            expected_revision: Some(running.revision()),
            events: vec![planned, uncertain],
            outbox: Vec::new(),
            committed_at_ms,
        })
        .unwrap();
    let before = scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    assert_eq!(before.state(), TaskState::Suspended);
    assert!(scheduler
        .coordinator
        .cancel(
            &actor_id,
            crate::daemon::CancelTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("reject-uncertain-cancel").unwrap(),
                task_id: task.task_id.clone(),
                run_id,
                expected_revision: Some(before.revision()),
            },
        )
        .is_err());
    let after = scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    assert_eq!(after, before);
    assert!(!after.cancellation_requested());
}

#[test]
fn shutdown_recovers_input_committed_before_memory_installation() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let actor_id = actor_id_for_uid(&installation, 1000).unwrap();
    let mut coordinator = TaskCoordinator::open(&database, Some(installation.clone())).unwrap();
    let task = coordinator
        .submit(&actor_id, submission("input-install-crash"))
        .unwrap();
    drop(coordinator);
    let probe = Arc::new(Mutex::new(InputProbe::default()));
    let mut scheduler = TaskScheduler::open(
        &database,
        Some(installation),
        BoundedOpaque::new("input-install-crash-worker").unwrap(),
        InputFactory {
            probe: Arc::clone(&probe),
            fail_dispatch: false,
        },
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();
    scheduler.fail_next_input_request_install_for_test();
    assert!(matches!(
        scheduler.tick(started_at + 1),
        Err(GatewayDaemonError::Protocol(message))
            if message == "injected failure before pending input installation"
    ));
    assert!(scheduler.active.as_ref().unwrap().pending_input.is_none());
    let request = probe.lock().unwrap().request.clone().unwrap();
    let pending = scheduler
        .coordinator
        .store
        .load_runtime_input_request(request.request_id())
        .unwrap();
    assert_eq!(pending.state, RuntimeInputRequestState::Pending);
    let shutdown_at = pending.updated_at_ms.saturating_add(1);
    assert!(matches!(
        scheduler.shutdown(shutdown_at).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Cancelled,
            ..
        })
    ));
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_runtime_input_request(request.request_id())
            .unwrap()
            .state,
        RuntimeInputRequestState::Cancelled
    );
    assert!(matches!(
        scheduler
            .coordinator
            .store
            .load_runtime_input_dispatch(request.request_id()),
        Err(StoreError::LedgerNotFound { .. })
    ));
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_task(&task.task_id)
            .unwrap()
            .state(),
        TaskState::Cancelled
    );
}

#[test]
fn suspended_cancel_waits_for_runtime_binding_and_input_dispatch_cleanup() {
    let (_root, mut scheduler, actor_id, task, probe, _started_at) = setup(false);
    let request = probe.lock().unwrap().request.clone().unwrap();
    let waiting = scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    let resolved_at = scheduler
        .coordinator
        .store
        .load_runtime_input_request(request.request_id())
        .unwrap()
        .updated_at_ms
        .saturating_add(1);
    scheduler.fail_next_input_dispatch_completion_for_test();
    scheduler.fail_next_input_unknown_cleanup_for_test();
    assert!(matches!(
        scheduler.resolve_input(
            &actor_id,
            IdempotencyKey::new("unknown-before-cleanup").unwrap(),
            &task.task_id,
            request.request_id(),
            selected_main(),
            Some(waiting.revision()),
            resolved_at,
        ),
        Err(GatewayDaemonError::Protocol(message))
            if message == "injected failure before uncertain input Runtime cleanup"
    ));
    let suspended = scheduler
        .coordinator
        .store
        .load_task(&task.task_id)
        .unwrap();
    assert_eq!(suspended.state(), TaskState::Suspended);
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_runtime_input_dispatch(request.request_id())
            .unwrap()
            .state,
        RuntimeInputDispatchState::Unknown
    );
    let cancel = crate::daemon::CancelTask {
        request_id: RequestId::new(),
        idempotency_key: IdempotencyKey::new("cancel-after-input-cleanup").unwrap(),
        task_id: task.task_id.clone(),
        run_id: suspended.active_run_id().unwrap().clone(),
        expected_revision: Some(suspended.revision()),
    };
    assert!(matches!(
        scheduler.coordinator.cancel(&actor_id, cancel.clone()),
        Err(GatewayDaemonError::Store(StoreError::LedgerConflict { .. }))
    ));
    assert_eq!(
        scheduler
            .coordinator
            .store
            .load_task(&task.task_id)
            .unwrap(),
        suspended
    );

    assert!(matches!(
        scheduler.finish_suspended_after_input(resolved_at).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Suspended,
            ..
        })
    ));
    while now_ms().unwrap() < resolved_at {
        std::thread::yield_now();
    }
    assert_eq!(
        scheduler
            .coordinator
            .cancel(&actor_id, cancel)
            .unwrap()
            .state,
        TaskState::Cancelled
    );
}
