use std::fs;
use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use agentsight::enforcement::{
    EnforcementClient, EnforcementCoordinator, EnforcementCoordinatorError, EnforcementError,
    EnforcementStore, EnforcementStoreError,
};
use agentsight_enforcement_protocol::{
    ApplyPolicy, Binding, BindingState, Command, Effect, HealthStatus, RemoteError,
    ReplaceFailureCode, ReplaceOutcome, ReplacementPolicy, Request, Response, ResponseBody,
    ViolationEvent, read_frame, write_frame,
};
use agentsight_enforcer::{EnforcementBackend, EnforcerService, MockBackend};
use uuid::Uuid;

struct TestEnforcer {
    socket_path: PathBuf,
    database_path: PathBuf,
    backend: Arc<MockBackend>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SubscriptionState {
    AckBlocked,
    Connected,
}

struct ControlledState {
    subscription: SubscriptionState,
    health_ready: bool,
    health_message: Option<String>,
    subscribe_attempts: usize,
    apply_attempts: usize,
    apply_blocked: bool,
    reject_next_required_subscription: bool,
    detach_attempts: usize,
    detach_failures_remaining: usize,
    list_attempts: usize,
    list_failures_remaining: usize,
    reconciliation_blocked: bool,
    bindings: Vec<Binding>,
}

struct ControlledEnforcer {
    socket_path: PathBuf,
    database_path: PathBuf,
    state: Arc<(Mutex<ControlledState>, Condvar)>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl TestEnforcer {
    fn start() -> Self {
        let id = Uuid::new_v4();
        let socket_path = PathBuf::from(format!("/tmp/agentsight-pipeline-{id}.sock"));
        let database_path = PathBuf::from(format!("/tmp/agentsight-enforcement-{id}.db"));
        let backend = Arc::new(MockBackend::new());
        let service = EnforcerService::bind(&socket_path, Arc::clone(&backend), None)
            .expect("fixture enforcer should bind");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            service
                .serve_until(&worker_stop)
                .expect("fixture enforcer should run");
        });
        wait_for_path(&socket_path);
        Self {
            socket_path,
            database_path,
            backend,
            stop,
            worker: Some(worker),
        }
    }

    fn apply_request(&self) -> ApplyPolicy {
        ApplyPolicy {
            binding_id: Uuid::new_v4(),
            agent_id: "pipeline-agent".into(),
            session_id: Some("pipeline-session".into()),
            root_pid: 4242,
            process_start_time: 99,
            policy_id: "pipeline-policy".into(),
            policy_revision: "revision-1".into(),
            policy_dsl: "label AGENT".into(),
            policy_mode: None,
        }
    }

    fn violation(&self, request: &ApplyPolicy) -> ViolationEvent {
        ViolationEvent {
            event_id: Uuid::new_v4(),
            binding_id: request.binding_id,
            agent_id: request.agent_id.clone(),
            session_id: request.session_id.clone(),
            policy_id: request.policy_id.clone(),
            policy_revision: request.policy_revision.clone(),
            pid: request.root_pid,
            ppid: Some(1),
            process_start_time: request.process_start_time,
            operation: "open".into(),
            target: "/tmp/secret".into(),
            effect: Effect::Block,
            blocked: true,
            killed: false,
            rule_id: Some("block-secret".into()),
            reason: Some("pipeline fixture".into()),
            occurred_at_ns: 100,
            observed_at_ns: 101,
            actplane_revision: "mock".into(),
        }
    }
}

impl ControlledEnforcer {
    fn start() -> Self {
        let id = Uuid::new_v4();
        let socket_path = PathBuf::from(format!("/tmp/agentsight-controlled-{id}.sock"));
        let database_path = PathBuf::from(format!("/tmp/agentsight-controlled-{id}.db"));
        let listener = UnixListener::bind(&socket_path).expect("controlled enforcer should bind");
        listener
            .set_nonblocking(true)
            .expect("controlled listener should be nonblocking");
        let state = Arc::new((
            Mutex::new(ControlledState {
                subscription: SubscriptionState::AckBlocked,
                health_ready: true,
                health_message: None,
                subscribe_attempts: 0,
                apply_attempts: 0,
                apply_blocked: false,
                reject_next_required_subscription: false,
                detach_attempts: 0,
                detach_failures_remaining: 0,
                list_attempts: 0,
                list_failures_remaining: 0,
                reconciliation_blocked: false,
                bindings: Vec::new(),
            }),
            Condvar::new(),
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_state = Arc::clone(&state);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let mut connections = Vec::new();
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let connection_state = Arc::clone(&worker_state);
                        let connection_stop = Arc::clone(&worker_stop);
                        connections.push(thread::spawn(move || {
                            handle_controlled_connection(stream, connection_state, connection_stop);
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("controlled enforcer accept failed: {error}"),
                }
            }
            for connection in connections {
                connection
                    .join()
                    .expect("controlled connection should stop");
            }
        });
        Self {
            socket_path,
            database_path,
            state,
            stop,
            worker: Some(worker),
        }
    }

    fn apply_request(&self) -> ApplyPolicy {
        ApplyPolicy {
            binding_id: Uuid::new_v4(),
            agent_id: "controlled-agent".into(),
            session_id: Some("controlled-session".into()),
            root_pid: 4242,
            process_start_time: 99,
            policy_id: "controlled-policy".into(),
            policy_revision: "revision-1".into(),
            policy_dsl: "label AGENT".into(),
            policy_mode: None,
        }
    }

    fn wait_for_subscribe_attempt(&self) -> bool {
        self.wait_for_subscribe_attempts(1)
    }

    fn wait_for_subscribe_attempts(&self, expected: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        let (state, changed) = &*self.state;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.subscribe_attempts < expected {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (next, timeout) = changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if timeout.timed_out() && state.subscribe_attempts < expected {
                return false;
            }
        }
        true
    }

    fn acknowledge_subscription(&self) {
        let (state, changed) = &*self.state;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.subscription = SubscriptionState::Connected;
        changed.notify_all();
    }

    fn disconnect_subscription(&self) {
        let (state, changed) = &*self.state;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.subscription = SubscriptionState::AckBlocked;
        changed.notify_all();
    }

    fn set_health(&self, ready: bool, message: Option<&str>) {
        let (state, changed) = &*self.state;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.health_ready = ready;
        state.health_message = message.map(str::to_owned);
        changed.notify_all();
    }

    fn block_apply(&self) {
        self.state
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply_blocked = true;
    }

    fn allow_apply(&self) {
        let (state, changed) = &*self.state;
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply_blocked = false;
        changed.notify_all();
    }

    fn reject_next_required_subscription(&self) {
        self.state
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reject_next_required_subscription = true;
    }

    fn restart_without_bindings_and_block_reconciliation(&self) {
        let (state, changed) = &*self.state;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.subscription = SubscriptionState::AckBlocked;
        state.reconciliation_blocked = true;
        state.bindings.clear();
        changed.notify_all();
    }

    fn disconnect_subscription_and_block_reconciliation(&self) {
        let (state, changed) = &*self.state;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.subscription = SubscriptionState::AckBlocked;
        state.reconciliation_blocked = true;
        changed.notify_all();
    }

    fn allow_reconciliation(&self) {
        let (state, changed) = &*self.state;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.reconciliation_blocked = false;
        changed.notify_all();
    }

    fn set_bindings(&self, bindings: Vec<Binding>) {
        self.state
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .bindings = bindings;
    }

    fn fail_next_reconciliation(&self) {
        self.state
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .list_failures_remaining += 1;
    }

    fn fail_next_detach(&self) {
        self.state
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .detach_failures_remaining += 1;
    }

    fn apply_attempts(&self) -> usize {
        self.state
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply_attempts
    }

    fn wait_for_apply_attempts(&self, expected: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        let (state, changed) = &*self.state;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.apply_attempts < expected {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (next, timeout) = changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if timeout.timed_out() && state.apply_attempts < expected {
                return false;
            }
        }
        true
    }

    fn detach_attempts(&self) -> usize {
        self.state
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .detach_attempts
    }

    fn wait_for_detach_attempts(&self, expected: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        let (state, changed) = &*self.state;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.detach_attempts < expected {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (next, timeout) = changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if timeout.timed_out() && state.detach_attempts < expected {
                return false;
            }
        }
        true
    }

    fn wait_for_list_attempts(&self, expected: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        let (state, changed) = &*self.state;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.list_attempts < expected {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (next, timeout) = changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if timeout.timed_out() && state.list_attempts < expected {
                return false;
            }
        }
        true
    }

    fn bindings(&self) -> Vec<Binding> {
        self.state
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .bindings
            .clone()
    }
}

impl Drop for TestEnforcer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("fixture enforcer should stop");
        }
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.database_path);
        let _ = fs::remove_file(format!("{}-wal", self.database_path.display()));
        let _ = fs::remove_file(format!("{}-shm", self.database_path.display()));
    }
}

impl Drop for ControlledEnforcer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.state.1.notify_all();
        if let Some(worker) = self.worker.take() {
            worker.join().expect("controlled enforcer should stop");
        }
        remove_fixture_files(&self.socket_path, &self.database_path);
    }
}

fn handle_controlled_connection(
    mut stream: UnixStream,
    state: Arc<(Mutex<ControlledState>, Condvar)>,
    stop: Arc<AtomicBool>,
) {
    let request: Request = read_frame(&mut BufReader::new(&stream))
        .expect("controlled request should decode")
        .expect("controlled request should not be EOF");
    let request_id = request.request_id;
    let result = match request.command {
        Command::Health => {
            let state = state
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Ok(ResponseBody::Health(HealthStatus {
                ready: state.health_ready,
                backend: "controlled".into(),
                capabilities:
                    agentsight_enforcement_protocol::EnforcementCapabilities::mock_development(),
                message: state.health_message.clone(),
            }))
        }
        Command::ApplyPolicy(_) | Command::ApplyCredentialPolicy(_) | Command::ReplacePolicy(_) => {
            Err(RemoteError {
                code: "required_subscription_unavailable".into(),
                message: "fixture requires a subscription lease".into(),
            })
        }
        Command::ApplyPolicyLeased { request, .. } => {
            let binding = Binding {
                request,
                state: BindingState::Enforced,
                message: None,
                domain_id: Some(1),
            };
            let (state, changed) = &*state;
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.apply_attempts += 1;
            changed.notify_all();
            if state.reject_next_required_subscription {
                state.reject_next_required_subscription = false;
                return write_frame(
                    &mut stream,
                    &Response {
                        protocol_version: agentsight_enforcement_protocol::PROTOCOL_VERSION,
                        request_id,
                        result: Err(RemoteError {
                            code: "required_subscription_unavailable".into(),
                            message: "fixture required subscription is stale".into(),
                        }),
                    },
                )
                .expect("controlled response should encode");
            }
            while state.apply_blocked && !stop.load(Ordering::Acquire) {
                state = changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            state.bindings.push(binding.clone());
            Ok(ResponseBody::Applied(binding))
        }
        Command::ApplyCredentialPolicyLeased { request, .. } => {
            let policy = request.policy;
            let binding = Binding {
                request: ApplyPolicy {
                    binding_id: request.binding_id,
                    agent_id: request.agent_id,
                    session_id: request.session_id,
                    root_pid: request.root_pid,
                    process_start_time: request.process_start_time,
                    policy_id: policy.policy_id,
                    policy_revision: policy.revision.to_string(),
                    policy_dsl: String::new(),
                    policy_mode: Some(policy.mode),
                },
                state: BindingState::Enforced,
                message: None,
                domain_id: Some(1),
            };
            let (state, changed) = &*state;
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.apply_attempts += 1;
            changed.notify_all();
            if state.reject_next_required_subscription {
                state.reject_next_required_subscription = false;
                return write_frame(
                    &mut stream,
                    &Response {
                        protocol_version: agentsight_enforcement_protocol::PROTOCOL_VERSION,
                        request_id,
                        result: Err(RemoteError {
                            code: "required_subscription_unavailable".into(),
                            message: "fixture required subscription is stale".into(),
                        }),
                    },
                )
                .expect("controlled response should encode");
            }
            while state.apply_blocked && !stop.load(Ordering::Acquire) {
                state = changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            state.bindings.push(binding.clone());
            Ok(ResponseBody::Applied(binding))
        }
        Command::ReplacePolicyLeased { request, .. } => {
            let target_request = match request.replacement {
                ReplacementPolicy::Generic(request) => request,
                ReplacementPolicy::Credential(request) => ApplyPolicy {
                    binding_id: request.binding_id,
                    agent_id: request.agent_id,
                    session_id: request.session_id,
                    root_pid: request.root_pid,
                    process_start_time: request.process_start_time,
                    policy_id: request.policy.policy_id,
                    policy_revision: request.policy.revision.to_string(),
                    policy_dsl: String::new(),
                    policy_mode: Some(request.policy.mode),
                },
            };
            let target = Binding {
                request: target_request,
                state: BindingState::Enforced,
                message: None,
                domain_id: request.expected.domain_id.or(Some(1)),
            };
            let mut state = state
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.bindings.contains(&target) {
                Ok(ResponseBody::Replaced(ReplaceOutcome::Applied(target)))
            } else if state.bindings.contains(&request.expected) {
                state.bindings.clear();
                state.bindings.push(target.clone());
                Ok(ResponseBody::Replaced(ReplaceOutcome::Applied(target)))
            } else {
                Ok(ResponseBody::Replaced(ReplaceOutcome::Conflict {
                    code: ReplaceFailureCode::BindingConflict,
                }))
            }
        }
        Command::ListBindings => {
            let (shared, changed) = &*state;
            let mut shared = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            shared.list_attempts += 1;
            changed.notify_all();
            if shared.list_failures_remaining > 0 {
                shared.list_failures_remaining -= 1;
                Err(RemoteError {
                    code: "fixture_reconciliation_failure".into(),
                    message: "fixture list bindings failed".into(),
                })
            } else {
                while shared.reconciliation_blocked && !stop.load(Ordering::Acquire) {
                    shared = changed
                        .wait(shared)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                if stop.load(Ordering::Acquire) {
                    return;
                }
                Ok(ResponseBody::Bindings(shared.bindings.clone()))
            }
        }
        Command::DetachAgent { binding_id } => {
            let (state, changed) = &*state;
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.detach_attempts += 1;
            changed.notify_all();
            if state.detach_failures_remaining > 0 {
                state.detach_failures_remaining -= 1;
                Err(RemoteError {
                    code: "fixture_detach_failure".into(),
                    message: "fixture detach failed".into(),
                })
            } else {
                let original_len = state.bindings.len();
                state
                    .bindings
                    .retain(|binding| binding.request.binding_id != binding_id);
                if state.bindings.len() == original_len {
                    Err(RemoteError {
                        code: "missing_binding".into(),
                        message: format!("binding {binding_id} is not active"),
                    })
                } else {
                    Ok(ResponseBody::Detached)
                }
            }
        }
        Command::SubscribeViolations { .. }
        | Command::SubscribeRequiredViolations { .. }
        | Command::SubscribeSecurityEvents => {
            let (shared, changed) = &*state;
            let mut shared = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            shared.subscribe_attempts += 1;
            changed.notify_all();
            while shared.subscription == SubscriptionState::AckBlocked
                && !stop.load(Ordering::Acquire)
            {
                shared = changed
                    .wait(shared)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            drop(shared);
            if write_frame(
                &mut stream,
                &Response {
                    protocol_version: agentsight_enforcement_protocol::PROTOCOL_VERSION,
                    request_id,
                    result: Ok(ResponseBody::Subscribed),
                },
            )
            .is_err()
            {
                return;
            }
            let mut shared = state
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while shared.subscription == SubscriptionState::Connected
                && !stop.load(Ordering::Acquire)
            {
                shared = state
                    .1
                    .wait(shared)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            return;
        }
    };
    write_frame(
        &mut stream,
        &Response {
            protocol_version: agentsight_enforcement_protocol::PROTOCOL_VERSION,
            request_id,
            result,
        },
    )
    .expect("controlled response should encode");
}

fn remove_fixture_files(socket_path: &Path, database_path: &Path) {
    let _ = fs::remove_file(socket_path);
    let _ = fs::remove_file(database_path);
    let _ = fs::remove_file(format!("{}-wal", database_path.display()));
    let _ = fs::remove_file(format!("{}-shm", database_path.display()));
}

fn wait_for_path(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("fixture path did not appear");
}

fn poll_until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    condition()
}

fn drop_violation_table(path: &Path) {
    rusqlite::Connection::open(path)
        .expect("fixture database should open")
        .execute_batch("DROP TABLE enforcement_violations;")
        .expect("fixture violation table should drop");
}

fn recreate_violation_table(path: &Path) {
    rusqlite::Connection::open(path)
        .expect("fixture database should open")
        .execute_batch(
            "CREATE TABLE enforcement_violations (
                event_id TEXT PRIMARY KEY,
                binding_id TEXT NOT NULL,
                occurred_at_ns INTEGER NOT NULL,
                event_json TEXT NOT NULL
             );
             CREATE INDEX idx_enforcement_violations_time
                ON enforcement_violations(occurred_at_ns DESC);",
        )
        .expect("fixture violation table should be recreated");
}

fn reject_detached_state_updates(path: &Path) {
    rusqlite::Connection::open(path)
        .expect("fixture database should open")
        .execute_batch(
            "CREATE TRIGGER reject_detached_state
             BEFORE UPDATE OF state ON enforcement_bindings
             WHEN NEW.state = 'detached'
             BEGIN
                 SELECT RAISE(ABORT, 'fixture detached persistence failure');
             END;",
        )
        .expect("fixture trigger should install");
}

fn allow_detached_state_updates(path: &Path) {
    rusqlite::Connection::open(path)
        .expect("fixture database should open")
        .execute_batch("DROP TRIGGER reject_detached_state;")
        .expect("fixture trigger should drop");
}

#[test]
fn health_waits_for_subscription_acknowledgement() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    let subscription_attempted = fixture.wait_for_subscribe_attempt();

    let before_ack = coordinator.health().expect("backend health should work");
    fixture.acknowledge_subscription();
    let ready_after_ack = poll_until(|| coordinator.health().is_ok_and(|health| health.ready));
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");

    assert!(subscription_attempted);
    assert!(!before_ack.ready);
    assert_eq!(
        before_ack.message.as_deref(),
        Some("violation ingestion is not subscribed")
    );
    assert!(ready_after_ack);
}

#[test]
fn backend_health_blocks_apply_without_changing_desired_state() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(fixture.wait_for_subscribe_attempt());
    fixture.acknowledge_subscription();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    fixture.set_health(false, Some("violation event delivery loss"));

    let result = coordinator.apply(fixture.apply_request());
    let persisted = coordinator
        .bindings()
        .expect("persisted bindings should load");
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");

    assert!(matches!(
        &result,
        Err(EnforcementCoordinatorError::EnforcementUnavailable(message))
            if message.contains("violation event delivery loss")
    ));
    assert_eq!(fixture.apply_attempts(), 0);
    assert!(fixture.bindings().is_empty());
    assert!(persisted.is_empty());
}

#[test]
fn remote_subscription_rejection_precedes_local_disconnect_observation() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(fixture.wait_for_subscribe_attempt());
    fixture.acknowledge_subscription();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    fixture.reject_next_required_subscription();

    let result = coordinator.apply(fixture.apply_request());
    let persisted = coordinator
        .bindings()
        .expect("persisted bindings should load");
    let locally_revoked = coordinator.health().is_ok_and(|health| !health.ready);
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");

    assert!(matches!(
        &result,
        Err(EnforcementCoordinatorError::PersistedUnacknowledged {
            state: BindingState::Degraded,
            source,
            ..
        }) if matches!(
            source.as_ref(),
            EnforcementCoordinatorError::EnforcementUnavailable(message)
                if message.contains("violation ingestion is not subscribed")
        )
    ));
    assert_eq!(fixture.apply_attempts(), 1);
    assert!(fixture.bindings().is_empty());
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].state, BindingState::Degraded);
    assert_eq!(
        persisted[0].message.as_deref(),
        Some("enforcement binding is degraded")
    );
    assert!(
        !persisted[0]
            .message
            .as_deref()
            .is_some_and(|message| message.contains("fixture required subscription"))
    );
    assert!(locally_revoked);
}

#[test]
fn apply_before_subscription_acknowledgement_changes_no_state() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    let subscription_attempted = fixture.wait_for_subscribe_attempt();

    let result = coordinator.apply(fixture.apply_request());
    let backend_bindings = fixture.bindings();
    let persisted_bindings = coordinator
        .bindings()
        .expect("persisted bindings should load");
    coordinator.stop_ingestion();
    fixture.acknowledge_subscription();
    ingestion.join().expect("ingestion should stop");

    assert!(subscription_attempted);
    let error = result.expect_err("apply before ingestion ACK should fail");
    assert!(matches!(
        &error,
        EnforcementCoordinatorError::IngestionUnavailable
    ));
    assert_eq!(error.to_string(), "violation ingestion is not subscribed");
    assert!(backend_bindings.is_empty());
    assert!(persisted_bindings.is_empty());
}

#[test]
fn subscription_disconnect_revokes_readiness_and_rejects_apply() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    let subscription_attempted = fixture.wait_for_subscribe_attempt();
    fixture.acknowledge_subscription();
    let became_ready = poll_until(|| coordinator.health().is_ok_and(|health| health.ready));

    fixture.disconnect_subscription();
    let readiness_revoked = poll_until(|| coordinator.health().is_ok_and(|health| !health.ready));
    let result = coordinator.apply(fixture.apply_request());
    let backend_bindings = fixture.bindings();
    let persisted_bindings = coordinator
        .bindings()
        .expect("persisted bindings should load");
    coordinator.stop_ingestion();
    fixture.acknowledge_subscription();
    ingestion.join().expect("ingestion should stop");

    assert!(subscription_attempted);
    assert!(became_ready);
    assert!(readiness_revoked);
    let error = result.expect_err("apply after disconnect should fail");
    assert!(matches!(
        &error,
        EnforcementCoordinatorError::IngestionUnavailable
    ));
    assert_eq!(error.to_string(), "violation ingestion is not subscribed");
    assert!(backend_bindings.is_empty());
    assert!(persisted_bindings.is_empty());
}

#[test]
fn disconnect_during_apply_never_reports_enforced_state() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator = Arc::new(EnforcementCoordinator::new(
        EnforcementClient::new(&fixture.socket_path),
        store,
    ));
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(fixture.wait_for_subscribe_attempt());
    fixture.acknowledge_subscription();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    fixture.block_apply();

    let request = fixture.apply_request();
    let applying = Arc::clone(&coordinator);
    let apply = thread::spawn(move || applying.apply(request));
    assert!(fixture.wait_for_apply_attempts(1));
    fixture.disconnect_subscription();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| !health.ready)));
    fixture.allow_apply();
    let result = apply.join().expect("apply worker should stop");
    let persisted = coordinator
        .bindings()
        .expect("persisted bindings should load");
    coordinator.stop_ingestion();
    fixture.acknowledge_subscription();
    ingestion.join().expect("ingestion should stop");

    assert!(
        matches!(
            &result,
            Err(EnforcementCoordinatorError::PersistedUnacknowledged {
                state: BindingState::Degraded,
                source,
                ..
            }) if matches!(
                source.as_ref(),
                EnforcementCoordinatorError::EnforcementUnavailable(message)
                    if message.contains("violation ingestion is not subscribed")
            )
        ),
        "unexpected apply result after disconnect: {result:?}"
    );
    assert_eq!(fixture.bindings().len(), 1);
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].state, BindingState::Degraded);
    assert!(
        persisted[0]
            .message
            .as_deref()
            .is_some_and(|message| message.contains("violation ingestion is not subscribed"))
    );
}

#[test]
fn apply_persists_enforced_state_and_deduplicates_violation() {
    let fixture = TestEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    let ready = poll_until(|| coordinator.health().is_ok_and(|health| health.ready));
    let request = fixture.apply_request();
    let binding = coordinator.apply(request.clone());
    assert!(ready);
    assert_eq!(
        binding.expect("mock apply should work").state,
        BindingState::Enforced
    );
    let violation = fixture.violation(&request);
    fixture
        .backend
        .publish_violation(violation.clone())
        .expect("first violation should publish");
    fixture
        .backend
        .publish_violation(violation)
        .expect("duplicate violation should publish");

    let violation_ingested = poll_until(|| {
        coordinator
            .violations(100)
            .expect("violation query should work")
            .len()
            == 1
    });
    assert!(violation_ingested);
    assert_eq!(
        coordinator
            .violations(100)
            .expect("violation query should work")
            .len(),
        1
    );
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");
}

#[test]
fn conflicting_binding_id_preserves_original_enforced_record() {
    let fixture = TestEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));

    let original_request = fixture.apply_request();
    let original = coordinator
        .apply(original_request.clone())
        .expect("original binding should apply");
    let mut conflicting_request = original_request;
    conflicting_request.policy_revision = "revision-2".into();
    conflicting_request.policy_dsl = "label CONFLICT".into();

    let error = coordinator
        .apply(conflicting_request)
        .expect_err("conflicting idempotency key should fail");
    let persisted = coordinator
        .bindings()
        .expect("persisted bindings should load");
    let backend = fixture
        .backend
        .bindings()
        .expect("backend bindings should load");
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");

    assert!(matches!(
        error,
        EnforcementCoordinatorError::Client(EnforcementError::Remote { code, .. })
            if code == "binding_conflict"
    ));
    assert_eq!(persisted, vec![original.clone()]);
    assert_eq!(backend, vec![original]);
}

#[test]
fn reconnect_reconciles_desired_state_before_restoring_readiness() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(fixture.wait_for_subscribe_attempt());
    fixture.acknowledge_subscription();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));

    let desired = coordinator
        .apply(fixture.apply_request())
        .expect("desired binding should apply");
    fixture.restart_without_bindings_and_block_reconciliation();
    assert!(fixture.wait_for_subscribe_attempts(2));
    fixture.acknowledge_subscription();
    thread::sleep(Duration::from_millis(50));

    let during_reconciliation = coordinator.health().expect("backend health should work");
    assert!(!during_reconciliation.ready);
    assert!(fixture.bindings().is_empty());

    fixture.allow_reconciliation();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    assert_eq!(fixture.bindings(), vec![desired.clone()]);
    assert_eq!(
        coordinator
            .bindings()
            .expect("persisted bindings should load"),
        vec![desired]
    );

    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");
}

#[test]
fn reconciliation_replaces_conflicting_actual_and_detaches_orphan() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let desired_request = fixture.apply_request();
    let desired = Binding {
        request: desired_request.clone(),
        state: BindingState::Degraded,
        message: Some("enforcer restarted".into()),
        domain_id: None,
    };
    store
        .upsert_binding(&desired)
        .expect("desired binding should seed");

    let mut conflicting_request = desired_request;
    conflicting_request.policy_revision = "conflicting-revision".into();
    let conflicting_actual = Binding {
        request: conflicting_request,
        state: BindingState::Enforced,
        message: None,
        domain_id: Some(7),
    };
    let orphan_actual = Binding {
        request: fixture.apply_request(),
        state: BindingState::Enforced,
        message: None,
        domain_id: Some(8),
    };
    fixture.set_bindings(vec![conflicting_actual, orphan_actual]);

    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(fixture.wait_for_subscribe_attempt());
    fixture.acknowledge_subscription();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));

    let actual = fixture.bindings();
    let persisted = coordinator
        .bindings()
        .expect("persisted bindings should load");
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");

    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0].request, desired.request);
    assert_eq!(actual[0].state, BindingState::Enforced);
    assert_eq!(persisted, actual);
}

#[test]
fn detach_waits_for_inflight_reconciliation_snapshot() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator = Arc::new(EnforcementCoordinator::new(
        EnforcementClient::new(&fixture.socket_path),
        store,
    ));
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(fixture.wait_for_subscribe_attempt());
    fixture.acknowledge_subscription();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    let binding = coordinator
        .apply(fixture.apply_request())
        .expect("binding should apply");

    fixture.disconnect_subscription_and_block_reconciliation();
    assert!(fixture.wait_for_subscribe_attempts(2));
    fixture.acknowledge_subscription();
    assert!(fixture.wait_for_list_attempts(2));

    let detach_coordinator = Arc::clone(&coordinator);
    let (detached, detached_result) = mpsc::channel();
    let detach = thread::spawn(move || {
        let result = detach_coordinator.detach(binding.request.binding_id);
        detached.send(result).expect("test receiver should remain");
    });
    let early_result = detached_result.recv_timeout(Duration::from_millis(100));
    let detach_was_blocked = matches!(early_result, Err(RecvTimeoutError::Timeout));
    fixture.allow_reconciliation();
    let result = match early_result {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => detached_result
            .recv_timeout(Duration::from_secs(2))
            .expect("detach should finish after reconciliation"),
        Err(RecvTimeoutError::Disconnected) => panic!("detach worker disconnected"),
    };
    detach.join().expect("detach worker should stop");
    assert!(result.is_ok());
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    let persisted = coordinator
        .bindings()
        .expect("persisted bindings should load");
    let actual = fixture.bindings();
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");

    assert!(detach_was_blocked);
    assert_eq!(persisted[0].state, BindingState::Detached);
    assert!(actual.is_empty());
}

#[test]
fn reconciliation_failure_preserves_detaching_until_retry_completes() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let request = fixture.apply_request();
    let desired = Binding {
        request: request.clone(),
        state: BindingState::Detaching,
        message: None,
        domain_id: Some(1),
    };
    store
        .upsert_binding(&desired)
        .expect("detaching binding should seed");
    fixture.set_bindings(vec![Binding {
        request,
        state: BindingState::Enforced,
        message: None,
        domain_id: Some(1),
    }]);
    fixture.fail_next_reconciliation();

    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(fixture.wait_for_subscribe_attempt());
    fixture.acknowledge_subscription();
    assert!(fixture.wait_for_list_attempts(2));
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    let persisted = coordinator
        .bindings()
        .expect("persisted bindings should load");
    let actual = fixture.bindings();
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");

    assert_eq!(persisted[0].state, BindingState::Detached);
    assert!(actual.is_empty());
}

#[test]
fn failed_detach_is_retried_as_removal_without_reapplying() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(fixture.wait_for_subscribe_attempt());
    fixture.acknowledge_subscription();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    let binding = coordinator
        .apply(fixture.apply_request())
        .expect("binding should apply");
    fixture.fail_next_detach();

    let error = coordinator
        .detach(binding.request.binding_id)
        .expect_err("first detach should fail");
    assert!(matches!(
        error,
        EnforcementCoordinatorError::Client(EnforcementError::Remote { code, .. })
            if code == "fixture_detach_failure"
    ));
    let pending_retry = coordinator
        .bindings()
        .expect("detaching binding should load");
    assert_eq!(pending_retry[0].state, BindingState::Detaching);
    assert!(pending_retry[0].message.is_some());

    fixture.disconnect_subscription();
    assert!(fixture.wait_for_subscribe_attempts(2));
    fixture.acknowledge_subscription();
    assert!(fixture.wait_for_detach_attempts(2));
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    let persisted = coordinator
        .bindings()
        .expect("persisted bindings should load");
    let actual = fixture.bindings();
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");

    assert_eq!(fixture.apply_attempts(), 1);
    assert_eq!(persisted[0].state, BindingState::Detached);
    assert!(actual.is_empty());
}

#[test]
fn identical_apply_preserves_detaching_and_detached_intent() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(fixture.wait_for_subscribe_attempt());
    fixture.acknowledge_subscription();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    let request = fixture.apply_request();
    coordinator
        .apply(request.clone())
        .expect("binding should apply");
    fixture.fail_next_detach();
    coordinator
        .detach(request.binding_id)
        .expect_err("first detach should fail");

    let detaching = coordinator
        .apply(request.clone())
        .expect("identical apply should return desired state");
    assert_eq!(detaching.state, BindingState::Detaching);
    assert_eq!(fixture.apply_attempts(), 1);

    coordinator
        .detach(request.binding_id)
        .expect("detach retry should succeed");
    let detached = coordinator
        .apply(request)
        .expect("identical apply should return terminal state");
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");

    assert_eq!(detached.state, BindingState::Detached);
    assert_eq!(fixture.apply_attempts(), 1);
}

#[test]
fn repeated_detach_is_idempotent_but_never_persisted_id_is_missing() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(fixture.wait_for_subscribe_attempt());
    fixture.acknowledge_subscription();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    let binding = coordinator
        .apply(fixture.apply_request())
        .expect("binding should apply");
    coordinator
        .detach(binding.request.binding_id)
        .expect("first detach should succeed");

    coordinator
        .detach(binding.request.binding_id)
        .expect("detached binding should remain a successful no-op");
    let missing = coordinator
        .detach(Uuid::new_v4())
        .expect_err("never-persisted binding should remain missing");
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");

    assert_eq!(fixture.detach_attempts(), 1);
    assert!(matches!(
        missing,
        EnforcementCoordinatorError::Store(EnforcementStoreError::MissingBinding(_))
    ));
}

#[test]
fn violation_persistence_failure_is_unready_until_same_event_is_stored() {
    let fixture = TestEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    let request = fixture.apply_request();
    coordinator
        .apply(request.clone())
        .expect("binding should apply");
    let event = fixture.violation(&request);
    drop_violation_table(&fixture.database_path);

    fixture
        .backend
        .publish_violation(event.clone())
        .expect("violation should publish");
    let mut failure_message = None;
    let became_unready = poll_until(|| {
        coordinator.health().is_ok_and(|health| {
            if !health.ready {
                failure_message = health.message;
                true
            } else {
                false
            }
        })
    });
    recreate_violation_table(&fixture.database_path);
    let recovered = poll_until(|| {
        coordinator.health().is_ok_and(|health| health.ready)
            && coordinator
                .violations(10)
                .is_ok_and(|violations| violations == vec![event.clone()])
    });
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");

    assert!(became_unready);
    assert!(
        failure_message
            .as_deref()
            .is_some_and(|message| message.contains("violation ingestion is not subscribed"))
    );
    assert!(recovered);
}

#[test]
fn stop_interrupts_violation_persistence_retry() {
    let fixture = TestEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    let request = fixture.apply_request();
    coordinator
        .apply(request.clone())
        .expect("binding should apply");
    drop_violation_table(&fixture.database_path);
    fixture
        .backend
        .publish_violation(fixture.violation(&request))
        .expect("violation should publish");
    let became_unready = poll_until(|| coordinator.health().is_ok_and(|health| !health.ready));

    coordinator.stop_ingestion();
    let (stopped, result) = mpsc::channel();
    thread::spawn(move || {
        stopped
            .send(ingestion.join().is_ok())
            .expect("test receiver should remain");
    });
    let stopped_promptly = result
        .recv_timeout(Duration::from_secs(1))
        .expect("retry should stop after worker is superseded");

    assert!(became_unready);
    assert!(stopped_promptly);
}

#[test]
fn missing_backend_binding_completes_detach_after_ack_persistence_failed() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(fixture.wait_for_subscribe_attempt());
    fixture.acknowledge_subscription();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    let binding = coordinator
        .apply(fixture.apply_request())
        .expect("binding should apply");
    reject_detached_state_updates(&fixture.database_path);

    let first = coordinator.detach(binding.request.binding_id);
    assert!(matches!(first, Err(EnforcementCoordinatorError::Store(_))));
    assert_eq!(
        coordinator
            .bindings()
            .expect("detaching binding should load")[0]
            .state,
        BindingState::Detaching
    );
    assert!(fixture.bindings().is_empty());
    allow_detached_state_updates(&fixture.database_path);

    coordinator
        .detach(binding.request.binding_id)
        .expect("missing backend binding should acknowledge prior detach");
    let persisted = coordinator
        .bindings()
        .expect("detached binding should load");
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");

    assert_eq!(fixture.detach_attempts(), 2);
    assert_eq!(persisted[0].state, BindingState::Detached);
    assert!(persisted[0].message.is_none());
    assert!(persisted[0].domain_id.is_none());
}

#[test]
fn first_detach_succeeds_when_persisted_binding_is_already_absent_from_backend() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let request = fixture.apply_request();
    store
        .upsert_binding(&Binding {
            request: request.clone(),
            state: BindingState::Enforced,
            message: None,
            domain_id: Some(1),
        })
        .expect("enforced binding should seed");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);

    coordinator
        .detach(request.binding_id)
        .expect("absent backend target should satisfy detach");
    let persisted = coordinator
        .bindings()
        .expect("detached binding should load");

    assert_eq!(fixture.detach_attempts(), 1);
    assert_eq!(persisted[0].state, BindingState::Detached);
    assert!(persisted[0].message.is_none());
    assert!(persisted[0].domain_id.is_none());
}

#[test]
fn successful_detach_clears_persisted_runtime_domain() {
    let fixture = ControlledEnforcer::start();
    let store = EnforcementStore::open(&fixture.database_path)
        .expect("temporary enforcement store should open");
    let coordinator =
        EnforcementCoordinator::new(EnforcementClient::new(&fixture.socket_path), store);
    let ingestion = coordinator
        .start_ingestion()
        .expect("ingestion should start");
    assert!(fixture.wait_for_subscribe_attempt());
    fixture.acknowledge_subscription();
    assert!(poll_until(|| coordinator
        .health()
        .is_ok_and(|health| health.ready)));
    let binding = coordinator
        .apply(fixture.apply_request())
        .expect("binding should apply");
    assert!(binding.domain_id.is_some());

    coordinator
        .detach(binding.request.binding_id)
        .expect("active binding should detach");

    let persisted = coordinator
        .bindings()
        .expect("detached binding should load");
    coordinator.stop_ingestion();
    ingestion.join().expect("ingestion should stop");
    assert_eq!(persisted[0].state, BindingState::Detached);
    assert!(persisted[0].domain_id.is_none());
}
