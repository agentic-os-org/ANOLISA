//! Debug-only process-kill evidence for Gateway durability boundaries.

#![cfg(debug_assertions)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use cosh_gateway::storage::{
    ApprovalRecord, ApprovalResolution, ApprovalState, BrokerExecutionState, CommitOutcome,
    ExecutionClaim, ExecutionCompletion, ExecutionState, LeaseClaim, LeaseCommand, LedgerCommand,
    LedgerOutcome, OutboxIntent, PermitState, SecurityAuditProof, SqliteTaskStore, TaskCommit,
};
use cosh_gateway_contracts::capability::{
    ApprovalDecision, ApprovalRequest, BrokeredOperation, CapabilityRequest, CapabilityScope,
    ExecutionPermit, OperationDescriptor, RuntimeExecutionFence, WorkspaceCheckpointCreateV1,
};
use cosh_gateway_contracts::common::{
    ActorKind, ActorRef, AuthAssurance, BoundedName, BoundedOpaque, BoundedText, ContractHeader,
    ContractSchema, Correlation, Digest, IdempotencyKey, RuntimeBindingRef, RuntimeSelector,
    TargetRef,
};
use cosh_gateway_contracts::external::{ExternalRef, ExternalRefKind};
use cosh_gateway_contracts::ids::{
    ActorId, AgentSessionId, ApprovalId, CheckpointId, DeliveryId, ExecutionId, InstallationId,
    MessageId, PermitId, RequestId, RunId, RuntimeBindingId, RuntimeInstanceId, TaskId,
};
use cosh_gateway_contracts::task::{TaskEvent, TaskEventEnvelope};

const CHILD_POINT: &str = "COSH_GATEWAY_KILL_POINT";
const DB_PATH: &str = "COSH_GATEWAY_KILL_DB";
const BARRIER_PATH: &str = "COSH_GATEWAY_KILL_BARRIER";
const MARKER_PATH: &str = "COSH_GATEWAY_KILL_MARKER";
const ACTOR_ID: &str = "COSH_GATEWAY_KILL_ACTOR";
const TASK_ID: &str = "COSH_GATEWAY_KILL_TASK";
const EVENT_ID: &str = "COSH_GATEWAY_KILL_EVENT";
const DELIVERY_ID: &str = "COSH_GATEWAY_KILL_DELIVERY";
const INSTALLATION_ID: &str = "COSH_GATEWAY_KILL_INSTALLATION";
const PERMIT_ID: &str = "COSH_GATEWAY_KILL_PERMIT";

fn digest(byte: char) -> Digest {
    Digest::parse(byte.to_string().repeat(64)).unwrap()
}

fn target() -> TargetRef {
    TargetRef {
        kind: BoundedName::new("local").unwrap(),
        authority: BoundedName::new("kill-point-test").unwrap(),
        identifier: BoundedOpaque::new("target").unwrap(),
    }
}

fn ledger_command(actor_id: &ActorId, key: &str, byte: char, now_ms: u64) -> LedgerCommand {
    LedgerCommand {
        actor_id: actor_id.clone(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        command_digest: digest(byte),
        committed_at_ms: now_ms,
    }
}

fn task_commit(
    actor_id: &ActorId,
    task_id: &TaskId,
    installation_id: &InstallationId,
    event_id: &MessageId,
    delivery_id: &DeliveryId,
) -> TaskCommit {
    let mut correlation = Correlation::new(installation_id.clone());
    correlation.actor_id = Some(actor_id.clone());
    correlation.task_id = Some(task_id.clone());
    let event = TaskEventEnvelope {
        header: ContractHeader::new(ContractSchema::TaskEvent, event_id.clone(), 10, correlation),
        task_id: task_id.clone(),
        revision: 1,
        event: TaskEvent::TaskSubmitted {
            intent_digest: digest('a'),
            target: target(),
        },
    };
    TaskCommit {
        actor_id: actor_id.clone(),
        idempotency_key: IdempotencyKey::new("kill-task-commit").unwrap(),
        command_digest: digest('b'),
        expected_revision: Some(0),
        events: vec![event.clone()],
        outbox: vec![OutboxIntent {
            delivery_id: delivery_id.clone(),
            event_id: event.header.message_id,
            delivery_kind: BoundedName::new("task_event").unwrap(),
            payload: serde_json::json!({"event": "submitted"}),
            next_attempt_at_ms: 10,
        }],
        committed_at_ms: 10,
    }
}

fn create_running_task(store: &mut SqliteTaskStore, actor_id: &ActorId, run_id: &RunId) -> TaskId {
    let task_id = TaskId::new();
    let mut correlation = Correlation::new(InstallationId::new());
    correlation.actor_id = Some(actor_id.clone());
    correlation.task_id = Some(task_id.clone());
    let envelope = |revision, event| TaskEventEnvelope {
        header: ContractHeader::new(
            ContractSchema::TaskEvent,
            MessageId::new(),
            1,
            correlation.clone(),
        ),
        task_id: task_id.clone(),
        revision,
        event,
    };
    let events = vec![
        envelope(
            1,
            TaskEvent::TaskSubmitted {
                intent_digest: digest('0'),
                target: target(),
            },
        ),
        envelope(
            2,
            TaskEvent::TaskQueued {
                run_id: run_id.clone(),
                runtime: RuntimeSelector {
                    runtime: BoundedName::new("acp").unwrap(),
                    profile: Some(BoundedName::new("kill-test").unwrap()),
                },
            },
        ),
        envelope(
            3,
            TaskEvent::RunStarted {
                run_id: run_id.clone(),
            },
        ),
    ];
    assert!(matches!(
        store
            .commit_task_for_test(&TaskCommit {
                actor_id: actor_id.clone(),
                idempotency_key: IdempotencyKey::new(format!("task-{}", task_id.as_str())).unwrap(),
                command_digest: digest('1'),
                expected_revision: Some(0),
                events,
                outbox: Vec::new(),
                committed_at_ms: 1,
            })
            .unwrap(),
        CommitOutcome::Applied(_)
    ));
    task_id
}

fn runtime_binding(task_id: &TaskId, run_id: &RunId, generation: u64) -> RuntimeBindingRef {
    RuntimeBindingRef {
        binding_id: RuntimeBindingId::new(),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        agent_session_id: AgentSessionId::new(),
        runtime_instance_id: RuntimeInstanceId::new(),
        runtime_generation: generation,
        external_session: ExternalRef {
            kind: ExternalRefKind::AcpSession,
            authority: BoundedName::new("kill-test").unwrap(),
            scope_digest: digest('3'),
            value: BoundedOpaque::new("session").unwrap(),
        },
    }
}

fn prepare_permit(path: &Path) -> (ActorId, ExecutionPermit) {
    let mut store = SqliteTaskStore::open(path).unwrap();
    let actor_id = ActorId::new();
    let run_id = RunId::new();
    let task_id = create_running_task(&mut store, &actor_id, &run_id);
    let lease_command = LeaseCommand {
        command: ledger_command(&actor_id, "kill-lease", '2', 2),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        lease_owner: BoundedOpaque::new("kill-worker").unwrap(),
        expires_at_ms: 200,
    };
    let LedgerOutcome::Applied(lease) = store.acquire_run_lease(&lease_command).unwrap() else {
        panic!("lease setup must apply")
    };
    let claim = LeaseClaim {
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        lease_owner: lease.lease_owner,
        generation: lease.generation,
        revision: lease.revision,
    };
    let binding = runtime_binding(&task_id, &run_id, claim.generation);
    store
        .bind_runtime(
            &ledger_command(&actor_id, "kill-bind", '3', 3),
            &binding,
            &claim,
        )
        .unwrap();
    store
        .record_runtime_sequence(
            &binding.binding_id,
            &binding.runtime_instance_id,
            binding.runtime_generation,
            1,
            4,
            &claim,
        )
        .unwrap();

    let approval_id = ApprovalId::new();
    let request_id = RequestId::new();
    let operation_digest = digest('4');
    let input_digest = digest('5');
    let identity_digest = digest('6');
    let fence = RuntimeExecutionFence {
        binding_id: binding.binding_id,
        runtime_generation: binding.runtime_generation,
        lease_generation: claim.generation,
        lease_revision: claim.revision,
    };
    let operation = BrokeredOperation::WorkspaceCheckpointCreateV1(WorkspaceCheckpointCreateV1 {
        checkpoint_id: CheckpointId::new(),
    });
    let request = CapabilityRequest {
        request_id: request_id.clone(),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        actor: ActorRef {
            actor_id: actor_id.clone(),
            actor_kind: ActorKind::Human,
            issuer: BoundedName::new("local-os").unwrap(),
            assurance: AuthAssurance::LocalOs,
        },
        target: target(),
        operation: OperationDescriptor {
            namespace: BoundedName::new("workspace").unwrap(),
            name: BoundedName::new("checkpoint_create").unwrap(),
            arguments_digest: operation_digest.clone(),
        },
        operation_digest: operation_digest.clone(),
        requested_scope: CapabilityScope {
            resource: BoundedName::new("workspace").unwrap(),
            access: BoundedName::new("checkpoint").unwrap(),
        },
        input_digest: input_digest.clone(),
        expires_at_ms: 100,
    };
    let approval_request = ApprovalRequest {
        approval_id: approval_id.clone(),
        request_id: request_id.clone(),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        summary: BoundedText::new("approve kill-point fixture").unwrap(),
        expires_at_ms: 100,
    };
    let approval = ApprovalRecord {
        approval_id: approval_id.clone(),
        request_id: request_id.clone(),
        actor_id: actor_id.clone(),
        task_id: task_id.clone(),
        run_id: run_id.clone(),
        target: target(),
        target_identity_digest: Some(identity_digest.clone()),
        runtime_fence: Some(fence.clone()),
        operation_digest: operation_digest.clone(),
        input_digest: input_digest.clone(),
        permission: None,
        state: ApprovalState::Pending,
        revision: 1,
        expires_at_ms: 100,
        decided_by_actor_id: None,
        created_at_ms: 10,
        updated_at_ms: 10,
    };
    store
        .create_brokered_approval(
            &ledger_command(&actor_id, "kill-approval", '7', 10),
            &request,
            &approval_request,
            &operation,
            None,
            &approval,
        )
        .unwrap();
    store
        .resolve_approval(
            &ledger_command(&actor_id, "kill-approve", '8', 20),
            &approval_id,
            1,
            ApprovalResolution::Decide(ApprovalDecision::Approve),
        )
        .unwrap();
    let permit = ExecutionPermit {
        permit_id: PermitId::new(),
        request_id,
        actor_id: actor_id.clone(),
        approval_id: Some(approval_id),
        task_id,
        run_id,
        execution_id: ExecutionId::new(),
        target: target(),
        target_identity_digest: identity_digest,
        runtime_fence: fence,
        operation_digest,
        input_digest,
        policy_revision: 1,
        valid_until_ms: 90,
        single_use: true,
    };
    store
        .issue_permit(&ledger_command(&actor_id, "kill-issue", '9', 30), &permit)
        .unwrap();
    (actor_id, permit)
}

fn execution_claim(store: &SqliteTaskStore, permit: &ExecutionPermit) -> ExecutionClaim {
    let lease = store.load_run_lease(&permit.run_id).unwrap();
    ExecutionClaim {
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
        lease: LeaseClaim {
            task_id: lease.task_id,
            run_id: lease.run_id,
            lease_owner: lease.lease_owner,
            generation: lease.generation,
            revision: lease.revision,
        },
    }
}

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap())
}

fn signal_barrier() {
    let path = env_path(BARRIER_PATH);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    file.write_all(b"durable\n").unwrap();
    file.sync_all().unwrap();
}

fn wait_for_kill() -> ! {
    loop {
        thread::park_timeout(Duration::from_secs(60));
    }
}

fn record_once(path: &Path, identity: &str) {
    match fs::read_to_string(path) {
        Ok(existing) => assert_eq!(existing, identity),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .unwrap();
            file.write_all(identity.as_bytes()).unwrap();
            file.sync_all().unwrap();
        }
        Err(error) => panic!("failed to read target marker: {error}"),
    }
}

fn child_task_commit(after_commit: bool) {
    let mut store = SqliteTaskStore::open(env_path(DB_PATH)).unwrap();
    let actor_id = ActorId::parse(std::env::var(ACTOR_ID).unwrap()).unwrap();
    let task_id = TaskId::parse(std::env::var(TASK_ID).unwrap()).unwrap();
    let event_id = MessageId::parse(std::env::var(EVENT_ID).unwrap()).unwrap();
    let delivery_id = DeliveryId::parse(std::env::var(DELIVERY_ID).unwrap()).unwrap();
    let installation_id = InstallationId::parse(std::env::var(INSTALLATION_ID).unwrap()).unwrap();
    if after_commit {
        store
            .commit_task_for_test(&task_commit(
                &actor_id,
                &task_id,
                &installation_id,
                &event_id,
                &delivery_id,
            ))
            .unwrap();
    }
    signal_barrier();
    wait_for_kill();
}

fn child_outbox(after_accept: bool) {
    let mut store = SqliteTaskStore::open(env_path(DB_PATH)).unwrap();
    let claim = store
        .claim_outbox(
            &BoundedName::new("task_event").unwrap(),
            &BoundedOpaque::new("killed-outbox-worker").unwrap(),
            20,
            30,
        )
        .unwrap()
        .unwrap();
    if after_accept {
        record_once(&env_path(MARKER_PATH), claim.delivery_id.as_str());
    }
    signal_barrier();
    wait_for_kill();
}

fn child_execution(point: &str) {
    let mut store = SqliteTaskStore::open(env_path(DB_PATH)).unwrap();
    let actor_id = ActorId::parse(std::env::var(ACTOR_ID).unwrap()).unwrap();
    let permit_id = PermitId::parse(std::env::var(PERMIT_ID).unwrap()).unwrap();
    let permit = store.load_permit_record(&permit_id).unwrap().permit;
    let claim = execution_claim(&store, &permit);
    let LedgerOutcome::Applied(claimed) = store
        .claim_execution(&ledger_command(&actor_id, "kill-claim", 'a', 40), &claim)
        .unwrap()
    else {
        panic!("execution claim must apply")
    };
    if point == "execution_claimed" {
        signal_barrier();
        wait_for_kill();
    }
    let LedgerOutcome::Applied(started) = store
        .start_claimed_execution(
            &ledger_command(&actor_id, "kill-start", 'b', 41),
            &permit.execution_id,
            claimed.revision,
            &SecurityAuditProof {
                proof_digest: digest('c'),
                persisted_at_ms: 40,
            },
        )
        .unwrap()
    else {
        panic!("execution start must apply")
    };
    record_once(&env_path(MARKER_PATH), permit.execution_id.as_str());
    if point == "execution_started" {
        signal_barrier();
        wait_for_kill();
    }
    store
        .complete_execution(
            &ledger_command(&actor_id, "kill-complete", 'd', 42),
            &ExecutionCompletion {
                execution_id: permit.execution_id,
                expected_revision: started.revision,
                succeeded: false,
                receipt_digest: digest('e'),
                safe_detail: Some(BoundedText::new("controlled test result").unwrap()),
                typed_result: None,
            },
        )
        .unwrap();
    signal_barrier();
    wait_for_kill();
}

#[test]
fn kill_point_child() {
    let Ok(point) = std::env::var(CHILD_POINT) else {
        return;
    };
    match point.as_str() {
        "task_before_commit" => child_task_commit(false),
        "task_after_commit" => child_task_commit(true),
        "outbox_before_send" => child_outbox(false),
        "outbox_after_accept" => child_outbox(true),
        "execution_claimed" | "execution_started" | "execution_completed" => {
            child_execution(&point)
        }
        other => panic!("unknown kill point: {other}"),
    }
}

fn spawn_child(
    point: &str,
    db_path: &Path,
    barrier_path: &Path,
    extra_env: &[(&str, &str)],
) -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg("kill_point_child")
        .arg("--nocapture")
        .env(CHILD_POINT, point)
        .env(DB_PATH, db_path)
        .env(BARRIER_PATH, barrier_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.spawn().unwrap()
}

fn kill_at_barrier(child: &mut Child, barrier: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !barrier.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("kill-point child exited before its barrier: {status}");
        }
        assert!(Instant::now() < deadline, "kill-point barrier timed out");
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success());
}

fn task_env<'a>(
    actor_id: &'a ActorId,
    task_id: &'a TaskId,
    event_id: &'a MessageId,
    delivery_id: &'a DeliveryId,
    installation_id: &'a InstallationId,
) -> [(&'static str, &'a str); 5] {
    [
        (ACTOR_ID, actor_id.as_str()),
        (TASK_ID, task_id.as_str()),
        (EVENT_ID, event_id.as_str()),
        (DELIVERY_ID, delivery_id.as_str()),
        (INSTALLATION_ID, installation_id.as_str()),
    ]
}

fn seed_outbox(path: &Path) -> DeliveryId {
    let actor_id = ActorId::new();
    let task_id = TaskId::new();
    let event_id = MessageId::new();
    let delivery_id = DeliveryId::new();
    let installation_id = InstallationId::new();
    let mut store = SqliteTaskStore::open(path).unwrap();
    store
        .commit_task_for_test(&task_commit(
            &actor_id,
            &task_id,
            &installation_id,
            &event_id,
            &delivery_id,
        ))
        .unwrap();
    delivery_id
}

fn create_private_dir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn sigkill_boundaries_recover_without_duplicate_effects() {
    let directory = tempfile::tempdir().unwrap();

    for (point, committed) in [("task_before_commit", false), ("task_after_commit", true)] {
        let root = directory.path().join(point);
        let db_path = root.join("gateway.db");
        create_private_dir(&root);
        let barrier = root.join("barrier");
        let actor_id = ActorId::new();
        let task_id = TaskId::new();
        let event_id = MessageId::new();
        let delivery_id = DeliveryId::new();
        let installation_id = InstallationId::new();
        let mut child = spawn_child(
            point,
            &db_path,
            &barrier,
            &task_env(
                &actor_id,
                &task_id,
                &event_id,
                &delivery_id,
                &installation_id,
            ),
        );
        kill_at_barrier(&mut child, &barrier);
        let mut store = SqliteTaskStore::open(&db_path).unwrap();
        if committed {
            assert!(store.load_task(&task_id).is_ok());
            assert!(matches!(
                store
                    .commit_task_for_test(&task_commit(
                        &actor_id,
                        &task_id,
                        &installation_id,
                        &event_id,
                        &delivery_id,
                    ))
                    .unwrap(),
                CommitOutcome::Replayed(_)
            ));
            let claim = store
                .claim_outbox(
                    &BoundedName::new("task_event").unwrap(),
                    &BoundedOpaque::new("post-restart-worker").unwrap(),
                    20,
                    30,
                )
                .unwrap()
                .unwrap();
            assert_eq!(claim.delivery_id, delivery_id);
        } else {
            assert!(store.load_task(&task_id).is_err());
            assert!(store
                .claim_outbox(
                    &BoundedName::new("task_event").unwrap(),
                    &BoundedOpaque::new("post-restart-worker").unwrap(),
                    20,
                    30,
                )
                .unwrap()
                .is_none());
        }
    }

    for point in ["outbox_before_send", "outbox_after_accept"] {
        let root = directory.path().join(point);
        create_private_dir(&root);
        let db_path = root.join("gateway.db");
        let barrier = root.join("barrier");
        let marker = root.join("receiver-marker");
        let delivery_id = seed_outbox(&db_path);
        let marker_string = marker.to_string_lossy().into_owned();
        let mut child = spawn_child(
            point,
            &db_path,
            &barrier,
            &[(MARKER_PATH, marker_string.as_str())],
        );
        kill_at_barrier(&mut child, &barrier);
        let mut store = SqliteTaskStore::open(&db_path).unwrap();
        let takeover = store
            .claim_outbox(
                &BoundedName::new("task_event").unwrap(),
                &BoundedOpaque::new("takeover-worker").unwrap(),
                30,
                40,
            )
            .unwrap()
            .unwrap();
        assert_eq!(takeover.delivery_id, delivery_id);
        assert_eq!(takeover.attempt, 2);
        record_once(&marker, takeover.delivery_id.as_str());
        store.complete_outbox(&takeover, 35).unwrap();
        assert_eq!(fs::read_to_string(&marker).unwrap(), delivery_id.as_str());
        assert!(store
            .claim_outbox(
                &BoundedName::new("task_event").unwrap(),
                &BoundedOpaque::new("third-worker").unwrap(),
                50,
                60,
            )
            .unwrap()
            .is_none());
    }

    for point in [
        "execution_claimed",
        "execution_started",
        "execution_completed",
    ] {
        let root = directory.path().join(point);
        create_private_dir(&root);
        let db_path = root.join("gateway.db");
        let barrier = root.join("barrier");
        let marker = root.join("target-marker");
        let (actor_id, permit) = prepare_permit(&db_path);
        let marker_string = marker.to_string_lossy().into_owned();
        let mut child = spawn_child(
            point,
            &db_path,
            &barrier,
            &[
                (ACTOR_ID, actor_id.as_str()),
                (PERMIT_ID, permit.permit_id.as_str()),
                (MARKER_PATH, marker_string.as_str()),
            ],
        );
        kill_at_barrier(&mut child, &barrier);
        let mut store = SqliteTaskStore::open(&db_path).unwrap();
        assert_eq!(
            store.load_permit_record(&permit.permit_id).unwrap().state,
            PermitState::Consumed
        );
        match point {
            "execution_claimed" => {
                let report = store.recover_gateway(250).unwrap();
                assert_eq!(report.executions_known_no_effect, 1);
                let record = store.load_execution_record(&permit.execution_id).unwrap();
                assert_eq!(record.state, ExecutionState::Planned);
                assert_eq!(
                    record.broker_state,
                    Some(BrokerExecutionState::KnownNoEffect)
                );
                assert!(!marker.exists());
            }
            "execution_started" => {
                let report = store.recover_gateway(250).unwrap();
                assert_eq!(report.executions_uncertain, 1);
                assert_eq!(
                    store
                        .load_execution_record(&permit.execution_id)
                        .unwrap()
                        .state,
                    ExecutionState::Uncertain
                );
                assert_eq!(
                    fs::read_to_string(&marker).unwrap(),
                    permit.execution_id.as_str()
                );
            }
            "execution_completed" => {
                let completion = ExecutionCompletion {
                    execution_id: permit.execution_id.clone(),
                    expected_revision: 3,
                    succeeded: false,
                    receipt_digest: digest('e'),
                    safe_detail: Some(BoundedText::new("controlled test result").unwrap()),
                    typed_result: None,
                };
                assert!(matches!(
                    store
                        .complete_execution(
                            &ledger_command(&actor_id, "kill-complete", 'd', 42),
                            &completion,
                        )
                        .unwrap(),
                    LedgerOutcome::Replayed(_)
                ));
                assert_eq!(
                    store
                        .load_execution_record(&permit.execution_id)
                        .unwrap()
                        .state,
                    ExecutionState::Failed
                );
                assert_eq!(
                    fs::read_to_string(&marker).unwrap(),
                    permit.execution_id.as_str()
                );
            }
            _ => unreachable!(),
        }
        let repeated_recovery = store.recover_gateway(251).unwrap();
        assert_eq!(repeated_recovery.executions_known_no_effect, 0);
        assert_eq!(repeated_recovery.executions_uncertain, 0);
        if marker.exists() {
            assert_eq!(fs::read_to_string(&marker).unwrap().lines().count(), 1);
        }
    }
}
