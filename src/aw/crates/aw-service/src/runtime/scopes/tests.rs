//! Exercise dispatch and acknowledgement without a maintenance thread masking races.

use super::*;
use crate::{runtime::evidence, runtime::EventEntry, runtime::Job, Capabilities, Operation};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{mpsc, TryLockError},
};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Fixture {
    runtime: Arc<Runtime>,
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("scope-races-")
            .tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))
            .unwrap();
        let python = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("python3"))
            .find(|path| path.is_file())
            .unwrap()
            .canonicalize()
            .unwrap();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scopes.py");
        let mut document = aw_config::Validator::new()
            .unwrap()
            .parse(include_bytes!(
                "../../../../aw-config/examples/aw.minimal.yaml"
            ))
            .unwrap()
            .as_value()
            .clone();
        document["spec"]["providers"]["policy"] = json!({
            "protocol":aw_provider::VERSION, "config":{}, "timeout_ms":5000,
            "max_output_bytes":65536,
            "transport":{"type":"stdio", "location":"agent", "argv":[python,"-I",script,directory.path()]}
        });
        document["spec"]["events"]["tool.after"]["steps"] = json!([
            {"id":"first", "provider":"policy", "operation":"first", "effects":["observe"], "on_error":"report"},
            {"id":"second", "provider":"policy", "operation":"second", "effects":["observe"], "on_error":"report"}
        ]);
        let runtime = Runtime::new(
            serde_json::to_vec(&document).unwrap(),
            &directory.path().join("audit"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        Self { runtime, directory }
    }

    fn call(&self, operation: Operation) -> Result<Value> {
        self.runtime
            .handle(operation, Instant::now() + TIMEOUT, &AtomicBool::new(false))
    }

    fn task(&self, launcher: u32, worker: u32) -> Binding {
        let cwd = self.directory.path().to_str().unwrap().to_owned();
        let root = self
            .call(Operation::Bind {
                target: "qoder".into(),
                capabilities: Capabilities {
                    failure_actions: Default::default(),
                    adapter: "qoder".into(),
                    version: "fixture".into(),
                    entrypoint: "fixture".into(),
                    events: BTreeMap::from([
                        ("tool.before".into(), vec!["observe".into()]),
                        ("tool.after".into(), vec!["observe".into()]),
                    ]),
                },
                cwd: cwd.clone(),
                environment: BTreeMap::new(),
            })
            .unwrap();
        let parent_id = root["instance_id"].as_str().unwrap().to_owned();
        self.call(Operation::OwnInstance {
            instance_id: parent_id.clone(),
            pid: launcher,
        })
        .unwrap();
        serde_json::from_value(
            self.call(Operation::OpenScope {
                parent_id,
                pid: worker,
                cwd,
                environment: BTreeMap::new(),
            })
            .unwrap(),
        )
        .unwrap()
    }

    fn acknowledge(&self, task: &Binding) -> Result<Value> {
        self.call(Operation::AcknowledgeScope {
            instance_id: task.instance_id.clone(),
        })
    }

    fn acknowledgements(&self) -> usize {
        fs::read_dir(self.directory.path().join("audit"))
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "jsonl")
            })
            .flat_map(|entry| {
                fs::read_to_string(entry.path())
                    .unwrap()
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .map(|line| serde_json::from_str::<Value>(&line).unwrap())
            .filter(|value| value["record"]["plan"]["kind"] == "scope_acknowledged")
            .count()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.runtime.maintain(true).unwrap();
    }
}

struct OwnerProcess {
    child: Child,
    directory: PathBuf,
    launcher: u32,
    worker: u32,
}

impl OwnerProcess {
    fn new(directory: &Path) -> Self {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scope-owner.py");
        let child = Command::new("python3")
            .arg("-I")
            .arg(script)
            .arg(directory)
            .process_group(0)
            .spawn()
            .unwrap();
        let mut owner = Self {
            child,
            directory: directory.into(),
            launcher: 0,
            worker: 0,
        };
        wait_for(|| directory.join("owners.json").exists());
        let pids: Value =
            serde_json::from_slice(&fs::read(directory.join("owners.json")).unwrap()).unwrap();
        owner.launcher = pids["launcher"].as_u64().unwrap() as u32;
        owner.worker = pids["worker"].as_u64().unwrap() as u32;
        owner
    }

    fn stop(&self, root: bool) {
        fs::write(
            self.directory.join("owner-stop.tmp"),
            if root { "launcher" } else { "worker" },
        )
        .unwrap();
        fs::rename(
            self.directory.join("owner-stop.tmp"),
            self.directory.join("owner-stop"),
        )
        .unwrap();
        wait_for(|| self.directory.join("owner-stopped").exists());
        let pid = if root { self.launcher } else { self.worker };
        wait_for(|| Process::read(pid).is_err());
    }
}

impl Drop for OwnerProcess {
    fn drop(&mut self) {
        fs::write(self.directory.join("owners-close"), "close").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if self.child.try_wait().unwrap().is_none() {
            // SAFETY: this fixture created the child's private process group.
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
        }
        self.child.wait().unwrap();
        for pid in [self.launcher, self.worker] {
            assert!(!Path::new("/proc").join(pid.to_string()).exists());
        }
    }
}

fn wait_for(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "fixture did not reach the required boundary"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn queued_step_rechecks_worker_and_launcher_before_provider_dispatch() {
    owner_death_before_dispatch(false);
}

#[test]
fn audited_step_rechecks_worker_and_launcher_before_provider_dispatch() {
    owner_death_before_dispatch(true);
}

fn owner_death_before_dispatch(during_audit: bool) {
    for root_dies in [false, true] {
        let fixture = Fixture::new();
        let runtime = &fixture.runtime;
        let owner = OwnerProcess::new(fixture.directory.path());
        let task = fixture.task(owner.launcher, owner.worker);
        fixture.acknowledge(&task).unwrap();
        let instance = &task.instance_id;
        let host = Arc::clone(
            &runtime.instances.lock().unwrap()[instance]
                .as_ref()
                .unwrap()
                .host,
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        let context = Arc::new(evidence::Cell::default());
        let event = host
            .event(
                json!({"name":"tool.after","session_id":"session",
            "agent":{"adapter":"qoder","binding_id":"qoder","instance_id":instance},
            "tool":{"name":"tool","native_name":"fixture","call_id":"call","input":{},"result":{}},"native":{}}),
                Instant::now() + TIMEOUT,
                &cancelled,
            )
            .unwrap();
        let key = runtime.identifier().unwrap();
        runtime.audit.claim(&key, json!({"kind":"event"})).unwrap();
        let (sender, receiver) = mpsc::sync_channel(4);
        runtime.events.lock().unwrap().insert(
            key.clone(),
            EventEntry {
                instance_id: instance.clone(),
                sender: sender.clone(),
                cancelled: Arc::clone(&cancelled),
                context: Arc::clone(&context),
            },
        );
        let invoke = |step: &str| {
            fixture.call(Operation::InvokeStep {
                event_id: key.clone(),
                instance_id: instance.clone(),
                step_id: step.into(),
                native_environment: None,
            })
        };
        // The production ordered queue is exercised without asynchronous maintenance.
        let rejected = thread::scope(|threads| {
            let worker = threads.spawn(|| {
                runtime.event_loop(
                    (&key, instance),
                    &event,
                    (&cancelled, &context),
                    receiver,
                    false,
                    true,
                )
            });
            let result = if during_audit {
                let journal = runtime.audit.hold_journal();
                let first = threads.spawn(|| invoke("first"));
                // Reaching append proves the first owner check succeeded. The
                // held journal prevents its start record from completing yet.
                wait_for(|| runtime.audit.started_appends() == 1);
                assert!(!fixture.directory.path().join("first").exists());
                owner.stop(root_dies);
                fs::write(fixture.directory.path().join("release"), "ready").unwrap();
                drop(journal);
                first.join().unwrap()
            } else {
                let first = threads.spawn(|| invoke("first"));
                wait_for(|| fixture.directory.path().join("first").exists());
                let second = threads.spawn(|| invoke("second"));
                wait_for(|| runtime.calls.load(Ordering::Acquire) == 2);
                assert!(!fixture.directory.path().join("second").exists());
                owner.stop(root_dies);
                fs::write(fixture.directory.path().join("release"), "ready").unwrap();
                assert_eq!(first.join().unwrap().unwrap()["status"], "ok");
                second.join().unwrap()
            };
            let (reply, closed) = mpsc::sync_channel(1);
            sender.send(Job::Close { reply }).unwrap();
            closed.recv_timeout(TIMEOUT).unwrap().unwrap();
            worker.join().unwrap();
            result
        });
        runtime.audit.release(&key);
        assert!(matches!(rejected, Err(Error::Rejected(code)) if code == "stale_process"));
        let blocked_step = if during_audit { "first" } else { "second" };
        assert!(
            !fixture.directory.path().join(blocked_step).exists(),
            "dead owner dispatched its Provider after {blocked_step} admission"
        );
        if !during_audit {
            let pid = fs::read_to_string(fixture.directory.path().join("first")).unwrap();
            assert!(!Path::new("/proc").join(pid).exists());
        }
        let records = runtime.audit.read(&key).unwrap();
        assert!(records["records"].as_array().unwrap().iter().any(|record| {
            record["record"]["phase"] == "started" && record["record"]["step_id"] == "first"
        }));
        runtime.maintain(false).unwrap();
        assert_eq!(
            fixture.call(Operation::Status).unwrap()["bindings"],
            if root_dies { 0 } else { 1 }
        );
    }
}

#[test]
fn committed_acknowledgement_survives_audit_delay_across_handoff_deadline() {
    let fixture = Fixture::new();
    let task = fixture.task(std::process::id(), std::process::id());
    let deadline = Instant::now() + Duration::from_secs(1);
    fixture
        .runtime
        .owners
        .lock()
        .unwrap()
        .get_mut(&task.instance_id)
        .unwrap()
        .acknowledge_by = Some(deadline);
    let (acknowledged, repeated) = thread::scope(|threads| {
        let journal = fixture.runtime.audit.hold_journal();
        let acknowledging = threads.spawn(|| fixture.acknowledge(&task));
        wait_for(|| {
            matches!(
                fixture.runtime.owners.try_lock(),
                Err(TryLockError::WouldBlock)
            )
        });
        thread::sleep(
            deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(20),
        );
        let repeated = threads.spawn(|| fixture.acknowledge(&task));
        drop(journal);
        (acknowledging.join().unwrap(), repeated.join().unwrap())
    });
    assert_eq!(acknowledged.unwrap()["acknowledged"], true);
    assert!(repeated.is_err());
    assert_eq!(fixture.acknowledgements(), 1);
    fixture
        .runtime
        .check_scope_owner(&task.instance_id)
        .unwrap();
    fixture.runtime.maintain(false).unwrap();
    assert_eq!(fixture.call(Operation::Status).unwrap()["bindings"], 2);
}

#[test]
fn expired_acknowledgement_has_no_success_record() {
    let fixture = Fixture::new();
    let task = fixture.task(std::process::id(), std::process::id());
    fixture
        .runtime
        .owners
        .lock()
        .unwrap()
        .get_mut(&task.instance_id)
        .unwrap()
        .acknowledge_by = Some(Instant::now());
    assert!(
        matches!(fixture.acknowledge(&task), Err(Error::Rejected(code)) if code == "scope_acknowledgement_expired")
    );
    assert_eq!(fixture.acknowledgements(), 0);
    fixture.runtime.maintain(false).unwrap();
    assert_eq!(fixture.call(Operation::Status).unwrap()["bindings"], 1);
}

#[test]
fn failed_acknowledgement_audit_cancels_committed_task() {
    let fixture = Fixture::new();
    let task = fixture.task(std::process::id(), std::process::id());
    let path = fixture.directory.path();
    fs::rename(path.join("audit"), path.join("audit-original")).unwrap();
    fs::write(path.join("audit"), "not a directory").unwrap();
    assert!(
        matches!(fixture.acknowledge(&task), Err(Error::Rejected(code)) if code == "audit_unavailable")
    );
    assert!(
        matches!(fixture.runtime.check_scope_owner(&task.instance_id), Err(Error::Rejected(code)) if code == "scope_closing")
    );
    assert!(
        matches!(fixture.runtime.maintain(false), Err(Error::Rejected(code)) if code == "audit_unavailable")
    );
    assert_eq!(fixture.call(Operation::Status).unwrap()["bindings"], 1);
}
