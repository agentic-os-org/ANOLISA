//! Bounded Provider fixtures and task-owned files for real Host exchanges.

use aw_host::{Host, ProcessContext};
use aw_provider::admission::AdapterCapabilities;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
pub const TIMEOUT: Duration = Duration::from_secs(10);
pub const CLEANUP_ALLOWANCE: Duration = Duration::from_secs(2);
pub const LITERAL_ARGUMENT: &str = "literal $HOME ; $(printf unexpected)";

pub struct Fixture(pub PathBuf);

impl Fixture {
    pub fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/host-tests");
        fs::create_dir_all(&parent).unwrap();
        let path = parent.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    pub fn document(&self) -> Value {
        let python = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|directory| directory.join("python3"))
            .find(|candidate| candidate.is_file())
            .expect("Host integration tests require python3 on PATH");
        let python = fs::canonicalize(python).unwrap();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider.py");
        json!({
            "apiVersion": "aw/v1alpha1", "kind": "AWConfiguration",
            "metadata": {"name": "host-fixture"},
            "spec": {
                "daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": "auto"},
                "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                "audit": {"enabled": true, "payload": "metadata_only"},
                "agents": {"target": {"adapter": "qoder", "argv": ["/no-agent-is-started"]}},
                "providers": {"policy": {
                    "protocol": aw_provider::VERSION,
                    "transport": {
                        "type": "stdio", "location": "agent",
                        "argv": [python, "-I", script, self.0, "normal", LITERAL_ARGUMENT]
                    },
                    "timeout_ms": 5000, "max_output_bytes": 65536,
                    "config": {"threshold": 0.75, "标签": "配置", "delays_ms": {}}
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

    pub fn context(&self) -> ProcessContext {
        ProcessContext {
            cwd: self.0.clone(),
            environment: BTreeMap::from([
                ("LC_ALL".into(), "C".into()),
                ("AW_HOST_MARKER".into(), "retained-context".into()),
            ]),
            stderr_bytes: 65536,
        }
    }

    pub fn prepare(&self, document: &Value) -> Host {
        Host::prepare(
            &serde_json::to_vec(document).unwrap(),
            "target",
            capabilities(),
            self.context(),
            Instant::now() + TIMEOUT,
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    pub fn calls(&self, method: &str) -> Vec<Value> {
        fs::read_dir(&self.0)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .map(|entry| serde_json::from_slice::<Value>(&fs::read(entry.path()).unwrap()).unwrap())
            .filter(|entry| entry["request"]["method"] == method)
            .collect()
    }

    pub fn call(&self, request_id: &str) -> Value {
        serde_json::from_slice(&fs::read(self.0.join(format!("{request_id}.json"))).unwrap())
            .unwrap()
    }

    pub fn wait_for_calls(&self, method: &str, count: usize, deadline: Instant) -> bool {
        while Instant::now() < deadline {
            if self.calls(method).len() >= count {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }

    pub fn assert_reaped(&self) {
        let remaining = self.remaining_processes();
        assert!(
            remaining.is_empty(),
            "Host did not reap fixture PIDs {remaining:?}"
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
                    Err(error) => panic!("cannot inspect fixture PID {pid}: {error}"),
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
        // aw-exec owns termination and reaping. Inspect recorded start times;
        // never signal a numeric PID after the runner has released its identity.
        let remaining = self.remaining_processes();
        let removed = fs::remove_dir_all(&self.0);
        if thread::panicking() {
            if !remaining.is_empty() || removed.is_err() {
                eprintln!("Host fixture cleanup: PIDs {remaining:?}, directory {removed:?}");
            }
        } else {
            assert!(
                remaining.is_empty(),
                "fixture PIDs were not reaped: {remaining:?}"
            );
            removed.unwrap();
            assert!(!self.0.exists());
        }
    }
}

pub fn capabilities() -> AdapterCapabilities {
    AdapterCapabilities {
        adapter: "qoder".into(),
        version: "fixture-version".into(),
        entrypoint: "fixture-entrypoint".into(),
        events: BTreeMap::from([
            ("tool.before".into(), vec!["observe".into(), "block".into()]),
            ("tool.after".into(), vec!["observe".into()]),
        ]),
    }
}

pub fn event(name: &str, scenario: &str) -> Value {
    json!({
        "name": name,
        "agent": {"adapter": "qoder", "binding_id": "target", "instance_id": null},
        "session_id": null,
        "tool": {
            "name": "arbitrary_tool", "native_name": "custom:工具", "call_id": null,
            "input": {"用户": "Alice", "ratio": 0.125, "array": [false, null, 7]},
            "result": null
        },
        "native": {"scenario": scenario, "opaque": {"标签": "原样", "decimal": -1.75}}
    })
}

pub fn add_second_step(document: &mut Value) {
    let mut step = document["spec"]["events"]["tool.before"]["steps"][0].clone();
    step["id"] = json!("second");
    step["operation"] = json!("second");
    document["spec"]["events"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(step);
}
