#![allow(dead_code)] // Shared by independent contract and process test targets.

use std::os::unix::fs::PermissionsExt as _;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

pub const METHODS: &str =
    include_str!("../../../../crates/asc-daemon-protocol/tests/fixtures/pap-methods.json");
pub const SCENARIO: &str =
    include_str!("../../../../crates/asc-daemon-protocol/tests/fixtures/pap-crud-e2e.json");

pub struct Directory(pub PathBuf);
impl Directory {
    /// Creates a unique directory short enough to hold a bindable socket path.
    ///
    /// Sockets in these tests live inside this directory, and `sun_path` caps a
    /// socket address at 104 bytes on macOS against 108 on Linux. `TMPDIR` there
    /// expands to a ~49-byte `/var/folders/...` path, which overruns the cap once
    /// a UUID-named subdirectory and file name are appended. Rooting at `/tmp`
    /// keeps the total near 60 bytes on both systems, so the length no longer
    /// depends on how long the caller's `TMPDIR` happens to be.
    pub fn new() -> Self {
        let base = Path::new("/tmp");
        let base = if base.is_dir() {
            base.to_path_buf()
        } else {
            std::env::temp_dir()
        };
        let path = base.join(format!("asc-cli-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn args_for(request: &Value, directory: &Path, socket: &Path) -> Vec<OsString> {
    let method = request["method"].as_str().unwrap();
    let parts: Vec<_> = method.split('.').collect();
    let resource = match parts[1] {
        "templates" => "policy",
        "scopes" => "scope",
        "bindings" => "binding",
        _ => panic!("unknown fixture method"),
    };
    let mut args: Vec<OsString> = ["--socket", socket.to_str().unwrap(), resource, parts[2]]
        .into_iter()
        .map(Into::into)
        .collect();
    for (field, value) in request["params"].as_object().unwrap() {
        match field.as_str() {
            "template" => {
                let path = directory.join("template with spaces.json");
                std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
                args.extend([OsString::from("--file"), path.into_os_string()]);
            }
            "selector" => {
                let (option, number) = if value["kind"] == "pid" {
                    ("--pid", &value["pid"])
                } else {
                    ("--cgroup-id", &value["cgroupId"])
                };
                args.extend([option.into(), number.to_string().into()]);
            }
            _ => {
                let option = match field.as_str() {
                    "policyName" => "--name",
                    "policyId" => "--policy-id",
                    "policyRevision" => "--policy-revision",
                    "scopeId" => "--scope-id",
                    "scopeRevision" => "--scope-revision",
                    "bindingId" => "--binding-id",
                    "revision" => "--revision",
                    "limit" => "--limit",
                    "offset" => "--offset",
                    "id" => match resource {
                        "policy" => "--policy-id",
                        "scope" => "--scope-id",
                        _ => "--binding-id",
                    },
                    _ => panic!("unknown fixture field {field}"),
                };
                let value = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                args.extend([option.into(), value.into()]);
            }
        }
    }
    args
}

pub fn expand(value: &Value, objects: &Value, variables: &BTreeMap<String, Value>) -> Value {
    match value {
        Value::Object(map) if map.len() == 1 && map.contains_key("$ref") => {
            expand(&objects[map["$ref"].as_str().unwrap()], objects, variables)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), expand(value, objects, variables)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| expand(value, objects, variables))
                .collect(),
        ),
        Value::String(text) if text.starts_with("${") && text.ends_with('}') => {
            variables[&text[2..text.len() - 1]].clone()
        }
        _ => value.clone(),
    }
}

pub fn run(args: &[OsString]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-sec-cli"))
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Drain both pipes concurrently so large snapshots cannot block child exit.
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let read = |mut stream: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = read(Box::new(stdout));
    let stderr = read(Box::new(stderr));
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("CLI process exceeded test deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}
