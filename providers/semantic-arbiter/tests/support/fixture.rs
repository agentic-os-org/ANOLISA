//! Credential-free real process and literal-loopback fixtures.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

pub const KEY_ENV: &str = "SEMANTIC_PROVIDER_SYNTHETIC_TEST_KEY";
pub const KEY: &str = "synthetic-only-not-a-real-credential";
pub const BINARY: &str = env!("CARGO_BIN_EXE_aw-provider-semantic-arbiter");

pub struct Fixture {
    pub root: TempDir,
    pub records: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let records = root.path().join("records");
        fs::create_dir(&records).unwrap();
        fs::set_permissions(&records, fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, records }
    }

    pub fn config(&self, endpoint: &str) -> Value {
        json!({
            "version":1,
            "task":{"session_id":"session/long+native", "version":"v1", "text":"Update the synthetic greeting only.",
                "allowed_resource_ids":["greeting"],"preserve_resource_ids":[]},
            "adapter":"qoder", "workspace_path":self.root.path(), "workspace_id":"synthetic-workspace",
            "tools":{"Write":{"operation":"replace","path_pointer":"/file_path"},
                "Edit":{"operation":"replace","path_pointer":"/file_path"}},
            "resources":{self.root.path().join("greeting.txt").to_str().unwrap():"greeting",
                self.root.path().join("private.txt").to_str().unwrap():"private"},
            "model":{"config_revision":"aw_request","backend":{"kind":"bailian","endpoint_url":endpoint,
                "model":"synthetic-model","key_env":KEY_ENV},"timeout_ms":2000,"max_response_bytes":4096,"transport":"loopback_test"},
            "record_dir":self.records,"max_records":20,"max_storage_bytes":83886080u64
        })
    }

    pub fn request(&self, config: Value) -> Value {
        json!({"api_version":"aw-provider/v1alpha1","request_id":"request/+native", "method":"invoke",
            "operation":"evaluate_task_scope", "config_revision":"a".repeat(64),"budget_ms":2000,
            "allowed_effects":["observe"], "config":config,
            "event":{"name":"tool.before","agent":{"adapter":"qoder","binding_id":"binding/+1", "instance_id":"instance/+1"},
                "session_id":"session/long+native", "tool":{"name":"write","native_name":"Write","call_id":"call/+1",
                "input":{"file_path":"greeting.txt","content":"synthetic full payload"},"result":null},
                "native":{"cwd":self.root.path(),"synthetic_extension":{"retained":true}}}})
    }

    pub fn records(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = fs::read_dir(&self.records)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        paths.sort();
        paths
    }
}

pub fn bind(request: Value) -> Value {
    aw_provider::Protocol::new()
        .unwrap()
        .bind_invocation(request)
        .unwrap()
        .as_value()
        .clone()
}

pub fn spawn(cwd: &Path, key: bool) -> Child {
    let mut command = Command::new(BINARY);
    command
        .current_dir(cwd)
        .env_remove(KEY_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if key {
        command.env(KEY_ENV, KEY);
    }
    command.spawn().unwrap()
}

pub fn finish(mut child: Child, bytes: &[u8]) -> Output {
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("provider exceeded fixture deadline");
        }
        thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

pub fn invoke(fixture: &Fixture, request: Value, key: bool) -> (Value, Value) {
    let request = bind(request);
    let output = finish(
        spawn(fixture.root.path(), key),
        &serde_json::to_vec(&request).unwrap(),
    );
    let protocol = aw_provider::Protocol::new().unwrap();
    let checked = protocol
        .parse_request(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    protocol.check_response(&checked, &output.stdout).unwrap();
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["effects"], json!([]));
    assert_eq!(response["input_digest"], request["input_digest"]);
    assert!(!String::from_utf8_lossy(&output.stderr).contains(KEY));
    (response, serde_json::from_slice(&output.stderr).unwrap())
}

pub fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

pub struct Server {
    pub endpoint: String,
    pub received: mpsc::Receiver<Value>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn new(probability: &str, status: u16, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/synthetic", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let (sender, received) = mpsc::channel();
        let probability = probability.to_owned();
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let end = loop {
                    let n = stream.read(&mut buffer).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                    assert!(bytes.len() < 1_048_576);
                };
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                while bytes.len() < end + length {
                    let n = stream.read(&mut buffer).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&buffer[..n]);
                }
                let body: Value = serde_json::from_slice(&bytes[end..end + length]).unwrap();
                let probability: Value = serde_json::from_str(&probability).unwrap();
                let answers: serde_json::Map<String, Value> = body["questions"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(|key| (key.clone(), json!({"type":"noul","noul":probability})))
                    .collect();
                sender.send(body).unwrap();
                let body = if status == 200 {
                    json!({"answers":answers}).to_string()
                } else {
                    "private-remote-error-body".into()
                };
                thread::sleep(delay);
                let reply = format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Self {
            endpoint,
            received,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
