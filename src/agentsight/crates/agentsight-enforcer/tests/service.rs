#![cfg(feature = "mock-backend")]

use std::fs;
use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, ApplyPolicy, BindingState, Command, CredentialExfiltrationPolicy,
    DestinationScope, Effect, PolicyMode, ReplaceOutcome, ReplacePolicy, ReplacementPolicy,
    ReplacementSource, Request, Response, ResponseBody, ViolationEvent, read_frame, write_frame,
};
use agentsight_enforcer::{BackendError, EnforcementBackend, EnforcerService, MockBackend};
use uuid::Uuid;

fn fixture_apply_policy() -> ApplyPolicy {
    ApplyPolicy {
        binding_id: Uuid::new_v4(),
        agent_id: "test-agent".into(),
        session_id: Some("session-1".into()),
        root_pid: 4242,
        process_start_time: 99,
        policy_id: "test-policy".into(),
        policy_revision: "revision-1".into(),
        policy_dsl: "label AGENT".into(),
        policy_mode: None,
    }
}

fn fixture_violation(request: &ApplyPolicy) -> ViolationEvent {
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
        reason: Some("test violation".into()),
        occurred_at_ns: 100,
        observed_at_ns: 101,
        actplane_revision: "mock".into(),
    }
}

#[test]
fn changed_duplicate_conflicts() {
    let backend = MockBackend::new();
    let request = fixture_apply_policy();
    assert!(backend.apply(request.clone()).is_ok());
    assert!(backend.apply(request.clone()).is_ok());
    let mut changed = request;
    changed.policy_revision = "revision-2".into();
    assert!(matches!(
        backend.apply(changed),
        Err(BackendError::BindingConflict(_))
    ));
}

#[test]
fn bind_recovers_a_stale_socket_after_unclean_shutdown() {
    let socket_path =
        PathBuf::from("/tmp").join(format!("agentsight-stale-{}.sock", Uuid::new_v4()));
    let listener = UnixListener::bind(&socket_path).expect("fixture socket should bind");
    drop(listener);

    let service = EnforcerService::bind(&socket_path, Arc::new(MockBackend::new()), None)
        .expect("stale service socket should be replaced");

    drop(service);
    assert!(!socket_path.exists());
}

#[test]
fn bind_never_replaces_a_non_socket_path() {
    let socket_path =
        PathBuf::from("/tmp").join(format!("agentsight-file-{}.sock", Uuid::new_v4()));
    fs::write(&socket_path, b"owned by another service").expect("fixture file should exist");

    let result = EnforcerService::bind(&socket_path, Arc::new(MockBackend::new()), None);

    assert!(result.is_err());
    assert_eq!(
        fs::read(&socket_path).expect("fixture file must remain"),
        b"owned by another service"
    );
    fs::remove_file(socket_path).expect("fixture file should clean up");
}

struct ServiceFixture {
    socket_path: PathBuf,
    backend: Arc<MockBackend>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl ServiceFixture {
    fn start() -> Self {
        let socket_path =
            PathBuf::from("/tmp").join(format!("agentsight-enforcer-{}.sock", Uuid::new_v4()));
        let backend = Arc::new(MockBackend::new());
        let service = EnforcerService::bind(&socket_path, Arc::clone(&backend), None)
            .expect("fixture service should bind");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            service
                .serve_until(&worker_stop)
                .expect("fixture service should run");
        });
        wait_for_socket(&socket_path);
        Self {
            socket_path,
            backend,
            stop,
            worker: Some(worker),
        }
    }

    fn call(&self, command: Command) -> Response {
        let request = Request::new(command);
        let mut stream = UnixStream::connect(&self.socket_path)
            .expect("fixture client should connect to service");
        write_frame(&mut stream, &request).expect("fixture request should encode");
        read_frame(&mut BufReader::new(stream))
            .expect("fixture response should decode")
            .expect("fixture response should exist")
    }
}

impl Drop for ServiceFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("fixture service should stop");
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn wait_for_socket(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("fixture service socket did not appear");
}

#[test]
fn uds_service_dispatches_lifecycle_and_streams_violations() {
    let fixture = ServiceFixture::start();
    let subscription_id = Uuid::new_v4();
    let request = Request::new(Command::SubscribeRequiredViolations { subscription_id });
    let mut stream =
        UnixStream::connect(&fixture.socket_path).expect("fixture subscriber should connect");
    write_frame(&mut stream, &request).expect("subscribe request should encode");
    let mut reader = BufReader::new(stream);
    let subscribed: Response = read_frame(&mut reader)
        .expect("subscribe response should decode")
        .expect("subscribe response should exist");
    assert_eq!(
        subscribed.request_id, request.request_id,
        "unexpected subscription response: {:?}",
        subscribed.result
    );
    assert!(matches!(subscribed.result, Ok(ResponseBody::Subscribed)));

    let health = fixture.call(Command::Health);
    assert!(matches!(health.result, Ok(ResponseBody::Health(_))));

    let apply = fixture_apply_policy();
    let applied = fixture.call(Command::ApplyPolicyLeased {
        request: apply.clone(),
        required_subscription_id: subscription_id,
    });
    let Ok(ResponseBody::Applied(binding)) = applied.result else {
        panic!("apply must return a binding");
    };
    assert_eq!(binding.state, BindingState::Enforced);

    let mut replacement = fixture_apply_policy();
    replacement.agent_id = apply.agent_id.clone();
    replacement.session_id = apply.session_id.clone();
    replacement.root_pid = apply.root_pid;
    replacement.process_start_time = apply.process_start_time;
    let replaced = fixture.call(Command::ReplacePolicyLeased {
        request: ReplacePolicy {
            expected: binding,
            source: ReplacementSource::Generic,
            replacement: ReplacementPolicy::Generic(replacement.clone()),
        },
        required_subscription_id: subscription_id,
    });
    let Ok(ResponseBody::Replaced(ReplaceOutcome::Applied(binding))) = replaced.result else {
        panic!("replace must transfer runtime ownership");
    };
    assert_eq!(binding.request, replacement);

    let listed = fixture.call(Command::ListBindings);
    let Ok(ResponseBody::Bindings(bindings)) = listed.result else {
        panic!("list must return bindings");
    };
    assert_eq!(bindings, vec![binding]);

    let violation = fixture_violation(&replacement);
    fixture
        .backend
        .publish_violation(violation.clone())
        .expect("fixture violation should publish");
    let streamed: Response = read_frame(&mut reader)
        .expect("violation response should decode")
        .expect("violation response should exist");
    assert_eq!(streamed.request_id, request.request_id);
    assert_eq!(streamed.result, Ok(ResponseBody::Violation(violation)));

    let detached = fixture.call(Command::DetachAgent {
        binding_id: replacement.binding_id,
    });
    assert!(matches!(detached.result, Ok(ResponseBody::Detached)));
    let listed = fixture.call(Command::ListBindings);
    assert_eq!(listed.result, Ok(ResponseBody::Bindings(Vec::new())));
}

#[test]
fn leased_apply_rejects_a_remotely_closed_required_subscription() {
    let fixture = ServiceFixture::start();
    let subscription_id = Uuid::new_v4();
    let request = Request::new(Command::SubscribeRequiredViolations { subscription_id });
    let mut stream =
        UnixStream::connect(&fixture.socket_path).expect("fixture subscriber should connect");
    write_frame(&mut stream, &request).expect("subscribe request should encode");
    let mut reader = BufReader::new(stream);
    let subscribed: Response = read_frame(&mut reader)
        .expect("subscribe response should decode")
        .expect("subscribe response should exist");
    assert!(matches!(subscribed.result, Ok(ResponseBody::Subscribed)));
    drop(reader);

    let applied = fixture.call(Command::ApplyPolicyLeased {
        request: fixture_apply_policy(),
        required_subscription_id: subscription_id,
    });

    assert!(matches!(
        applied.result,
        Err(error) if error.code == "required_subscription_unavailable"
    ));
    assert!(
        fixture
            .backend
            .bindings()
            .expect("backend bindings should load")
            .is_empty()
    );
}

#[test]
fn best_effort_observer_exit_does_not_block_future_apply() {
    let fixture = ServiceFixture::start();
    let required_subscription_id = Uuid::new_v4();
    let required_request = Request::new(Command::SubscribeRequiredViolations {
        subscription_id: required_subscription_id,
    });
    let mut required_stream =
        UnixStream::connect(&fixture.socket_path).expect("required subscriber should connect");
    write_frame(&mut required_stream, &required_request)
        .expect("required subscribe request should encode");
    let mut required_reader = BufReader::new(required_stream);
    let required_ack: Response = read_frame(&mut required_reader)
        .expect("required response should decode")
        .expect("required response should exist");
    assert!(matches!(required_ack.result, Ok(ResponseBody::Subscribed)));

    let observer_request = Request::new(Command::SubscribeViolations {
        subscription_id: Uuid::new_v4(),
    });
    let mut observer_stream =
        UnixStream::connect(&fixture.socket_path).expect("observer should connect");
    write_frame(&mut observer_stream, &observer_request).expect("observer request should encode");
    let mut observer_reader = BufReader::new(observer_stream);
    let observer_ack: Response = read_frame(&mut observer_reader)
        .expect("observer response should decode")
        .expect("observer response should exist");
    assert!(matches!(observer_ack.result, Ok(ResponseBody::Subscribed)));
    drop(observer_reader);
    let apply = fixture_apply_policy();
    let applied = fixture.call(Command::ApplyPolicyLeased {
        request: apply.clone(),
        required_subscription_id,
    });
    assert!(matches!(applied.result, Ok(ResponseBody::Applied(_))));
    let violation = fixture_violation(&apply);
    fixture
        .backend
        .publish_violation(violation.clone())
        .expect("fixture violation should publish");
    let streamed: Response = read_frame(&mut required_reader)
        .expect("required violation should decode")
        .expect("required violation should exist");
    assert_eq!(streamed.result, Ok(ResponseBody::Violation(violation)));
    let health = fixture.call(Command::Health);
    assert!(matches!(
        health.result,
        Ok(ResponseBody::Health(health)) if health.ready
    ));
}

#[test]
fn security_subscription_is_acknowledged_when_backend_support_exists() {
    let fixture = ServiceFixture::start();

    let response = fixture.call(Command::SubscribeSecurityEvents);

    assert!(matches!(response.result, Ok(ResponseBody::Subscribed)));
}

#[test]
fn security_disconnect_records_failed_and_queued_frames() {
    let fixture = ServiceFixture::start();
    let request = ApplyCredentialPolicy {
        binding_id: Uuid::new_v4(),
        agent_id: "security-disconnect".into(),
        session_id: Some("session-1".into()),
        root_pid: 4242,
        process_start_time: 99,
        policy: CredentialExfiltrationPolicy {
            policy_id: "credential-exfiltration".into(),
            revision: 1,
            source_patterns: vec!["/tmp/credential".into()],
            trusted_endpoints: Vec::new(),
            taint_label: "CREDENTIAL".into(),
            taint_ttl_secs: 300,
            destination_scope: DestinationScope::PublicIpv4,
            mode: PolicyMode::Audit,
        },
    };
    fixture
        .backend
        .apply_credential_policy(request.clone())
        .expect("fixture credential policy should apply");
    let subscribe = Request::new(Command::SubscribeSecurityEvents);
    let mut stream =
        UnixStream::connect(&fixture.socket_path).expect("security subscriber should connect");
    write_frame(&mut stream, &subscribe).expect("security subscribe request should encode");
    let mut reader = BufReader::new(stream);
    let subscribed: Response = read_frame(&mut reader)
        .expect("security subscribe response should decode")
        .expect("security subscribe response should exist");
    assert!(matches!(subscribed.result, Ok(ResponseBody::Subscribed)));
    reader
        .get_ref()
        .shutdown(Shutdown::Both)
        .expect("fixture peer should disconnect");
    drop(reader);

    fixture
        .backend
        .emit_credential_exfiltration(request.binding_id, "/tmp/credential", "8.8.8.8:443")
        .expect("fixture evidence chain should publish");

    let mut degraded = false;
    for _ in 0..100 {
        if fixture.backend.health().is_ok_and(|health| {
            health
                .message
                .as_deref()
                .is_some_and(|message| message.contains("security event delivery loss"))
        }) {
            degraded = true;
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert!(degraded, "socket write loss must degrade backend health");
}
