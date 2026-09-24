//! Real UDS protocol checks with a synthetic resolver; actual FUSE acceptance is separate.

use super::*;
use asc_action_runtime::{Finalizer, SecurityEventSink, testing::audit_finalizer};
use asc_capability_skill_sec::{
    SkillSecConfig, SkillSecService, executor::SkillSecExecutor, scanner::ScannerRegistry,
};
use asc_daemon_core::RootManagedPrincipalPolicy;
use asc_daemon_service::{BoundUnixSocket, ServiceConfig, ShutdownToken, UnixService};
use asc_security_events::SecurityEvent;
use auth::{CONTROL_CLIENT, CONTROL_SERVER, Secret};
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::{
    fs::{MetadataExt as _, PermissionsExt as _},
    net::{UnixListener, UnixStream},
};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;
use tempfile::TempDir;

#[derive(Default)]
struct Events(Mutex<Vec<SecurityEvent>>);
impl SecurityEventSink for Events {
    fn write(&self, event: &SecurityEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

fn start_bridge(
    config: SkillFsConfig,
    service: Arc<SkillSecService>,
    finalizer: Finalizer,
) -> Result<SkillFsBridge, SkillFsError> {
    let bridge = SkillFsBridge::prepare(config)?;
    let application = crate::skill_application(
        finalizer,
        SkillSecExecutor::new(service).with_environment(bridge.environment()),
    );
    bridge.start_worker(application)?;
    Ok(bridge)
}

fn ordinary_dispatcher(application: Arc<ActionService>) -> DaemonDispatcher {
    DaemonDispatcher::new(
        asc_pap::PapService::new(
            Arc::new(asc_pap_repository_memory::ProcessLocalPapRepository::default()),
            Arc::new(asc_policy_engine::PolicyTemplateCompiler),
        ),
        Arc::new(RootManagedPrincipalPolicy::default()),
        application,
    )
}

struct Fixture {
    directory: TempDir,
    canonical: PathBuf,
    live: PathBuf,
    control: PathBuf,
    secret: Secret,
    mode: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    server: Option<JoinHandle<()>>,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let canonical = directory.path().join("canonical");
        let live = directory.path().join("live");
        fs::create_dir(&canonical).unwrap();
        fs::create_dir_all(live.join("demo")).unwrap();
        fs::write(
            live.join("demo/SKILL.md"),
            "---\nname: demo\ndescription: safe fixture\n---\nSafe skill\n",
        )
        .unwrap();
        let control = directory.path().join("control.sock");
        let listener = UnixListener::bind(&control).unwrap();
        fs::set_permissions(&control, fs::Permissions::from_mode(0o600)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let secret = Secret(vec![42; 32].into());
        let stop = Arc::new(AtomicBool::new(false));
        let mode = Arc::new(AtomicU8::new(0));
        let (worker_stop, worker_mode, key, physical, canonical_prefix) = (
            stop.clone(),
            mode.clone(),
            secret.clone(),
            live.clone(),
            canonical.clone(),
        );
        let server = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => respond_control(
                        stream,
                        &key,
                        &physical,
                        &canonical_prefix,
                        worker_mode.load(Ordering::Acquire),
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("control accept: {error}"),
                }
            }
        });
        Self {
            directory,
            canonical,
            live,
            control,
            secret,
            mode,
            stop,
            server: Some(server),
        }
    }

    fn config(&self) -> SkillFsConfig {
        let key = self.directory.path().join("hmac.key");
        fs::write(&key, &self.secret.0).unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        SkillFsConfig {
            auth_key_file: key,
            mounts: vec![Mount {
                control_socket: self.control.clone(),
                canonical_root: self.canonical.clone(),
                live_root: self.live.clone(),
                peer_uid: rustix::process::geteuid().as_raw(),
            }],
        }
    }
    fn service(&self) -> Arc<SkillSecService> {
        let state = self.directory.path().join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        Arc::new(
            SkillSecService::new(
                SkillSecConfig {
                    state_dir: state,
                    managed_skill_dirs: Vec::new(),
                },
                ScannerRegistry::default(),
            )
            .unwrap(),
        )
    }
    fn notification(&self, paths: &Value) -> Value {
        json!({"id":"fixture-1","method":"skill_ledger.skillfs_notify_change","params":{"schemaVersion":2,"canonicalSkillDir":self.canonical.join("demo"),"skillId":"demo","eventKind":"write","paths":paths},"trace_context":{},"timeout_ms":5000})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.server.take() {
            thread.join().unwrap();
        }
    }
}

fn line(reader: &mut BufReader<UnixStream>) -> Vec<u8> {
    let mut bytes = Vec::new();
    reader.read_until(b'\n', &mut bytes).unwrap();
    assert_eq!(bytes.pop(), Some(b'\n'));
    bytes
}
fn send(reader: &mut BufReader<UnixStream>, bytes: &[u8]) {
    reader.get_mut().write_all(bytes).unwrap();
    reader.get_mut().write_all(b"\n").unwrap();
}
fn respond_control(stream: UnixStream, key: &Secret, physical: &Path, canonical: &Path, mode: u8) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    Frame::parse(&line(&mut reader), "auth.init").unwrap();
    let nonce = [7; 32];
    send(
        &mut reader,
        &Frame::encode("auth.challenge", Some(&nonce), None),
    );
    let proof = Frame::parse(&line(&mut reader), "auth.proof")
        .unwrap()
        .proof()
        .unwrap();
    auth::verify(key, CONTROL_CLIENT, &nonce, None, &proof).unwrap();
    send(
        &mut reader,
        &Frame::encode(
            "auth.ok",
            None,
            Some(auth::sign(key, CONTROL_SERVER, &nonce, None).as_ref()),
        ),
    );
    let request = line(&mut reader);
    let proof = Frame::parse(&line(&mut reader), "auth.frame")
        .unwrap()
        .proof()
        .unwrap();
    auth::verify(key, CONTROL_CLIENT, &nonce, Some(&request), &proof).unwrap();
    let request: Value = serde_json::from_slice(&request).unwrap();
    assert_eq!(request["method"], "skill.resolveLiveSource");
    let relative = Path::new(request["canonicalSkillDir"].as_str().unwrap())
        .strip_prefix(canonical)
        .unwrap();
    let metadata = fs::metadata(physical.join(relative)).unwrap();
    let mut response = json!({"schemaVersion":"1","ok":true,"result":{"managed":true,"canonicalSkillDir":request["canonicalSkillDir"],"skillId":relative,"relativeSkillDir":relative,"liveSkillDir":physical.join(relative),"transport":"shared_path","identity":{"device":metadata.dev(),"inode":metadata.ino()}}});
    match mode {
        1 => response["result"]["managed"] = json!(false),
        2 => response["result"]["identity"]["inode"] = json!(0),
        3 => response["result"]["liveSkillDir"] = json!("/unconfigured/path"),
        _ => {}
    }
    let response = serde_json::to_vec(&response).unwrap();
    send(&mut reader, &response);
    send(
        &mut reader,
        &Frame::encode(
            "auth.frame",
            None,
            Some(auth::sign(key, CONTROL_SERVER, &nonce, Some(&response)).as_ref()),
        ),
    );
}

fn notify(
    socket: &Path,
    key: &Secret,
    request: &Value,
    wrong_key: bool,
    tamper: bool,
) -> Option<Value> {
    let stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    send(&mut reader, &Frame::encode("auth.init", None, None));
    let nonce = Frame::parse(&line(&mut reader), "auth.challenge")
        .unwrap()
        .nonce()
        .unwrap();
    let bad = Secret(vec![99; 32].into());
    let proof = auth::sign(
        if wrong_key { &bad } else { key },
        NOTIFY_CLIENT,
        &nonce,
        None,
    );
    send(
        &mut reader,
        &Frame::encode("auth.proof", None, Some(proof.as_ref())),
    );
    if wrong_key {
        let mut bytes = Vec::new();
        assert_eq!(reader.read_until(b'\n', &mut bytes).unwrap(), 0);
        return None;
    }
    let proof = Frame::parse(&line(&mut reader), "auth.ok")
        .unwrap()
        .proof()
        .unwrap();
    auth::verify(key, NOTIFY_SERVER, &nonce, None, &proof).unwrap();
    let bytes = serde_json::to_vec(request).unwrap();
    let tag = auth::sign(
        key,
        NOTIFY_CLIENT,
        &nonce,
        Some(if tamper { b"different bytes" } else { &bytes }),
    );
    // One write deliberately coalesces the payload and tag into the same socket read.
    let mut packet = bytes;
    packet.push(b'\n');
    packet.extend_from_slice(&Frame::encode("auth.frame", None, Some(tag.as_ref())));
    packet.push(b'\n');
    reader.get_mut().write_all(&packet).unwrap();
    if tamper {
        let mut bytes = Vec::new();
        assert_eq!(reader.read_until(b'\n', &mut bytes).unwrap(), 0);
        return None;
    }
    let response = line(&mut reader);
    let tag = Frame::parse(&line(&mut reader), "auth.frame")
        .unwrap()
        .proof()
        .unwrap();
    auth::verify(key, NOTIFY_SERVER, &nonce, Some(&response), &tag).unwrap();
    Some(serde_json::from_slice(&response).unwrap())
}

#[test]
fn resolver_aliases_share_identity_and_reject_false_mappings_without_fallback() {
    let fixture = Fixture::new();
    let events = Arc::new(Events::default());
    let service = fixture.service();
    let bridge = start_bridge(fixture.config(), service.clone(), audit_finalizer(events)).unwrap();
    let canonical = SkillIdentity::new(fixture.canonical.join("demo")).unwrap();
    let alias = SkillIdentity::new(fixture.live.join("demo")).unwrap();
    let deadline = || Instant::now() + Duration::from_secs(5);
    let root = bridge.resolve(&canonical, deadline()).unwrap();
    let live = bridge.resolve(&alias, deadline()).unwrap();
    assert_eq!(root.identity, live.identity);
    assert_eq!(root.io_dir, live.io_dir);
    service.initialize().unwrap();
    service
        .certify(&root, "fixture", None, &json!([]), deadline())
        .unwrap();
    assert_eq!(service.check(&live, deadline()).unwrap()["status"], "pass");
    for mode in 1..=3 {
        fixture.mode.store(mode, Ordering::Release);
        assert!(bridge.resolve(&canonical, deadline()).is_err());
    }
    fixture.mode.store(0, Ordering::Release);
    let outside = SkillIdentity::new(fixture.directory.path().join("outside")).unwrap();
    assert_eq!(
        bridge.resolve(&outside, deadline()).unwrap().io_dir,
        outside.path()
    );
    fs::set_permissions(&fixture.control, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(bridge.resolve(&canonical, deadline()).is_err());
    bridge.shutdown().unwrap();
}

#[test]
fn resolver_accepts_reserved_leaf_inside_a_category_only() {
    let fixture = Fixture::new();
    let nested = fixture.live.join("apple/skill-discover");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("SKILL.md"), "Safe categorized Skill").unwrap();
    let bridge = start_bridge(
        fixture.config(),
        fixture.service(),
        audit_finalizer(Arc::new(Events::default())),
    )
    .unwrap();
    let identity = SkillIdentity::new(fixture.canonical.join("apple/skill-discover")).unwrap();
    assert_eq!(
        bridge
            .resolve(&identity, Instant::now() + Duration::from_secs(5))
            .unwrap()
            .io_dir,
        nested
    );
    for relative in ["skill-discover", "skill-discover/child", "apple/.hidden"] {
        let identity = SkillIdentity::new(fixture.canonical.join(relative)).unwrap();
        assert!(
            bridge
                .resolve(&identity, Instant::now() + Duration::from_secs(5))
                .is_err()
        );
    }
    bridge.shutdown().unwrap();
}

#[test]
fn rotation_commands_resolve_current_inodes_and_retain_failed_intents() {
    for command in [
        json!({"command":"rotate-keys"}),
        json!({"command":"init","forceKeys":true,"baseline":false}),
        json!({"command":"init","forceKeys":true,"baseline":true}),
    ] {
        let fixture = Fixture::new();
        let events = Arc::new(Events::default());
        let service = fixture.service();
        let finalizer = audit_finalizer(events);
        let bridge =
            Arc::new(start_bridge(fixture.config(), service.clone(), finalizer.clone()).unwrap());
        let identity = SkillIdentity::new(fixture.canonical.join("demo")).unwrap();
        let deadline = || Instant::now() + Duration::from_secs(5);
        let root = bridge.resolve(&identity, deadline()).unwrap();
        service.initialize().unwrap();
        service
            .certify(&root, "fixture", None, &json!([]), deadline())
            .unwrap();
        let before = service.key_status(deadline()).unwrap()["fingerprint"].clone();
        let intent = fixture.directory.path().join("state/key-rotation.json");
        fs::write(
            &intent,
            serde_json::to_vec(&json!({
                "previous_fingerprint":before,"skills":[identity]
            }))
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&intent, fs::Permissions::from_mode(0o600)).unwrap();
        let parked = fixture.live.join("parked");
        fs::rename(&root.io_dir, &parked).unwrap();
        fs::create_dir(&root.io_dir).unwrap();
        for name in ["SKILL.md", ".skill-meta"] {
            fs::rename(parked.join(name), root.io_dir.join(name)).unwrap();
        }
        assert_ne!(
            fs::metadata(&root.io_dir).unwrap().ino(),
            fs::metadata(&parked).unwrap().ino()
        );
        assert!(
            service
                .rotate_keys(std::slice::from_ref(&root), 0, deadline())
                .is_err()
        );
        let application = crate::skill_application(
            finalizer,
            SkillSecExecutor::new(service.clone()).with_environment(bridge.environment()),
        );
        let dispatcher = ordinary_dispatcher(application);
        let invoke = || {
            serde_json::to_value(dispatcher.handle_with_control(
                asc_daemon_protocol::RequestId::new("rotation").unwrap(),
                asc_daemon_core::PeerCredentials::new(0, 0, 1),
                &asc_daemon_service::DispatchControl::new(deadline()),
                asc_daemon_protocol::DaemonRequest {
                    method: "action.skill_sec".into(),
                    params: command.clone(),
                    trace_context: None,
                    compatibility: None,
                },
            ))
            .unwrap()
        };
        fixture.mode.store(1, Ordering::Release);
        assert_eq!(invoke()["result"]["success"], false);
        assert_eq!(
            service.key_status(deadline()).unwrap()["fingerprint"],
            before
        );
        assert!(intent.exists());
        assert!(matches!(
            service.check(&root, deadline()),
            Err(SkillSecError::RotationPending)
        ));
        fixture.mode.store(0, Ordering::Release);
        let result = invoke();
        assert_eq!(result["result"]["success"], true, "{command}: {result}");
        assert_ne!(
            service.key_status(deadline()).unwrap()["fingerprint"],
            before
        );
        assert!(!intent.exists());
        let resolved = bridge.resolve(&identity, deadline()).unwrap();
        let expected = if command["baseline"] == true {
            "pass"
        } else {
            "tampered"
        };
        assert_eq!(
            service.check(&resolved, deadline()).unwrap()["status"],
            expected
        );
        bridge.shutdown().unwrap();
    }
}

#[test]
fn configuration_and_secret_validation_reject_unsafe_bindings() {
    let fixture = Fixture::new();
    let mut config = fixture.config();
    config.mounts[0].live_root = config.mounts[0].canonical_root.join("nested");
    assert!(validate_config(&config).is_err());
    config.mounts[0].live_root = config.mounts[0].canonical_root.clone();
    validate_config(&config).unwrap();
    config.mounts.push(Mount {
        control_socket: fixture.control.clone(),
        canonical_root: config.mounts[0].canonical_root.clone(),
        live_root: fixture.live.clone(),
        peer_uid: rustix::process::geteuid().as_raw(),
    });
    assert!(validate_config(&config).is_err());
    let config = fixture.config();
    fs::set_permissions(&config.auth_key_file, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(resolver::load_secret(&config.auth_key_file).is_err());
    fs::set_permissions(&config.auth_key_file, fs::Permissions::from_mode(0o600)).unwrap();
    let link = fixture.directory.path().join("key-link");
    std::os::unix::fs::symlink(&config.auth_key_file, &link).unwrap();
    assert!(resolver::load_secret(&link).is_err());
    let hard = fixture.directory.path().join("key-hardlink");
    fs::hard_link(&config.auth_key_file, &hard).unwrap();
    assert!(resolver::load_secret(&config.auth_key_file).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_socket_authenticates_notify_and_keeps_v2_requests_separate() {
    let fixture = Arc::new(Fixture::new());
    let service = fixture.service();
    let events = Arc::new(Events::default());
    let finalizer = audit_finalizer(events.clone());
    let bridge = Arc::new(SkillFsBridge::prepare(fixture.config()).unwrap());
    let application = crate::skill_application(
        finalizer,
        SkillSecExecutor::new(service.clone()).with_environment(bridge.environment()),
    );
    bridge.start_worker(application.clone()).unwrap();
    let dispatcher = Arc::new(SkillFsDispatcher::new(
        ordinary_dispatcher(application),
        Some(bridge.clone()),
    ));
    assert_eq!(
        dispatcher
            .dispatch_timeout(br#"{"method":"action.skill_sec","params":{"command":"scan"}}"#),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        dispatcher.dispatch_timeout(br#"{"method":"action.code_scan","params":{}}"#),
        None
    );
    assert_eq!(
        dispatcher.dispatch_timeout(br#"{"type":"auth.init"}"#),
        None
    );
    let socket = fixture.directory.path().join("daemon.sock");
    let bound = BoundUnixSocket::bind(&socket, 0o666).unwrap();
    let server = UnixService::new(
        bound,
        transport_config(),
        dispatcher,
        Arc::new(asc_daemon_handler::JsonRejectionEncoder),
    )
    .unwrap();
    let shutdown = ShutdownToken::new();
    let task = tokio::spawn(server.serve(shutdown.clone()));
    let path = socket.clone();
    let inputs = fixture.clone();
    tokio::task::spawn_blocking(move || assert_notify_contract(&inputs, &path))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if bridge.status()["processed"].as_u64().unwrap() >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(bridge.status()["processed"], 1);
    assert_eq!(bridge.status()["failed"], 0);
    assert!(!fixture.canonical.join("demo/.skill-meta").exists());
    assert!(
        fixture
            .live
            .join("demo/.skill-meta/activation.json")
            .exists()
    );
    let report_events = events.0.lock().unwrap().clone();
    assert_eq!(report_events.len(), 2);
    assert!(
        report_events
            .iter()
            .all(|e| e.uid == rustix::process::geteuid().as_raw())
    );
    assert_restart_and_failed_mapping(&fixture, &bridge, &service, &socket).await;
    let status = ordinary(
        &socket,
        json!({"method":"action.skill_sec","params":{"command":"status"}}),
    )
    .await;
    assert_eq!(status["result"]["data"]["skillfs"]["processed"], 2);
    let scan = ordinary(
        &socket,
        json!({"method":"action.code_scan","params":{"code":"echo safe","language":"bash"}}),
    )
    .await;
    assert_eq!(scan["result"]["ok"], true);
    let protocol_error = ordinary(
        &socket,
        json!({"id":"old-id","method":"action.code_scan","params":{}}),
    )
    .await;
    assert!(protocol_error.get("error").is_some());
    assert_activation_after_scan_error(&fixture, &bridge, &service, &events).await;
    shutdown.request();
    task.await.unwrap().unwrap();
    tokio::task::spawn_blocking(move || bridge.shutdown().unwrap())
        .await
        .unwrap();
}

fn assert_notify_contract(inputs: &Fixture, path: &Path) {
    let request = inputs.notification(&json!([".skill-meta/activation.json"]));
    let ignored = notify(path, &inputs.secret, &request, false, false).unwrap();
    assert!(uuid::Uuid::parse_str(ignored["request_id"].as_str().unwrap()).is_ok());
    assert_eq!(ignored["stdout"], "");
    assert_eq!(ignored["stderr"], "");
    assert_eq!(ignored["exit_code"], 0);
    assert_eq!(
        ignored["data"],
        json!({"schemaVersion":2,"accepted":true,"ignored":true,
            "reason":"metadata-only change", "skill": {
                "canonicalSkillDir":inputs.canonical.join("demo"), "skillName":"demo",
                "reportedSkillId":"demo", "eventKinds":["write"],
                "paths":[".skill-meta/activation.json"]}})
    );
    for (relative, accepted) in [
        ("apple/skill-discover", true),
        ("skill-discover", false),
        ("skill-discover/child", false),
        ("apple/.hidden", false),
    ] {
        let mut named = request.clone();
        named["params"]["canonicalSkillDir"] = json!(inputs.canonical.join(relative));
        named["params"]["skillId"] = json!(relative);
        let response = notify(path, &inputs.secret, &named, false, false).unwrap();
        assert_eq!(
            response["exit_code"],
            i32::from(!accepted),
            "{relative}: {response}"
        );
    }
    assert!(notify(path, &inputs.secret, &request, true, false).is_none());
    assert!(notify(path, &inputs.secret, &request, false, true).is_none());
    let mut invalid = inputs.notification(&json!(["../escape"]));
    let rejected = notify(path, &inputs.secret, &invalid, false, false).unwrap();
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["exit_code"], 1);
    assert_eq!(rejected["error"]["code"], "bad_request");
    assert_eq!(rejected["stderr"], rejected["error"]["message"]);
    invalid = inputs.notification(&json!([]));
    invalid["params"]["canonicalSkillDir"] = json!("/unconfigured/demo");
    assert_eq!(
        notify(path, &inputs.secret, &invalid, false, false).unwrap()["ok"],
        false
    );
    let mut plain = BufReader::new(UnixStream::connect(path).unwrap());
    send(&mut plain, &serde_json::to_vec(&request).unwrap());
    let rejected: Value = serde_json::from_slice(&line(&mut plain)).unwrap();
    assert!(rejected.get("error").is_some());
    for attempt in 0..3 {
        assert_eq!(
            notify(
                path,
                &inputs.secret,
                &inputs.notification(&json!(["SKILL.md", "SKILL.md"])),
                false,
                false
            )
            .unwrap()["data"],
            json!({"schemaVersion":2,"accepted":true,"ignored":false,"queued":true,
                "coalesced":attempt > 0,"skill":{
                    "canonicalSkillDir":inputs.canonical.join("demo"), "skillName":"demo",
                    "reportedSkillId":"demo", "eventKinds":["write"],"paths":["SKILL.md"]}})
        );
    }
}

fn transport_config() -> ServiceConfig {
    ServiceConfig {
        max_request_frame_bytes: 4 * 1024 * 1024,
        max_response_frame_bytes: 4 * 1024 * 1024,
        max_connections: 8,
        max_rejection_connections: 2,
        rejection_encode_timeout: Duration::from_millis(250),
        request_read_timeout: Duration::from_secs(5),
        dispatch_timeout: Duration::from_secs(5),
        response_write_timeout: Duration::from_secs(5),
        drain_timeout: Duration::from_secs(5),
        accept_error_backoff: Duration::from_millis(10),
    }
}

async fn ordinary(socket: &Path, request: Value) -> Value {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
    let mut bytes = serde_json::to_vec(&request).unwrap();
    bytes.push(b'\n');
    stream.write_all(&bytes).await.unwrap();
    let mut bytes = Vec::new();
    tokio::io::BufReader::new(stream)
        .read_until(b'\n', &mut bytes)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn assert_restart_and_failed_mapping(
    fixture: &Fixture,
    bridge: &SkillFsBridge,
    service: &SkillSecService,
    socket: &Path,
) {
    fs::write(
        fixture.live.join("demo/new-file.txt"),
        "changed while daemon was down",
    )
    .unwrap();
    bridge
        .schedule_reconcile(service.managed_skills().unwrap())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while bridge.status()["processed"].as_u64().unwrap() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let latest: Value = serde_json::from_slice(
        &fs::read(fixture.live.join("demo/.skill-meta/latest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(latest["versionId"], "v000002");
    fixture.mode.store(1, Ordering::Release);
    let status = ordinary(
        socket,
        json!({"method":"action.skill_sec","params":{"command":"status","verbose":true}}),
    )
    .await;
    assert_eq!(status["result"]["data"]["keys"]["initialized"], true);
    assert_eq!(status["result"]["data"]["skills"]["breakdown"]["error"], 1);
    assert_eq!(status["result"]["data"]["results"][0]["status"], "error");
    fixture.mode.store(0, Ordering::Release);
}

async fn assert_activation_after_scan_error(
    fixture: &Fixture,
    bridge: &SkillFsBridge,
    service: &SkillSecService,
    events: &Events,
) {
    fs::remove_file(fixture.live.join("demo/SKILL.md")).unwrap();
    let before = events.0.lock().unwrap().len();
    bridge
        .schedule_reconcile(service.managed_skills().unwrap())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while bridge.status()["processed"].as_u64().unwrap() < 3 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(bridge.status()["failed"], 1);
    let events = events.0.lock().unwrap();
    assert_eq!(events.len(), before + 2);
    let scan = serde_json::to_value(&events[before]).unwrap();
    let activate = serde_json::to_value(&events[before + 1]).unwrap();
    assert_eq!(scan["details"]["request"]["command"], "scan");
    assert_eq!(activate["details"]["request"]["command"], "activate");
}
