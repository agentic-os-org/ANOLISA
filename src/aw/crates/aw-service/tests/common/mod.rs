//! Real service fixtures retain process identities and clean only task-owned files.

use aw_service::{Binding, Capabilities, Client, EventHandle, Operation, Server};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
pub const TIMEOUT: Duration = Duration::from_secs(10);
pub const PRIVATE_MARKER: &str = "private-input-config-output-must-not-be-audited";

pub struct Fixture(pub PathBuf);

impl Fixture {
    pub fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/service-tests");
        fs::create_dir_all(&parent).unwrap();
        let path = parent.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    pub fn state(&self) -> PathBuf {
        self.0.join("state")
    }

    pub fn document(&self) -> Value {
        let python = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|directory| directory.join("python3"))
            .find(|candidate| candidate.is_file())
            .expect("service integration tests require python3 on PATH");
        let python = fs::canonicalize(python).unwrap();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider.py");
        json!({
            "apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
            "metadata": {"name": "service-fixture"},
            "spec": {
                "daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": "auto"},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                "audit": {"enabled": true, "payload": "metadata_only"},
                "agents": {"target": {"adapter": "qoder", "argv": ["/no-agent-is-started"]}},
                "providers": {"policy": {
                    "protocol": aw_provider::VERSION,
                    "transport": {"type": "stdio", "location": "agent", "argv": [python, "-I", script, self.0]},
                    "timeout_ms": 5000, "max_output_bytes": 65536,
                    "config": {"private_marker": PRIVATE_MARKER, "delays_ms": {}}
                }},
                "events": {
                    "tool.before": {"enabled": true, "steps": [{
                        "id": "check", "provider": "policy", "operation": "check",
                        "effects": ["observe", "block"], "on_error": "block"
                    }]},
                    "tool.after": {"enabled": true, "steps": [{
                        "id": "record", "provider": "policy", "operation": "record",
                        "effects": ["observe"], "on_error": "report"
                    }]}
                }
            }
        })
    }

    pub fn bind(&self, client: &Client) -> Binding {
        serde_json::from_value(
            client
                .call(self.bind_operation(), Instant::now() + TIMEOUT)
                .unwrap(),
        )
        .unwrap()
    }

    pub fn bind_operation(&self) -> Operation {
        Operation::Bind {
            target: "target".into(),
            capabilities: Capabilities {
                adapter: "qoder".into(),
                version: "fixture-version".into(),
                entrypoint: "fixture-entrypoint".into(),
                events: BTreeMap::from([
                    ("tool.before".into(), vec!["observe".into(), "block".into()]),
                    ("tool.after".into(), vec!["observe".into()]),
                ]),
            },
            cwd: self.0.to_str().unwrap().into(),
            environment: BTreeMap::from([("AW_SERVICE_MARKER".into(), "explicit-context".into())]),
        }
    }

    pub fn calls(&self, method: &str) -> Vec<Value> {
        fs::read_dir(&self.0)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".call.json"))
            .map(|entry| serde_json::from_slice::<Value>(&fs::read(entry.path()).unwrap()).unwrap())
            .filter(|entry| entry["request"]["method"] == method)
            .collect()
    }

    pub fn wait_for_calls(&self, method: &str, count: usize) {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            if self.calls(method).len() >= count {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("did not observe {count} {method} calls before deadline");
    }

    pub fn assert_reaped(&self) {
        assert!(
            self.remaining_processes().is_empty(),
            "Provider process remains"
        );
    }

    fn remaining_processes(&self) -> Vec<u32> {
        fs::read_dir(&self.0)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "pid"))
            .filter_map(|entry| {
                let record = fs::read_to_string(entry.path()).unwrap();
                let (pid, started) = record.trim().split_once(' ').unwrap();
                let pid: u32 = pid.parse().unwrap();
                let started: u64 = started.parse().unwrap();
                let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
                    Ok(stat) => stat,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
                    Err(error) => panic!("cannot inspect Provider PID {pid}: {error}"),
                };
                let fields: Vec<_> = stat
                    .rsplit_once(')')
                    .unwrap()
                    .1
                    .split_whitespace()
                    .collect();
                (fields[19].parse::<u64>().unwrap() == started).then_some(pid)
            })
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let remaining = self.remaining_processes();
        let removed = fs::remove_dir_all(&self.0);
        if thread::panicking() {
            if !remaining.is_empty() || removed.is_err() {
                eprintln!("service fixture cleanup: PIDs {remaining:?}, directory {removed:?}");
            }
        } else {
            assert!(remaining.is_empty(), "Provider PIDs remain: {remaining:?}");
            removed.unwrap();
            assert!(!self.0.exists());
        }
    }
}

pub struct Running {
    pub client: Client,
    pub socket: PathBuf,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<(), aw_service::Error>>>,
}

impl Running {
    pub fn start(fixture: &Fixture, document: &Value) -> Self {
        let server = Server::bind(serde_json::to_vec(document).unwrap(), fixture.state()).unwrap();
        let socket = server.socket_path();
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancelled);
        let worker = thread::spawn(move || server.run(&signal));
        let client = match Client::connect(&socket, Instant::now() + TIMEOUT) {
            Ok(client) => client,
            Err(error) => {
                cancelled.store(true, Ordering::Release);
                let _ = worker.join();
                panic!("service client could not connect: {error}");
            }
        };
        Self {
            client,
            socket,
            cancelled,
            worker: Some(worker),
        }
    }

    pub fn finish(&mut self) -> Result<(), aw_service::Error> {
        self.cancelled.store(true, Ordering::Release);
        let result = self.worker.take().unwrap().join().unwrap();
        assert!(!self.socket.exists());
        result
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let result = self.finish();
            if thread::panicking() {
                if let Err(error) = result {
                    eprintln!("service teardown: {error}");
                }
            } else {
                result.unwrap();
            }
        }
    }
}

pub fn event(binding: &Binding, name: &str, scenario: &str) -> Value {
    json!({
        "name": name,
        "agent": {"adapter": "qoder", "binding_id": "target", "instance_id": binding.instance_id},
        "session_id": "native-session",
        "tool": {"name": "arbitrary_tool", "native_name": "custom:工具", "call_id": "native-call",
            "input": {"private": PRIVATE_MARKER, "ratio": 0.125, "标签": "测试"},
            "result": if name == "tool.after" { json!({"private": PRIVATE_MARKER}) } else { Value::Null }},
        "native": {"scenario": scenario, "private": PRIVATE_MARKER}
    })
}

pub fn open(client: &Client, binding: &Binding, value: Value) -> EventHandle {
    serde_json::from_value(
        client
            .call(
                Operation::OpenEvent {
                    instance_id: binding.instance_id.clone(),
                    event: value,
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap(),
    )
    .unwrap()
}

pub fn invoke(handle: &EventHandle, step: &str) -> Operation {
    Operation::InvokeStep {
        event_id: handle.event_id.clone(),
        instance_id: handle.instance_id.clone(),
        step_id: step.into(),
    }
}

pub fn close(client: &Client, handle: &EventHandle) -> Value {
    client
        .call(
            Operation::CloseEvent {
                event_id: handle.event_id.clone(),
                instance_id: handle.instance_id.clone(),
            },
            Instant::now() + TIMEOUT,
        )
        .unwrap()
}

pub fn audit(client: &Client, key: &str) -> Value {
    client
        .call(
            Operation::Audit { key: key.into() },
            Instant::now() + TIMEOUT,
        )
        .unwrap()
}

pub fn second_step(document: &mut Value) {
    let mut step = document["spec"]["events"]["tool.before"]["steps"][0].clone();
    step["id"] = json!("second");
    step["operation"] = json!("second");
    document["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(step);
}
