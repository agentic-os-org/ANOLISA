//! On-demand startup and external reuse with real detached service processes.
#![cfg(target_os = "linux")]

use aw_service::{
    launch_service::{ensure_service, EnsuredService},
    Operation,
};
use serde_json::json;
use std::{
    fs,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const TIMEOUT: Duration = Duration::from_secs(10);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/launch-tests");
        fs::create_dir_all(&parent).unwrap();
        let path = parent.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn config(&self, startup: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"apiVersion":"aw/v1alpha1","kind":"AWConfiguration","metadata":{"name":"launch"},"spec":{
            "daemon":{"startup":startup,"endpoint":"auto","state_dir":self.0.join("state")},
            "execution":{"guarantee":"native_hook","default_event_budget_ms":1000},
            "audit":{"enabled":true,"payload":"metadata_only"},
            "agents":{"qoder":{"adapter":"qoder","argv":["qodercli"]}},"providers":{},"events":{}}})).unwrap()
    }
    fn register(&self, service: &EnsuredService) {
        fs::write(self.0.join("process.json"), json!({"pid":service.pid,"socket":service.paths.socket,
            "command":[env!("CARGO_BIN_EXE_aw"),"serve","--config",service.paths.state_dir.join(format!("config-{}.yaml", service.paths.config_revision)),"--state-dir",service.paths.state_dir],
            "cwd":service.paths.state_dir,"log":service.paths.state_dir.join("service.log"),"ports":[],
            "expected_lifetime":"until this bounded test teardown", "stop":[env!("CARGO_BIN_EXE_aw"),"stop","--socket",service.paths.socket]}).to_string()).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Managed(EnsuredService);
impl Managed {
    fn start(fixture: &Fixture, bytes: &[u8]) -> Self {
        let service = Self(
            ensure_service(
                bytes,
                Path::new(env!("CARGO_BIN_EXE_aw")),
                Instant::now() + TIMEOUT,
            )
            .unwrap(),
        );
        assert!(service.0.started);
        fixture.register(&service.0);
        service
    }
}
impl Drop for Managed {
    fn drop(&mut self) {
        let stopped = if self.0.paths.socket.exists() {
            self.0
                .client
                .call(Operation::Stop, Instant::now() + TIMEOUT)
                .map(|_| ())
        } else {
            Ok(())
        };
        let reaped = self.0.reap_started(Instant::now() + TIMEOUT);
        if thread::panicking() {
            if stopped.is_err() || reaped.is_err() {
                eprintln!("startup test teardown: {stopped:?}, {reaped:?}");
            }
        } else {
            stopped.unwrap();
            reaped.unwrap();
            assert!(!self.0.paths.socket.exists());
            assert!(!Path::new(&format!("/proc/{}", self.0.pid)).exists());
        }
    }
}

#[test]
fn starts_once_reuses_exact_revision_and_detaches_standard_streams() {
    let fixture = Fixture::new();
    let bytes = fixture.config("on_demand");
    let service = Managed::start(&fixture, &bytes);
    let reused =
        ensure_service(&bytes, Path::new("/not-needed"), Instant::now() + TIMEOUT).unwrap();
    assert!(!reused.started);
    assert_eq!(reused.pid, service.0.pid);
    assert_eq!(reused.client.identity(), service.0.client.identity());
    drop(reused);
    assert_eq!(
        service
            .0
            .client
            .call(Operation::Status, Instant::now() + TIMEOUT)
            .unwrap()["pid"],
        service.0.pid
    );
    for fd in [0, 1] {
        assert_eq!(
            fs::read_link(format!("/proc/{}/fd/{fd}", service.0.pid)).unwrap(),
            Path::new("/dev/null")
        );
    }
    assert_eq!(
        fs::read_link(format!("/proc/{}/fd/2", service.0.pid)).unwrap(),
        service.0.paths.state_dir.join("service.log")
    );
    let stat = fs::read_to_string(format!("/proc/{}/stat", service.0.pid)).unwrap();
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .collect();
    assert_eq!(fields[3].parse::<u32>().unwrap(), service.0.pid);
    for entry in fs::read_dir(format!("/proc/{}/fd", service.0.pid)).unwrap() {
        if let Ok(path) = fs::read_link(entry.unwrap().path()) {
            assert_ne!(path, service.0.paths.state_dir.join("launch.lock"));
        }
    }
    let mut changed = bytes.clone();
    changed.push(b'\n');
    assert!(ensure_service(
        &changed,
        Path::new(env!("CARGO_BIN_EXE_aw")),
        Instant::now() + TIMEOUT
    )
    .err()
    .unwrap()
    .to_string()
    .contains("revision_mismatch"));
    let snapshot = service.0.paths.state_dir.join(format!(
        "config-{}.yaml",
        service.0.client.identity().config_revision
    ));
    assert_eq!(fs::read(snapshot).unwrap(), bytes);
}

#[test]
fn concurrent_launchers_share_one_started_process() {
    let fixture = Fixture::new();
    let bytes = fixture.config("on_demand");
    let services = thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    ensure_service(
                        &bytes,
                        Path::new(env!("CARGO_BIN_EXE_aw")),
                        Instant::now() + TIMEOUT,
                    )
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let mut started = Vec::new();
    let mut reused = Vec::new();
    let mut errors = Vec::new();
    for service in services {
        match service {
            Ok(service) if service.started => started.push(Managed(service)),
            Ok(service) => reused.push(service),
            Err(error) => errors.push(error),
        }
    }
    assert!(errors.is_empty(), "concurrent startup failures: {errors:?}");
    assert_eq!(started.len(), 1);
    let service = started.pop().unwrap();
    fixture.register(&service.0);
    for other in reused {
        assert_eq!(other.pid, service.0.pid);
        assert_eq!(other.client.identity(), service.0.client.identity());
    }
}

#[test]
fn external_mode_requires_the_exact_live_configuration() {
    let fixture = Fixture::new();
    let bytes = fixture.config("external");
    let server = aw_service::Server::bind(bytes.clone(), fixture.0.join("state")).unwrap();
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal = std::sync::Arc::clone(&cancelled);
    let worker = thread::spawn(move || server.run(&signal));
    let result = ensure_service(
        &bytes,
        Path::new("/must-not-be-executed"),
        Instant::now() + TIMEOUT,
    );
    cancelled.store(true, Ordering::Release);
    worker.join().unwrap().unwrap();
    let service = result.unwrap();
    assert!(!service.started);
    assert_eq!(service.pid, std::process::id());
    assert!(!fixture.0.join("state/launch.lock").exists());
    assert!(!fixture.0.join("state/service.log").exists());
}

#[test]
fn stale_socket_is_preserved_and_never_triggers_a_replacement_daemon() {
    let fixture = Fixture::new();
    let state = fixture.0.join("state");
    fs::DirBuilder::new().mode(0o700).create(&state).unwrap();
    let socket = state.join("aw.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let inode = fs::metadata(&socket).unwrap().ino();
    drop(listener);
    assert!(ensure_service(
        &fixture.config("on_demand"),
        Path::new(env!("CARGO_BIN_EXE_aw")),
        Instant::now() + TIMEOUT
    )
    .is_err());
    assert_eq!(fs::metadata(&socket).unwrap().ino(), inode);
    assert!(!state.join("service.log").exists());
}

#[test]
fn startup_timeout_terminates_and_reaps_only_its_owned_child() {
    let fixture = Fixture::new();
    let executable = fixture.0.join("unready-provider");
    fs::write(
        &executable,
        b"#!/bin/sh\nprintf '%s\\n' \"$$\" > fixture.pid\nexec /bin/sleep 30\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let started = Instant::now();
    let (observed, result) = thread::scope(|scope| {
        let worker = scope
            .spawn(|| ensure_service(&fixture.config("on_demand"), &executable, started + TIMEOUT));
        // Observe the child before its startup deadline: timing out before exec
        // cannot demonstrate that the launcher terminated and reaped a process.
        let observation_deadline = started + TIMEOUT / 2;
        let observed = loop {
            if let Ok(text) = fs::read_to_string(fixture.0.join("state/fixture.pid")) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    break Some((pid, Path::new(&format!("/proc/{pid}")).exists()));
                }
            }
            if worker.is_finished() || Instant::now() >= observation_deadline {
                break None;
            }
            thread::sleep(Duration::from_millis(10));
        };
        (observed, worker.join().unwrap())
    });
    assert!(
        matches!(result, Err(aw_service::Error::Rejected(ref code)) if code == "service_startup_deadline")
    );
    assert!(started.elapsed() < TIMEOUT + Duration::from_secs(5));
    let (pid, was_running) =
        observed.expect("startup fixture did not publish its PID before timeout");
    assert!(was_running, "startup fixture exited before the deadline");
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert!(!fixture.0.join("state/aw.sock").exists());
}

#[test]
fn a_client_clone_can_stop_the_service_before_the_launcher_reaps_it() {
    let fixture = Fixture::new();
    let bytes = fixture.config("on_demand");
    let mut service = Managed::start(&fixture, &bytes);
    // The real launch CLI owns this handle through Agent exit; a client clone
    // remains valid independently of the original launcher-side client value.
    let detached = service.0.client.clone();
    let pid = service.0.pid;
    detached
        .call(Operation::Status, Instant::now() + TIMEOUT)
        .unwrap();
    detached
        .call(Operation::Stop, Instant::now() + TIMEOUT)
        .unwrap();
    service.0.reap_started(Instant::now() + TIMEOUT).unwrap();
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}
