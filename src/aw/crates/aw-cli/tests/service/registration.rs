//! Real independent owner processes exercise the JSON CLI and service peer checks.

use super::*;
use std::io::{BufRead, BufReader};

struct Owner {
    child: Child,
    output: BufReader<std::process::ChildStdout>,
    hello: Value,
}

impl Owner {
    fn start(fixture: &Fixture, service: &Running, adapter: &str) -> Self {
        let mut child = Command::new("python3")
            .args([
                "-I",
                &format!(
                    "{}/tests/fixtures/independent_owner.py",
                    env!("CARGO_MANIFEST_DIR")
                ),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        writeln!(
            child.stdin.as_mut().unwrap(),
            "{}",
            json!({"aw":env!("CARGO_BIN_EXE_aw"),
            "socket":service.socket,"cwd":fixture.0,"adapter":adapter})
        )
        .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let hello = serde_json::from_str(&line).unwrap();
        Self {
            child,
            output,
            hello,
        }
    }

    fn send(&mut self, request: Value) -> Value {
        writeln!(self.child.stdin.as_mut().unwrap(), "{request}").unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("owner response {line:?}: {error}"))
    }

    fn action(&mut self, action: &str, tag: &str) -> Value {
        self.send(json!({"action":action,"tag":tag}))
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn configured(fixture: &Fixture, adapter: &str) -> Value {
    let mut document = fixture.document();
    document["spec"]["agents"]["target"]["adapter"] = json!(adapter);
    document
}

fn bindings(service: &Running) -> u64 {
    service
        .client
        .call(Operation::Status, Instant::now() + TIMEOUT)
        .unwrap()["bindings"]
        .as_u64()
        .unwrap()
}

fn wait_bindings(service: &Running, expected: u64) {
    let deadline = Instant::now() + TIMEOUT;
    while bindings(service) != expected {
        assert!(
            Instant::now() < deadline,
            "bindings did not converge to {expected}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn independent_cli_prepares_confirms_invokes_and_releases_two_instances_per_owner() {
    for adapter in ["qoder-sdk", "qwenpaw"] {
        let fixture = Fixture::new();
        let service = Running::start(&fixture, &configured(&fixture, adapter));
        let mut owner = Owner::start(&fixture, &service, adapter);
        let first = owner.action("register", "first");
        assert_eq!(first["result"]["state"], "pending_ready", "{first}");
        assert_eq!(owner.action("call", "first")["exit"], 1);
        assert!(fixture.calls("invoke").is_empty());
        // Discard the successful reply and recover by the original request key.
        let recovered = owner.action("query", "first");
        assert_eq!(recovered["result"]["handle"], first["result"]["handle"]);
        let duplicate = owner.action("register", "first");
        assert_eq!(duplicate["result"]["handle"], first["result"]["handle"]);
        assert_eq!(fixture.calls("describe").len(), 1);
        assert_eq!(owner.action("ready", "first")["result"]["state"], "ready");
        assert_eq!(owner.action("call", "first")["executed"], true);
        let blocked = owner.send(json!({"action":"call","tag":"first","scenario":"block"}));
        assert_eq!(blocked["executed"], false, "{blocked}");
        assert_eq!(blocked["exit"], 2);
        let second = owner.action("register", "second");
        assert_eq!(second["result"]["state"], "pending_ready");
        assert_eq!(owner.action("ready", "second")["result"]["state"], "ready");
        assert_eq!(bindings(&service), 2);
        assert_eq!(
            owner.action("release", "first")["result"]["state"],
            "released"
        );
        assert_eq!(
            owner.action("release", "first")["result"]["state"],
            "released"
        );
        assert_eq!(owner.action("call", "second")["executed"], true);
        assert_eq!(
            owner.action("release", "second")["result"]["state"],
            "released"
        );
        assert_eq!(bindings(&service), 0);
        owner.action("exit", "first");
        assert!(owner.child.wait().unwrap().success());
        fixture.assert_reaped();
    }
}

#[test]
fn independent_owners_reject_forwarded_handles_workers_and_old_generations() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &configured(&fixture, "qoder-sdk"));
    let mut first = Owner::start(&fixture, &service, "qoder-sdk");
    let mut second = Owner::start(&fixture, &service, "qoder-sdk");
    let mut forged = serde_json::to_value(register_direct(&fixture)).unwrap();
    forged["key"]["owner"] = second.hello["owner"].clone();
    assert_eq!(
        first.send(json!({"action":"lifecycle","command":forged}))["error"],
        "registration_peer_mismatch"
    );
    forged["key"]["owner"] = first.hello["owner"].clone();
    forged["key"]["owner"]["start_ticks"] = json!("0");
    assert_eq!(
        first.send(json!({"action":"lifecycle","command":forged}))["error"],
        "registration_owner_mismatch"
    );
    let one = first.action("register", "first");
    let two = second.action("register", "first");
    let handle = one["result"]["handle"].clone();
    let foreign =
        second.send(json!({"action":"lifecycle","command":{"action":"release","handle":handle}}));
    assert_eq!(foreign["error"], "registration_peer_mismatch");
    let raw = second.send(json!({"action":"raw","operation":{"method":"release_instance","instance_id":handle["binding"]["instance_id"]}}));
    assert!(raw["stderr"]
        .as_str()
        .unwrap()
        .contains("registration_peer_mismatch"));
    first.action("ready", "first");
    let wrong_worker = first.send(json!({"action":"raw","operation":{
        "method":"open_scope","parent_id":handle["binding"]["instance_id"],
        "pid":second.hello["owner"]["pid"],"cwd":fixture.0,"environment":{}}}));
    assert!(wrong_worker["stderr"]
        .as_str()
        .unwrap()
        .contains("bridge_process_mismatch"));
    let mut stale = handle.clone();
    stale["binding"]["identity"]["generation"] = json!("old-generation");
    let rejected =
        first.send(json!({"action":"lifecycle","command":{"action":"release","handle":stale}}));
    assert_eq!(rejected["error"], "stale_service");
    let mut crossed = handle.clone();
    crossed["binding"] = two["result"]["handle"]["binding"].clone();
    let rejected =
        first.send(json!({"action":"lifecycle","command":{"action":"release","handle":crossed}}));
    assert_eq!(rejected["error"], "registration_handle_mismatch");
    second.action("ready", "first");
    first.action("exit", "first");
    first.child.wait().unwrap();
    wait_bindings(&service, 1);
    assert_eq!(second.action("call", "first")["executed"], true);
    second.action("release", "first");
    wait_bindings(&service, 0);
}

fn register_direct(fixture: &Fixture) -> aw_service::RegistrationCommand {
    let Operation::Bind {
        target,
        mut capabilities,
        cwd,
        environment,
    } = fixture.bind_operation()
    else {
        unreachable!()
    };
    capabilities.adapter = "qoder-sdk".into();
    capabilities.version = "1.0.56".into();
    capabilities.entrypoint = "sdk-worker".into();
    aw_service::RegistrationCommand::Register {
        key: aw_service::RegistrationKey {
            request_id: "direct".into(),
            owner: aw_service::RegistrationOwner {
                pid: std::process::id(),
                start_ticks: aw_service::ProcessIdentity::read(std::process::id())
                    .unwrap()
                    .metadata()["start_ticks"]
                    .as_str()
                    .unwrap()
                    .into(),
            },
        },
        target,
        capabilities,
        cwd,
        environment,
    }
}

fn registration(
    service: &Running,
    command: aw_service::RegistrationCommand,
) -> Result<Value, aw_service::Error> {
    service.client.call(
        Operation::Registration { command },
        Instant::now() + TIMEOUT,
    )
}

#[test]
fn independent_release_drains_provider_descendants_before_reclaiming_capacity() {
    use aw_service::{Binding, RegistrationCommand as R, RegistrationHandle};
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &configured(&fixture, "qoder-sdk"));
    let prepared = registration(&service, register_direct(&fixture)).unwrap();
    let handle: RegistrationHandle = serde_json::from_value(prepared["handle"].clone()).unwrap();
    registration(
        &service,
        R::Ready {
            handle: handle.clone(),
            policy_revision: prepared["policy_revision"].as_str().unwrap().into(),
        },
    )
    .unwrap();
    let task: Binding = serde_json::from_value(
        service
            .client
            .call(
                Operation::OpenScope {
                    parent_id: handle.binding.instance_id.clone(),
                    pid: std::process::id(),
                    cwd: fixture.0.to_str().unwrap().into(),
                    environment: Default::default(),
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap(),
    )
    .unwrap();
    service
        .client
        .call(
            Operation::AcknowledgeScope {
                instance_id: task.instance_id.clone(),
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap();
    let mut value = event(&task, "tool.before", "descendant");
    value["agent"]["adapter"] = json!("qoder-sdk");
    let event = open(&service.client, &task, value);
    thread::scope(|scope| {
        let invocation = scope.spawn(|| {
            service
                .client
                .call(invoke(&event, "check"), Instant::now() + TIMEOUT)
        });
        let deadline = Instant::now() + TIMEOUT;
        while !fixture.0.join("descendant-ready").exists() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            registration(&service, R::Release { handle }).unwrap()["state"],
            "released"
        );
        let result = invocation.join().unwrap().unwrap();
        assert_eq!(result["status"], "error");
        fixture.assert_reaped();
        assert_eq!(bindings(&service), 0);
    });
}

#[test]
fn independent_preparation_failure_keeps_audit_and_never_replays_the_request() {
    use aw_service::RegistrationCommand as R;
    let fixture = Fixture::new();
    let mut document = configured(&fixture, "qoder-sdk");
    document["spec"]["providers"]["policy"]["config"]["reject"] = json!(true);
    let service = Running::start(&fixture, &document);
    let command = register_direct(&fixture);
    let R::Register { key, .. } = &command else {
        unreachable!()
    };
    let failed = registration(&service, command.clone());
    let Err(aw_service::Error::Attempt { audit_key, .. }) = failed else {
        panic!("{failed:?}")
    };
    assert_eq!(audit(&service.client, &audit_key)["terminal"], true);
    assert_eq!(
        registration(&service, R::Query { key: key.clone() }).unwrap()["state"],
        "failed"
    );
    assert_eq!(registration(&service, command).unwrap()["state"], "failed");
    assert_eq!(fixture.calls("describe").len(), 1);
    assert_eq!(bindings(&service), 0);
    fixture.assert_reaped();
}

#[test]
fn missing_registration_socket_reply_is_recovered_without_new_preparation() {
    use aw_service::RegistrationCommand as R;
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &configured(&fixture, "qoder-sdk"));
    let command = register_direct(&fixture);
    let R::Register { key, .. } = &command else {
        unreachable!()
    };
    // SAFETY: clock_gettime writes the supplied initialized timespec.
    let mut now: libc::timespec = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) },
        0
    );
    let request = Request {
        api_version: VERSION.into(),
        identity: Some(service.client.identity().clone()),
        deadline_ns: now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64 + 10_000_000_000,
        operation: Operation::Registration {
            command: command.clone(),
        },
    };
    let bytes = serde_json::to_vec(&request).unwrap();
    let mut stream = UnixStream::connect(&service.socket).unwrap();
    stream.set_write_timeout(Some(TIMEOUT)).unwrap();
    stream.shutdown(std::net::Shutdown::Read).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&bytes).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let recovered = loop {
        let result = registration(&service, R::Query { key: key.clone() });
        match result {
            Ok(value) if value["state"] == "pending_ready" => break value,
            Ok(value) => assert_eq!(value["state"], "preparing", "{value}"),
            Err(aw_service::Error::Rejected(code)) => assert_eq!(code, "registration_unknown"),
            result => panic!("{result:?}"),
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    };
    drop(stream);
    assert_eq!(
        registration(&service, command).unwrap()["handle"],
        recovered["handle"]
    );
    assert_eq!(fixture.calls("describe").len(), 1);
    assert_eq!(
        registration(
            &service,
            R::Release {
                handle: serde_json::from_value(recovered["handle"].clone()).unwrap()
            }
        )
        .unwrap()["state"],
        "released"
    );
    fixture.assert_reaped();
}

#[test]
fn owner_exit_cancels_an_inflight_registration_task_and_reaps_its_provider() {
    let fixture = Fixture::new();
    let service = Running::start(&fixture, &configured(&fixture, "qoder-sdk"));
    let mut owner = Owner::start(&fixture, &service, "qoder-sdk");
    owner.action("register", "first");
    owner.action("ready", "first");
    writeln!(
        owner.child.stdin.as_mut().unwrap(),
        "{}",
        json!({"action":"call","tag":"first","scenario":"descendant"})
    )
    .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !fixture.0.join("descendant-ready").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let children = fs::read_to_string(format!(
        "/proc/{}/task/{}/children",
        owner.child.id(),
        owner.child.id()
    ))
    .unwrap();
    let callers = children
        .split_whitespace()
        .map(|pid| aw_service::ProcessIdentity::read(pid.parse().unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert!(!callers.is_empty());
    owner.child.kill().unwrap();
    owner.child.wait().unwrap();
    wait_bindings(&service, 0);
    fixture.assert_reaped();
    for caller in callers {
        while caller.check().is_ok() {
            assert!(
                Instant::now() < deadline,
                "orphaned CLI did not exit after cancellation"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
