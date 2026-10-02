use std::collections::VecDeque;
use std::io::{Cursor, Write as _};
use std::os::unix::fs::DirBuilderExt;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cosh_gateway_contracts::common::{
    BoundedName, BoundedOpaque, BoundedText, IdempotencyKey, RuntimeBindingRef, MAX_TEXT_BYTES,
};
use cosh_gateway_contracts::error::{ContractError, ErrorCategory};
use cosh_gateway_contracts::external::{ExternalRef, ExternalRefKind};
use cosh_gateway_contracts::ids::{AgentSessionId, RuntimeBindingId, RuntimeInstanceId};
use cosh_gateway_contracts::runtime::{
    RuntimeInputSelections, RuntimePermissionDecision, RuntimePermissionRef,
};
use cosh_gateway_contracts::task::RuntimeUpdate;
use tempfile::TempDir;

use super::*;

fn brokered_core_runtime() -> RuntimeSelector {
    RuntimeSelector {
        runtime: BoundedName::new("core").unwrap(),
        profile: Some(BoundedName::new("gateway-brokered-v1").unwrap()),
    }
}

fn acp_runtime() -> RuntimeSelector {
    RuntimeSelector {
        runtime: BoundedName::new("acp").unwrap(),
        profile: Some(BoundedName::new("codex").unwrap()),
    }
}

fn submit(key: &str) -> SubmitTask {
    SubmitTask {
        request_id: RequestId::new(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        intent: BoundedText::new("inspect the failed service").unwrap(),
        target: GatewayCapabilityProfile::task_only_v1().governed_target(),
        runtime: brokered_core_runtime(),
    }
}

fn private_directory(root: &TempDir, name: &str) -> PathBuf {
    let path = root.path().join(name);
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(&path).unwrap();
    path
}

fn private_tempdir() -> TempDir {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn daemon_config(socket_path: PathBuf, database_path: PathBuf) -> GatewayDaemonConfig {
    let workspace = WorkspaceRef {
        scope_digest: sha256_digest(b"cosh.gateway.test.workspace.v1"),
        display_name: None,
    };
    GatewayDaemonConfig {
        socket_path,
        database_path,
        installation_id: None,
        launch_catalog: TaskLaunchCatalog::new(
            workspace.clone(),
            LaunchReadiness::Ready,
            LaunchReadiness::unavailable(BoundedText::new("Codex is not configured").unwrap()),
            LaunchReadiness::unavailable(
                BoundedText::new("checkpoint provider is not configured").unwrap(),
            ),
        ),
    }
}

fn delegated_daemon_config(socket_path: PathBuf, database_path: PathBuf) -> GatewayDaemonConfig {
    let workspace = WorkspaceRef {
        scope_digest: sha256_digest(b"cosh.gateway.test.delegated-workspace.v1"),
        display_name: None,
    };
    GatewayDaemonConfig {
        socket_path,
        database_path,
        installation_id: None,
        launch_catalog: TaskLaunchCatalog::new(
            workspace.clone(),
            LaunchReadiness::unavailable(BoundedText::new("Core is not configured").unwrap()),
            LaunchReadiness::Ready,
            LaunchReadiness::unavailable(
                BoundedText::new("checkpoint provider is not configured").unwrap(),
            ),
        ),
    }
}

#[test]
fn local_capabilities_and_submit_launch_use_the_sealed_catalog() {
    let root = private_tempdir();
    let socket_path = private_directory(&root, "runtime").join("gateway.sock");
    let database_path = root.path().join("gateway.db");
    let config = daemon_config(socket_path.clone(), database_path);
    let workspace = config.launch_catalog.default_workspace().clone();
    let mut daemon = GatewayDaemon::bind(config).unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let server_shutdown = Arc::clone(&shutdown);
    let server = std::thread::spawn(move || daemon.serve_until(&server_shutdown));
    let client = LocalGatewayClient::new(socket_path);

    let GatewayResult::Capabilities(capabilities) = client.capabilities(RequestId::new()).unwrap()
    else {
        panic!("capability request must return the launch catalog")
    };
    assert_eq!(capabilities.default_workspace, workspace);
    assert_eq!(capabilities.default_approval, ApprovalPolicy::AllowAll);
    assert_eq!(capabilities.runtimes[0].runtime, TaskRuntime::Core);
    assert_eq!(capabilities.runtimes[0].readiness, LaunchReadiness::Ready);
    assert_eq!(
        capabilities.runtimes[0].security,
        RuntimeSecurityPosture {
            delegated_local_authority: true,
            gateway_brokered_effects: false,
            checkpoint_is_baseline_only: false,
        }
    );
    assert!(matches!(
        capabilities.runtimes[1].readiness,
        LaunchReadiness::Unavailable { .. }
    ));

    let launch = TaskLaunchSpecV1::new(
        BoundedText::new("inspect the failed service").unwrap(),
        TaskRuntime::Core,
        workspace,
        CheckpointPolicy::Off,
        ApprovalPolicy::AllowAll,
    );
    let GatewayResult::Task(task) = client
        .submit_launch(SubmitLaunch {
            request_id: RequestId::new(),
            idempotency_key: IdempotencyKey::new("sealed-launch").unwrap(),
            launch: launch.clone(),
        })
        .unwrap()
    else {
        panic!("launch submission must return its Task")
    };
    assert_eq!(
        task.launch,
        Some(TaskLaunchDescriptorV1::from_spec(
            sha256_digest(launch.goal.as_str().as_bytes()),
            &launch,
        ))
    );
    assert_eq!(task.state, TaskState::Queued);
    assert_eq!(task.target.identifier.as_str(), "workspace-write-v1");

    shutdown.store(true, Ordering::Relaxed);
    server.join().unwrap().unwrap();
}

#[test]
fn task_list_omits_private_goals_and_stays_compact() {
    let root = private_tempdir();
    let socket_path = private_directory(&root, "runtime").join("gateway.sock");
    let database_path = root.path().join("gateway.db");
    let config = daemon_config(socket_path.clone(), database_path);
    let workspace = config.launch_catalog.default_workspace().clone();
    let mut daemon = GatewayDaemon::bind(config).unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let server_shutdown = Arc::clone(&shutdown);
    let server = std::thread::spawn(move || daemon.serve_until(&server_shutdown));
    let client = LocalGatewayClient::new(socket_path);
    let sentinel = "PRIVATE_GOAL_SENTINEL_";
    let private_goal = format!("{sentinel}{}", "x".repeat(MAX_TEXT_BYTES - sentinel.len()));

    for index in 0..20 {
        let launch = TaskLaunchSpecV1::new(
            BoundedText::new(private_goal.clone()).unwrap(),
            TaskRuntime::Core,
            workspace.clone(),
            CheckpointPolicy::Off,
            ApprovalPolicy::AllowAll,
        );
        let GatewayResult::Task(task) = client
            .submit_launch(SubmitLaunch {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new(format!("compact-list-{index}")).unwrap(),
                launch,
            })
            .unwrap()
        else {
            panic!("launch submission must return its Task")
        };
        assert!(task.launch.is_some());
    }

    let GatewayResult::Tasks(page) = client.list(RequestId::new(), 20).unwrap() else {
        panic!("list must return Task projections")
    };
    let json = serde_json::to_vec(&page).unwrap();
    assert_eq!(page.tasks.len(), 20);
    assert!(!json
        .windows(sentinel.len())
        .any(|window| window == sentinel.as_bytes()));
    assert!(
        json.len() < 64 * 1024,
        "safe list projection is unexpectedly large"
    );
    assert!(page.tasks.iter().all(|task| task
        .launch
        .as_ref()
        .is_some_and(|launch| launch.goal_digest == sha256_digest(private_goal.as_bytes()))));

    shutdown.store(true, Ordering::Relaxed);
    server.join().unwrap().unwrap();
}

struct FakeFactory {
    polls: Option<VecDeque<RuntimePoll>>,
    started: Arc<Mutex<Vec<ScheduledRun>>>,
    cancellations: Arc<AtomicUsize>,
    ack_sabotage_path: Option<PathBuf>,
}

impl RuntimeFactory for FakeFactory {
    fn open(
        &mut self,
        run: &ScheduledRun,
    ) -> Result<StartedRuntime, cosh_gateway_contracts::error::ContractError> {
        self.started.lock().unwrap().push(run.clone());
        if let Some(path) = &self.ack_sabotage_path {
            let connection = rusqlite::Connection::open(path).unwrap();
            connection
                .execute(
                    "UPDATE outbox SET attempt=attempt+1 WHERE state='leased'",
                    [],
                )
                .unwrap();
        }
        Ok(StartedRuntime {
            binding: RuntimeBindingRef {
                binding_id: RuntimeBindingId::new(),
                task_id: run.task_id.clone(),
                run_id: run.run_id.clone(),
                agent_session_id: AgentSessionId::new(),
                runtime_instance_id: RuntimeInstanceId::new(),
                runtime_generation: run.lease_generation,
                external_session: ExternalRef {
                    kind: ExternalRefKind::AcpSession,
                    authority: BoundedName::new("test-adapter").unwrap(),
                    scope_digest: sha256_digest(b"test-runtime-session"),
                    value: BoundedOpaque::new("test-session").unwrap(),
                },
            },
            handle: Box::new(FakeHandle {
                polls: self.polls.take().unwrap_or_default(),
                cancellations: Arc::clone(&self.cancellations),
            }),
        })
    }
}

struct FakeHandle {
    polls: VecDeque<RuntimePoll>,
    cancellations: Arc<AtomicUsize>,
}

impl RuntimeHandle for FakeHandle {
    fn begin(&mut self) -> Result<(), cosh_gateway_contracts::error::ContractError> {
        Ok(())
    }

    fn poll(&mut self) -> RuntimePoll {
        self.polls.pop_front().unwrap_or(RuntimePoll::Pending)
    }

    fn shutdown(
        &mut self,
        _reason: CancelReason,
    ) -> Result<(), cosh_gateway_contracts::error::ContractError> {
        self.cancellations.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn resolve_provider_permission(
        &mut self,
        _permission: &RuntimePermissionRef,
        _decision: RuntimePermissionDecision,
    ) -> Result<(), cosh_gateway_contracts::error::ContractError> {
        Ok(())
    }
}

fn fake_factory(polls: impl IntoIterator<Item = RuntimePoll>) -> (FakeFactory, Arc<AtomicUsize>) {
    let cancellations = Arc::new(AtomicUsize::new(0));
    (
        FakeFactory {
            polls: Some(polls.into_iter().collect()),
            started: Arc::new(Mutex::new(Vec::new())),
            cancellations: Arc::clone(&cancellations),
            ack_sabotage_path: None,
        },
        cancellations,
    )
}

#[test]
fn frame_is_big_endian_bounded_and_round_trips() {
    let request = GatewayRequest::Ping {
        api_version: GATEWAY_API_VERSION.to_owned(),
        request_id: RequestId::new(),
    };
    let mut wire = Vec::new();
    write_frame(&mut wire, &request).unwrap();
    let declared = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    assert_eq!(declared, wire.len() - 4);
    let decoded = read_frame::<GatewayRequest>(&mut Cursor::new(wire)).unwrap();
    assert_eq!(decoded.request_id(), request.request_id());

    let mut oversized = (u32::try_from(MAX_GATEWAY_FRAME_BYTES).unwrap() + 1)
        .to_be_bytes()
        .to_vec();
    oversized.extend_from_slice(b"{}");
    assert!(matches!(
        read_frame::<GatewayRequest>(&mut Cursor::new(oversized)),
        Err(GatewayDaemonError::Protocol(_))
    ));

    let maximum_payload = "x".repeat(MAX_GATEWAY_FRAME_BYTES - 2);
    let mut maximum_frame = Vec::new();
    write_frame(&mut maximum_frame, &maximum_payload).unwrap();
    assert_eq!(
        u32::from_be_bytes(maximum_frame[..4].try_into().unwrap()) as usize,
        MAX_GATEWAY_FRAME_BYTES
    );
    assert_eq!(
        read_frame::<String>(&mut Cursor::new(maximum_frame)).unwrap(),
        maximum_payload
    );
}

#[test]
fn request_rejects_unknown_fields() {
    let request_id = RequestId::new();
    let payload = format!(
        r#"{{"command":"ping","api_version":"cosh.gateway.v1","request_id":"{request_id}","actor":"forged"}}"#
    );
    assert!(serde_json::from_str::<GatewayRequest>(&payload).is_err());

    for request in [
        GatewayRequest::Submit {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request: submit("strict-submit"),
        },
        GatewayRequest::Cancel {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request: CancelTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("strict-cancel").unwrap(),
                task_id: TaskId::new(),
                run_id: RunId::new(),
                expected_revision: None,
            },
        },
        GatewayRequest::Retry {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request: RetryTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("strict-retry").unwrap(),
                task_id: TaskId::new(),
                previous_run_id: RunId::new(),
                expected_revision: None,
            },
        },
        GatewayRequest::AppendInput {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request: AppendTaskInput {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("strict-input").unwrap(),
                task_id: TaskId::new(),
                input_request_id: InputRequestId::new(),
                response: RuntimeInputResponse::Options {
                    selections: RuntimeInputSelections::new(vec![0]).unwrap(),
                },
                expected_revision: None,
            },
        },
    ] {
        let mut value = serde_json::to_value(request).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("actor".to_owned(), serde_json::json!("forged"));
        assert!(serde_json::from_value::<GatewayRequest>(value).is_err());
    }
}

#[test]
fn gateway_wire_v1_matches_frozen_golden_corpus() {
    let expected: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../tests/fixtures/gateway-wire-v1.json")).unwrap();
    let requests: Vec<GatewayRequest> = expected
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()
        .unwrap();
    let commands: Vec<&str> = expected
        .iter()
        .map(|value| value["command"].as_str().unwrap())
        .collect();
    assert_eq!(
        commands,
        [
            "ping",
            "submit",
            "get",
            "events",
            "cancel",
            "resolve_approval",
            "retry",
            "append_input",
        ]
    );

    let actual: Vec<serde_json::Value> = requests
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn task_bound_approval_is_an_append_only_v1_command() {
    let request = GatewayRequest::ResolveApprovalForTask {
        api_version: GATEWAY_API_VERSION.to_owned(),
        request: ResolveApprovalForTask {
            request_id: RequestId::new(),
            idempotency_key: IdempotencyKey::new("bound-approval").unwrap(),
            task_id: TaskId::new(),
            approval_id: ApprovalId::new(),
            decision: ApprovalDecision::Approve,
        },
    };
    let value = serde_json::to_value(request).unwrap();
    assert_eq!(value["command"], "resolve_approval_for_task");
    assert_eq!(value["api_version"], "cosh.gateway.v1");
    assert!(value.get("task_id").is_some());
}

#[test]
fn stable_actor_identity_depends_on_peer_uid() {
    let installation = InstallationId::new();
    let first = actor_id_for_uid(&installation, 1000).unwrap();
    assert_eq!(first, actor_id_for_uid(&installation, 1000).unwrap());
    assert_ne!(first, actor_id_for_uid(&installation, 1001).unwrap());
    assert_ne!(
        first,
        actor_id_for_uid(&InstallationId::new(), 1000).unwrap()
    );
}

#[test]
fn coordinator_replays_submit_and_hides_foreign_tasks() {
    let root = private_tempdir();
    let mut coordinator =
        TaskCoordinator::open(root.path().join("gateway.db"), Some(InstallationId::new())).unwrap();
    let owner = actor_id_for_uid(&coordinator.installation_id, 1000).unwrap();
    let request = submit("retry-key");
    let first = coordinator.submit(&owner, request.clone()).unwrap();
    let replay = coordinator.submit(&owner, request).unwrap();
    assert_eq!(first, replay);
    assert_eq!(
        coordinator.list(&owner, 64).unwrap().tasks,
        vec![first.clone()]
    );
    assert!(coordinator
        .list(
            &actor_id_for_uid(&coordinator.installation_id, 1001).unwrap(),
            64,
        )
        .unwrap()
        .tasks
        .is_empty());
    assert!(matches!(
        coordinator.get(
            &actor_id_for_uid(&coordinator.installation_id, 1001).unwrap(),
            &first.task_id
        ),
        Err(GatewayDaemonError::Store(StoreError::TaskNotFound))
    ));
}

#[test]
fn task_list_rejects_a_row_owner_diverging_from_events() {
    let root = private_tempdir();
    let database_path = root.path().join("gateway.db");
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(InstallationId::new())).unwrap();
    let owner = actor_id_for_uid(&coordinator.installation_id, 1000).unwrap();
    coordinator
        .submit(&owner, submit("list-owner-corruption"))
        .unwrap();
    let forged = actor_id_for_uid(&coordinator.installation_id, 1001).unwrap();
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .execute(
            "UPDATE tasks SET owner_actor_id=?1",
            rusqlite::params![forged.as_str()],
        )
        .unwrap();
    assert!(matches!(
        coordinator.list(&forged, 64),
        Err(GatewayDaemonError::Store(StoreError::Corrupt { .. }))
    ));
}

#[test]
fn scheduler_claims_once_records_progress_and_settles() {
    let root = private_tempdir();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    let task = coordinator
        .submit(&actor, submit("scheduled-success"))
        .unwrap();
    let (factory, _) = fake_factory([
        RuntimePoll::Update {
            sequence: 2,
            update: RuntimeUpdate::Progress {
                summary: BoundedText::new("working").unwrap(),
            },
        },
        RuntimePoll::Succeeded,
    ]);
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-a").unwrap(),
        factory,
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);

    assert!(matches!(
        scheduler.tick(started_at).unwrap(),
        SchedulerTick::Started(TaskView {
            state: TaskState::Running,
            ..
        })
    ));
    assert!(matches!(
        scheduler.tick(started_at + 1).unwrap(),
        SchedulerTick::Progressed(_)
    ));
    assert!(matches!(
        scheduler.tick(started_at + 2).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Succeeded,
            ..
        })
    ));
    assert_eq!(
        coordinator.get(&actor, &task.task_id).unwrap().state,
        TaskState::Succeeded
    );
}

#[test]
fn retryable_failure_requires_explicit_retry_and_replays_the_new_start_intent() {
    let root = private_tempdir();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_ref_for_uid(&installation, 1000).unwrap();
    let queued = coordinator
        .submit(&actor.actor_id, submit("retryable-run"))
        .unwrap();
    let previous_run_id = queued.active_run_id.clone().unwrap();
    let submitted_candidate = coordinator
        .store
        .peek_ready_outbox(
            &scheduler::runtime_start_delivery_kind(),
            now_ms().unwrap().saturating_add(1),
        )
        .unwrap()
        .unwrap();
    let decode_catalog =
        daemon_config(root.path().join("decode.sock"), database_path.clone()).launch_catalog;
    let original_identity =
        scheduler::decode_runtime_start_intent(submitted_candidate.payload, &decode_catalog)
            .unwrap()
            .capability_profile;
    let retryable = ContractError::new(
        "runtime_busy",
        ErrorCategory::RuntimeUnavailable,
        true,
        "Runtime is temporarily unavailable",
    )
    .unwrap();
    let (factory, _) = fake_factory([RuntimePoll::Failed(retryable)]);
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation.clone()),
        BoundedOpaque::new("worker-retryable-first").unwrap(),
        factory,
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    assert!(matches!(
        scheduler.tick(started_at).unwrap(),
        SchedulerTick::Started(_)
    ));
    assert!(matches!(
        scheduler.tick(started_at + 1).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Suspended,
            ..
        })
    ));
    let suspended = coordinator.get(&actor.actor_id, &queued.task_id).unwrap();
    assert_eq!(suspended.state, TaskState::Suspended);
    assert_eq!(scheduler.tick(started_at + 2).unwrap(), SchedulerTick::Idle);

    let config = daemon_config(root.path().join("unused.sock"), database_path.clone());
    let retry = RetryTask {
        request_id: RequestId::new(),
        idempotency_key: IdempotencyKey::new("explicit-retry").unwrap(),
        task_id: queued.task_id.clone(),
        previous_run_id: previous_run_id.clone(),
        expected_revision: Some(suspended.revision),
    };
    let retried = coordinator
        .retry_admitted(
            &actor,
            &GatewayCapabilityProfile::task_only_v1().governed_target(),
            config.launch_catalog.default_workspace(),
            &brokered_core_runtime(),
            retry.clone(),
        )
        .unwrap();
    assert_eq!(retried.state, TaskState::Queued);
    let next_run_id = retried.active_run_id.clone().unwrap();
    assert_ne!(next_run_id, previous_run_id);
    let retry_candidate = coordinator
        .store
        .peek_ready_outbox(
            &scheduler::runtime_start_delivery_kind(),
            now_ms().unwrap().saturating_add(1),
        )
        .unwrap()
        .unwrap();
    let retry_intent =
        scheduler::decode_runtime_start_intent(retry_candidate.payload, &decode_catalog).unwrap();
    assert_eq!(retry_intent.run_id, next_run_id);
    assert_eq!(retry_intent.capability_profile, original_identity);
    assert_eq!(
        coordinator
            .retry_admitted(
                &actor,
                &GatewayCapabilityProfile::task_only_v1().governed_target(),
                config.launch_catalog.default_workspace(),
                &brokered_core_runtime(),
                retry.clone(),
            )
            .unwrap(),
        retried
    );

    let mut conflicting = retry;
    conflicting.previous_run_id = next_run_id.clone();
    assert!(matches!(
        coordinator.retry_admitted(
            &actor,
            &GatewayCapabilityProfile::task_only_v1().governed_target(),
            config.launch_catalog.default_workspace(),
            &brokered_core_runtime(),
            conflicting,
        ),
        Err(GatewayDaemonError::Store(StoreError::IdempotencyConflict))
    ));

    let started = Arc::new(Mutex::new(Vec::new()));
    let factory = FakeFactory {
        polls: Some(VecDeque::from([RuntimePoll::Succeeded])),
        started: Arc::clone(&started),
        cancellations: Arc::new(AtomicUsize::new(0)),
        ack_sabotage_path: None,
    };
    let mut retry_scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-retryable-second").unwrap(),
        factory,
    )
    .unwrap();
    let retry_started_at = now_ms().unwrap().saturating_add(1);
    assert!(matches!(
        retry_scheduler.tick(retry_started_at).unwrap(),
        SchedulerTick::Started(_)
    ));
    assert_eq!(started.lock().unwrap()[0].run_id, next_run_id);
    assert!(matches!(
        retry_scheduler.tick(retry_started_at + 1).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Succeeded,
            ..
        })
    ));
}

#[test]
fn retry_waits_for_crash_window_lease_recovery_before_queueing() {
    let root = private_tempdir();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_ref_for_uid(&installation, 1000).unwrap();
    let queued = coordinator
        .submit(&actor.actor_id, submit("retry-after-release-crash"))
        .unwrap();
    let previous_run_id = queued.active_run_id.clone().unwrap();
    let retryable = ContractError::new(
        "runtime_busy",
        ErrorCategory::RuntimeUnavailable,
        true,
        "Runtime is temporarily unavailable",
    )
    .unwrap();
    let (factory, _) = fake_factory([RuntimePoll::Failed(retryable)]);
    let config = TaskSchedulerConfig {
        lease_duration: Duration::from_millis(200),
        lease_renewal_margin: Duration::from_millis(50),
        runtime_operation_timeout: Duration::from_millis(50),
    };
    let mut scheduler = TaskScheduler::open_with_config(
        &database_path,
        Some(installation.clone()),
        BoundedOpaque::new("worker-release-crash").unwrap(),
        factory,
        config,
    )
    .unwrap();
    scheduler.fail_next_terminal_lease_release_for_test();
    scheduler.tick(now_ms().unwrap()).unwrap();
    assert!(matches!(
        scheduler.tick(now_ms().unwrap()),
        Err(GatewayDaemonError::Protocol(ref message))
            if message == "injected failure before terminal Run lease release"
    ));
    let suspended = coordinator.get(&actor.actor_id, &queued.task_id).unwrap();
    assert_eq!(suspended.state, TaskState::Suspended);
    let stale_lease = coordinator.store.load_run_lease(&previous_run_id).unwrap();
    assert!(stale_lease.expires_at_ms > now_ms().unwrap());

    let admission = daemon_config(root.path().join("unused.sock"), database_path.clone());
    let retry = RetryTask {
        request_id: RequestId::new(),
        idempotency_key: IdempotencyKey::new("explicit-retry-after-release-crash").unwrap(),
        task_id: queued.task_id.clone(),
        previous_run_id: previous_run_id.clone(),
        expected_revision: Some(suspended.revision),
    };
    let blocked = coordinator.retry_admitted(
        &actor,
        &GatewayCapabilityProfile::task_only_v1().governed_target(),
        admission.launch_catalog.default_workspace(),
        &brokered_core_runtime(),
        retry.clone(),
    );
    assert!(
        matches!(
            blocked,
            Err(GatewayDaemonError::Store(StoreError::LedgerConflict { .. }))
        ),
        "unexpected retry result: {blocked:?}"
    );
    assert_eq!(
        coordinator.get(&actor.actor_id, &queued.task_id).unwrap(),
        suspended
    );

    drop(scheduler);
    while now_ms().unwrap() <= stale_lease.expires_at_ms {
        std::thread::sleep(Duration::from_millis(5));
    }
    let (factory, _) = fake_factory([]);
    let mut recovery = TaskScheduler::open_with_config(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-release-recovery").unwrap(),
        factory,
        config,
    )
    .unwrap();
    assert!(matches!(
        recovery.tick(now_ms().unwrap()).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Suspended,
            ..
        })
    ));
    assert!(
        coordinator
            .store
            .load_run_lease(&previous_run_id)
            .unwrap()
            .generation
            > stale_lease.generation
    );
    drop(recovery);

    let retried = coordinator
        .retry_admitted(
            &actor,
            &GatewayCapabilityProfile::task_only_v1().governed_target(),
            admission.launch_catalog.default_workspace(),
            &brokered_core_runtime(),
            retry,
        )
        .unwrap();
    assert_eq!(retried.state, TaskState::Queued);
    assert_ne!(retried.active_run_id.as_ref(), Some(&previous_run_id));
}

#[test]
fn terminal_tasks_are_never_reopened_by_retry() {
    let root = private_tempdir();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_ref_for_uid(&installation, 1000).unwrap();
    let queued = coordinator
        .submit(&actor.actor_id, submit("terminal-run"))
        .unwrap();
    let previous_run_id = queued.active_run_id.clone().unwrap();
    let terminal = ContractError::new(
        "invalid_runtime_request",
        ErrorCategory::InvalidRequest,
        false,
        "Runtime request is invalid",
    )
    .unwrap();
    let (factory, _) = fake_factory([RuntimePoll::Failed(terminal)]);
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-terminal").unwrap(),
        factory,
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();
    assert!(matches!(
        scheduler.tick(started_at + 1).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Failed,
            ..
        })
    ));

    let config = daemon_config(root.path().join("unused.sock"), database_path);
    assert!(matches!(
        coordinator.retry_admitted(
            &actor,
            &GatewayCapabilityProfile::task_only_v1().governed_target(),
            config.launch_catalog.default_workspace(),
            &brokered_core_runtime(),
            RetryTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("forbidden-terminal-retry").unwrap(),
                task_id: queued.task_id,
                previous_run_id,
                expected_revision: None,
            },
        ),
        Err(GatewayDaemonError::Protocol(_))
    ));

    let succeeded = coordinator
        .submit(&actor.actor_id, submit("succeeded-run"))
        .unwrap();
    let succeeded_run_id = succeeded.active_run_id.clone().unwrap();
    let (factory, _) = fake_factory([RuntimePoll::Succeeded]);
    let mut scheduler = TaskScheduler::open(
        &config.database_path,
        None,
        BoundedOpaque::new("worker-succeeded").unwrap(),
        factory,
    )
    .unwrap();
    let succeeded_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(succeeded_at).unwrap();
    assert!(matches!(
        scheduler.tick(succeeded_at + 1).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Succeeded,
            ..
        })
    ));
    assert!(matches!(
        coordinator.retry_admitted(
            &actor,
            &GatewayCapabilityProfile::task_only_v1().governed_target(),
            config.launch_catalog.default_workspace(),
            &brokered_core_runtime(),
            RetryTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("forbidden-succeeded-retry").unwrap(),
                task_id: succeeded.task_id,
                previous_run_id: succeeded_run_id,
                expected_revision: None,
            },
        ),
        Err(GatewayDaemonError::Protocol(_))
    ));

    let cancelled = coordinator
        .submit(&actor.actor_id, submit("cancelled-run"))
        .unwrap();
    let cancelled_run_id = cancelled.active_run_id.clone().unwrap();
    let cancelled = coordinator
        .cancel(
            &actor.actor_id,
            CancelTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("cancel-before-retry").unwrap(),
                task_id: cancelled.task_id,
                run_id: cancelled_run_id.clone(),
                expected_revision: Some(cancelled.revision),
            },
        )
        .unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert!(matches!(
        coordinator.retry_admitted(
            &actor,
            &GatewayCapabilityProfile::task_only_v1().governed_target(),
            config.launch_catalog.default_workspace(),
            &brokered_core_runtime(),
            RetryTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("forbidden-cancelled-retry").unwrap(),
                task_id: cancelled.task_id,
                previous_run_id: cancelled_run_id,
                expected_revision: None,
            },
        ),
        Err(GatewayDaemonError::Protocol(_))
    ));
}

#[test]
fn scheduler_forwards_running_cancel_before_polling_again() {
    let root = private_tempdir();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    let queued = coordinator
        .submit(&actor, submit("scheduled-cancel"))
        .unwrap();
    let (factory, cancellations) = fake_factory([RuntimePoll::Pending]);
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-cancel").unwrap(),
        factory,
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(started_at).unwrap();
    let running = coordinator.get(&actor, &queued.task_id).unwrap();
    let requested = coordinator
        .cancel(
            &actor,
            CancelTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("cancel-running").unwrap(),
                task_id: queued.task_id.clone(),
                run_id: queued.active_run_id.unwrap(),
                expected_revision: Some(running.revision),
            },
        )
        .unwrap();
    assert_eq!(requested.state, TaskState::Running);

    assert!(matches!(
        scheduler.tick(started_at + 1).unwrap(),
        SchedulerTick::Settled(TaskView {
            state: TaskState::Cancelled,
            ..
        })
    ));
    assert_eq!(cancellations.load(Ordering::Relaxed), 1);
}

#[test]
fn scheduler_cancels_runtime_when_start_ack_is_fenced() {
    let root = private_tempdir();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    let queued = coordinator
        .submit(&actor, submit("start-ack-fenced"))
        .unwrap();
    let (mut factory, cancellations) = fake_factory([RuntimePoll::Pending]);
    factory.ack_sabotage_path = Some(database_path.clone());
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-ack-failure").unwrap(),
        factory,
    )
    .unwrap();

    assert!(matches!(
        scheduler.tick(now_ms().unwrap().saturating_add(1)),
        Err(GatewayDaemonError::Store(
            StoreError::GenerationFenced { .. }
        ))
    ));
    assert_eq!(cancellations.load(Ordering::Relaxed), 1);
    assert_eq!(
        coordinator.get(&actor, &queued.task_id).unwrap().state,
        TaskState::Failed
    );
}

#[test]
fn second_scheduler_cannot_redeliver_a_started_run() {
    let root = private_tempdir();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor, submit("single-delivery"))
        .unwrap();
    let (first_factory, _) = fake_factory([RuntimePoll::Pending]);
    let (second_factory, _) = fake_factory([RuntimePoll::Pending]);
    let mut first = TaskScheduler::open(
        &database_path,
        Some(installation.clone()),
        BoundedOpaque::new("worker-first").unwrap(),
        first_factory,
    )
    .unwrap();
    let mut second = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-second").unwrap(),
        second_factory,
    )
    .unwrap();
    let started_at = now_ms().unwrap().saturating_add(1);
    assert!(matches!(
        first.tick(started_at).unwrap(),
        SchedulerTick::Started(_)
    ));
    assert_eq!(second.tick(started_at + 1).unwrap(), SchedulerTick::Idle);
}

#[test]
fn local_client_controls_durable_task_through_authenticated_socket() {
    let root = private_tempdir();
    let socket_dir = private_directory(&root, "runtime");
    let socket_path = socket_dir.join("gateway.sock");
    let database_path = root.path().join("gateway.db");
    let mut daemon =
        GatewayDaemon::bind(daemon_config(socket_path.clone(), database_path)).unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let server_shutdown = Arc::clone(&shutdown);
    let server = std::thread::spawn(move || daemon.serve_until(&server_shutdown));

    let client = LocalGatewayClient::new(socket_path.clone());
    assert_eq!(client.ping(RequestId::new()).unwrap(), GatewayResult::Pong);
    let request = submit("submit-once");
    let first = client.submit(request.clone()).unwrap();
    let GatewayResult::Task(task) = first else {
        panic!("submit must return a Task")
    };
    assert_eq!(task.state, TaskState::Queued);
    let GatewayResult::Task(replay) = client.submit(request).unwrap() else {
        panic!("replay must return a Task")
    };
    assert_eq!(replay.task_id, task.task_id);

    let GatewayResult::Tasks(tasks) = client.list(RequestId::new(), 64).unwrap() else {
        panic!("list must return authorized Tasks")
    };
    assert_eq!(tasks.tasks, vec![task.clone()]);

    let GatewayResult::Events(page) = client
        .events(RequestId::new(), task.task_id.clone(), None, 1)
        .unwrap()
    else {
        panic!("events must return a page")
    };
    assert_eq!(page.events.len(), 1);
    assert!(page.has_more);

    let run_id = task.active_run_id.clone().unwrap();
    let GatewayResult::Cancelled(cancelled) = client
        .cancel(CancelTask {
            request_id: RequestId::new(),
            idempotency_key: IdempotencyKey::new("cancel-once").unwrap(),
            task_id: task.task_id.clone(),
            run_id: run_id.clone(),
            expected_revision: Some(task.revision),
        })
        .unwrap()
    else {
        panic!("cancel must return a projection")
    };
    assert_eq!(cancelled.state, TaskState::Cancelled);
    let GatewayResult::Cancelled(replayed) = client
        .cancel(CancelTask {
            request_id: RequestId::new(),
            idempotency_key: IdempotencyKey::new("cancel-once").unwrap(),
            task_id: task.task_id,
            run_id,
            expected_revision: Some(task.revision),
        })
        .unwrap()
    else {
        panic!("cancel replay must return a projection")
    };
    assert_eq!(replayed, cancelled);

    shutdown.store(true, Ordering::Relaxed);
    server.join().unwrap().unwrap();
    assert!(!socket_path.exists());
}

#[test]
fn local_api_replays_submit_after_response_loss_and_rejects_digest_change() {
    let root = private_tempdir();
    let socket_dir = private_directory(&root, "runtime");
    let socket_path = socket_dir.join("gateway.sock");
    let database_path = root.path().join("gateway.db");
    let mut daemon =
        GatewayDaemon::bind(daemon_config(socket_path.clone(), database_path)).unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let server_shutdown = Arc::clone(&shutdown);
    let server = std::thread::spawn(move || daemon.serve_until(&server_shutdown));

    let mut request = submit("lost-submit-response");
    let mut abandoned = UnixStream::connect(&socket_path).unwrap();
    write_frame(
        &mut abandoned,
        &GatewayRequest::Submit {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request: request.clone(),
        },
    )
    .unwrap();
    drop(abandoned);

    request.request_id = RequestId::new();
    let client = LocalGatewayClient::new(socket_path.clone());
    let GatewayResult::Task(replayed) = client.submit(request.clone()).unwrap() else {
        panic!("retry after response loss must return the durable Task")
    };
    assert_eq!(replayed.state, TaskState::Queued);

    request.request_id = RequestId::new();
    request.intent = BoundedText::new("different command digest").unwrap();
    assert!(matches!(
        client.submit(request),
        Err(GatewayDaemonError::Remote {
            code,
            recoverable: false,
            ..
        }) if code == "idempotency_conflict"
    ));

    let GatewayResult::Events(events) = client
        .events(RequestId::new(), replayed.task_id, None, 16)
        .unwrap()
    else {
        panic!("replayed Task must retain its original event stream")
    };
    assert_eq!(events.events.len(), 2);
    assert!(!events.has_more);

    shutdown.store(true, Ordering::Relaxed);
    server.join().unwrap().unwrap();
    assert!(!socket_path.exists());
}

#[test]
fn local_api_queues_only_an_explicit_retry_and_replays_it() {
    let root = private_tempdir();
    let socket_dir = private_directory(&root, "runtime");
    let socket_path = socket_dir.join("gateway.sock");
    let database_path = root.path().join("gateway.db");
    let mut daemon =
        GatewayDaemon::bind(daemon_config(socket_path.clone(), database_path.clone())).unwrap();
    let installation = daemon.installation_id().clone();
    let actor = actor_id_for_uid(&installation, Uid::effective().as_raw()).unwrap();
    let queued = daemon
        .coordinator
        .submit(&actor, submit("local-explicit-retry"))
        .unwrap();
    let previous_run_id = queued.active_run_id.clone().unwrap();
    let retryable = ContractError::new(
        "runtime_busy",
        ErrorCategory::RuntimeUnavailable,
        true,
        "Runtime is temporarily unavailable",
    )
    .unwrap();
    let (factory, _) = fake_factory([RuntimePoll::Failed(retryable)]);
    let mut scheduler = TaskScheduler::open(
        &database_path,
        Some(installation),
        BoundedOpaque::new("worker-local-retry").unwrap(),
        factory,
    )
    .unwrap();
    let failed_at = now_ms().unwrap().saturating_add(1);
    scheduler.tick(failed_at).unwrap();
    scheduler.tick(failed_at + 1).unwrap();
    assert_eq!(scheduler.tick(failed_at + 2).unwrap(), SchedulerTick::Idle);
    let suspended = daemon.coordinator.get(&actor, &queued.task_id).unwrap();
    assert_eq!(suspended.state, TaskState::Suspended);
    drop(scheduler);

    let shutdown = Arc::new(AtomicBool::new(false));
    let server_shutdown = Arc::clone(&shutdown);
    let server = std::thread::spawn(move || daemon.serve_until(&server_shutdown));
    let client = LocalGatewayClient::new(socket_path.clone());
    let mut request = RetryTask {
        request_id: RequestId::new(),
        idempotency_key: IdempotencyKey::new("local-retry-once").unwrap(),
        task_id: queued.task_id.clone(),
        previous_run_id: previous_run_id.clone(),
        expected_revision: Some(suspended.revision),
    };
    let GatewayResult::Retried(retried) = client.retry(request.clone()).unwrap() else {
        panic!("explicit retry must return the queued replacement Run")
    };
    assert_eq!(retried.state, TaskState::Queued);
    assert_ne!(retried.active_run_id.as_ref(), Some(&previous_run_id));

    request.request_id = RequestId::new();
    let GatewayResult::Retried(replayed) = client.retry(request.clone()).unwrap() else {
        panic!("same retry key and digest must replay")
    };
    assert_eq!(replayed, retried);

    request.request_id = RequestId::new();
    request.previous_run_id = retried.active_run_id.unwrap();
    assert!(matches!(
        client.retry(request),
        Err(GatewayDaemonError::Remote {
            code,
            recoverable: false,
            ..
        }) if code == "idempotency_conflict"
    ));

    shutdown.store(true, Ordering::Relaxed);
    server.join().unwrap().unwrap();
    assert!(!socket_path.exists());
}

#[test]
fn slow_same_uid_connections_cannot_starve_scheduler_or_following_client() {
    let root = private_tempdir();
    let socket_dir = private_directory(&root, "runtime");
    let socket_path = socket_dir.join("gateway.sock");
    let database_path = root.path().join("gateway.db");
    let mut daemon =
        GatewayDaemon::bind(daemon_config(socket_path.clone(), database_path.clone())).unwrap();
    let installation = daemon.installation_id().clone();
    let actor = actor_id_for_uid(&installation, Uid::effective().as_raw()).unwrap();
    let queued = daemon
        .coordinator
        .submit(&actor, submit("slow-client-starvation"))
        .unwrap();
    let (factory, _) = fake_factory([RuntimePoll::Pending, RuntimePoll::Succeeded]);
    daemon.scheduler = Some(
        TaskScheduler::open(
            &database_path,
            Some(installation),
            BoundedOpaque::new("worker-slow-client").unwrap(),
            Box::new(factory) as Box<dyn RuntimeFactory>,
        )
        .unwrap(),
    );

    // Queue both hostile clients before the serve loop starts so the valid
    // request cannot bypass either serial admission quantum.
    let idle = UnixStream::connect(&socket_path).unwrap();
    let mut partial = UnixStream::connect(&socket_path).unwrap();
    partial.write_all(&64_u32.to_be_bytes()).unwrap();
    partial.write_all(b"{").unwrap();

    let shutdown = Arc::new(AtomicBool::new(false));
    let server_shutdown = Arc::clone(&shutdown);
    let server = std::thread::spawn(move || daemon.serve_until(&server_shutdown));
    let started = Instant::now();
    let result = LocalGatewayClient::new(socket_path.clone())
        .get(RequestId::new(), queued.task_id)
        .unwrap();

    let GatewayResult::Task(task) = result else {
        panic!("valid request after slow clients must return a Task")
    };
    assert_eq!(task.state, TaskState::Succeeded);
    assert!(started.elapsed() < Duration::from_secs(4));

    drop(idle);
    drop(partial);
    shutdown.store(true, Ordering::Relaxed);
    server.join().unwrap().unwrap();
    assert!(!socket_path.exists());
}

#[test]
fn database_rejects_installation_identity_substitution() {
    let root = private_tempdir();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    assert_eq!(coordinator.installation_id, installation);
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator
        .submit(&actor, submit("migration-history"))
        .unwrap();
    drop(coordinator);

    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute("DELETE FROM gateway_identity", [])
        .unwrap();
    drop(connection);
    let reopened = TaskCoordinator::open(&database_path, None).unwrap();
    assert_eq!(reopened.installation_id, installation);
    drop(reopened);

    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute("DELETE FROM gateway_identity", [])
        .unwrap();
    drop(connection);
    assert!(matches!(
        TaskCoordinator::open(&database_path, Some(InstallationId::new())),
        Err(GatewayDaemonError::Store(StoreError::LedgerConflict { .. }))
    ));
}

#[test]
fn database_rejects_mixed_installation_history() {
    let root = private_tempdir();
    let database_path = root.path().join("gateway.db");
    let installation = InstallationId::new();
    let mut coordinator =
        TaskCoordinator::open(&database_path, Some(installation.clone())).unwrap();
    let actor = actor_id_for_uid(&installation, 1000).unwrap();
    coordinator.submit(&actor, submit("mixed-history")).unwrap();
    drop(coordinator);

    let connection = rusqlite::Connection::open(&database_path).unwrap();
    let payload: String = connection
        .query_row(
            "SELECT payload_json FROM task_events WHERE revision = 2",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut payload = serde_json::from_str::<serde_json::Value>(&payload).unwrap();
    payload["header"]["correlation"]["installation_id"] =
        serde_json::Value::String(InstallationId::new().to_string());
    connection
        .execute(
            "UPDATE task_events SET payload_json = ?1 WHERE revision = 2",
            [serde_json::to_string(&payload).unwrap()],
        )
        .unwrap();
    connection
        .execute("DELETE FROM gateway_identity", [])
        .unwrap();
    drop(connection);

    assert!(matches!(
        TaskCoordinator::open(&database_path, None),
        Err(GatewayDaemonError::Store(StoreError::Corrupt { .. }))
    ));
}

#[test]
fn bind_never_unlinks_a_regular_file() {
    let root = private_tempdir();
    let socket_dir = private_directory(&root, "runtime");
    let socket_path = socket_dir.join("gateway.sock");
    fs::write(&socket_path, b"do not remove").unwrap();
    let result = GatewayDaemon::bind(daemon_config(
        socket_path.clone(),
        root.path().join("gateway.db"),
    ));
    assert!(matches!(result, Err(GatewayDaemonError::UnsafePath { .. })));
    assert_eq!(fs::read(socket_path).unwrap(), b"do not remove");
}

#[test]
fn daemon_admission_accepts_only_the_exact_brokered_core_selector() {
    let mut request = submit("brokered-core-admission");
    let admitted = brokered_core_runtime();
    request.runtime = admitted.clone();

    assert!(supported_daemon_runtime(
        GatewayCapabilityProfile::task_only_v1(),
        &admitted
    ));
    let catalog = daemon_config(
        PathBuf::from("/tmp/unused-gateway.sock"),
        PathBuf::from("/tmp/unused-gateway.db"),
    )
    .launch_catalog;
    assert!(handler::validate_submission_admission(&request, &catalog).is_ok());

    request.runtime = acp_runtime();
    assert!(!supported_daemon_runtime(
        GatewayCapabilityProfile::task_only_v1(),
        &request.runtime
    ));
    assert!(matches!(
        handler::validate_submission_admission(&request, &catalog),
        Err(GatewayDaemonError::Protocol(_))
    ));
}

#[test]
fn task_handler_has_no_execution_or_persistence_dependencies() {
    let source = include_str!("handler.rs");
    for forbidden in [
        "crate::storage",
        "storage::",
        "scheduler::",
        "crate::runtime",
        "runtime::",
        "std::process",
        "process::",
        "pty::",
        "ExecutionTarget",
        "Pty",
        "PTY",
    ] {
        assert!(
            !source.contains(forbidden),
            "Task transport handler imports forbidden boundary {forbidden}"
        );
    }
}

#[test]
fn bind_accepts_an_acp_only_launch_catalog() {
    let root = private_tempdir();
    let socket_dir = private_directory(&root, "runtime");
    let socket_path = socket_dir.join("gateway.sock");
    let database_path = root.path().join("gateway.db");
    let config = delegated_daemon_config(socket_path.clone(), database_path.clone());

    let result = GatewayDaemon::bind(config);

    assert!(result.is_ok());
    assert!(socket_path.exists());
    assert!(database_path.exists());
}

#[test]
fn bind_accepts_the_exact_brokered_core_selector() {
    let root = private_tempdir();
    let socket_dir = private_directory(&root, "runtime");
    let socket_path = socket_dir.join("gateway.sock");
    let database_path = root.path().join("gateway.db");

    let daemon = GatewayDaemon::bind(daemon_config(socket_path.clone(), database_path.clone()))
        .expect("the exact brokered Core selector must be admitted");

    assert!(socket_path.exists());
    assert!(database_path.exists());
    drop(daemon);
    assert!(!socket_path.exists());
}

#[test]
fn bind_accepts_the_exact_delegated_acp_selector() {
    let root = private_tempdir();
    let socket_dir = private_directory(&root, "runtime");
    let socket_path = socket_dir.join("gateway.sock");
    let database_path = root.path().join("gateway.db");

    let daemon = GatewayDaemon::bind(delegated_daemon_config(
        socket_path.clone(),
        database_path.clone(),
    ))
    .expect("the exact delegated ACP selector must be admitted");

    assert!(socket_path.exists());
    assert!(database_path.exists());
    drop(daemon);
    assert!(!socket_path.exists());
}

#[test]
fn bind_replaces_only_an_owned_stale_socket() {
    let root = private_tempdir();
    let socket_dir = private_directory(&root, "runtime");
    let socket_path = socket_dir.join("gateway.sock");
    let stale = UnixListener::bind(&socket_path).unwrap();
    drop(stale);
    let daemon = GatewayDaemon::bind(daemon_config(
        socket_path.clone(),
        root.path().join("gateway.db"),
    ))
    .unwrap();
    assert!(socket_path.exists());
    drop(daemon);
    assert!(!socket_path.exists());
}

#[path = "tests/snapshot.rs"]
mod snapshot;
