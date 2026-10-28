//! Public Host fixtures and bounded loopback inference with a timed overlap gate.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Barrier, Condvar, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use aw_host::{Host, ProcessContext, StepOutput};
use aw_provider::admission::AdapterCapabilities;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::fixture::{read, Fixture, BINARY, KEY, KEY_ENV};

/// Retain the shared sessionless config with enough budget for a timed overlap wave.
pub fn config(fixture: &Fixture, endpoint: &str) -> Value {
    let mut config = fixture.config(endpoint);
    assert_eq!(config["version"], 1);
    assert!(config["task"].get("session_id").is_none());
    config["model"]["timeout_ms"] = json!(6000);
    config
}

/// Prepare the actual binary against a complete immutable AW configuration.
pub fn prepare(fixture: &Fixture, config: &Value, target: &str, name: &str) -> Host {
    let adapter = config["adapter"].as_str().unwrap();
    let document = json!({
        "apiVersion":"aw/v1alpha1","kind":"AWConfiguration","metadata":{"name":name},
        "spec":{
            "daemon":{"startup":"on_demand","endpoint":"auto","state_dir":"auto"},
            "execution":{"guarantee":"native_hook","default_event_budget_ms":8000},
            "audit":{"enabled":true,"payload":"metadata_only"},
            "agents":{target:{"adapter":adapter,"argv":["/no-agent-is-started"]}},
            "providers":{"semantic":{"protocol":"aw-provider/v1alpha1",
                "transport":{"type":"stdio","location":"agent","argv":[BINARY]},
                "timeout_ms":8000,"max_output_bytes":65536,"config":config}},
            "events":{"tool.before":{"enabled":true,"steps":[{"id":"shadow",
                "provider":"semantic","operation":"evaluate_task_scope",
                "effects":["observe"],"on_error":"report"}]}}
        }
    });
    let cancelled = AtomicBool::new(false);
    let host = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        target,
        AdapterCapabilities {
            adapter: adapter.into(),
            version: "fixture".into(),
            entrypoint: "fixture".into(),
            failure_actions: Default::default(),
            events: BTreeMap::from([("tool.before".into(), vec!["observe".into()])]),
        },
        ProcessContext {
            cwd: fixture.root.path().to_path_buf(),
            environment: BTreeMap::from([(KEY_ENV.into(), KEY.into())]),
            stderr_bytes: 4096,
        },
        Instant::now() + Duration::from_secs(10),
        &cancelled,
    )
    .unwrap();
    assert_eq!(host.preparation().len(), 2);
    host
}

/// Preserve the complete fixture candidate while changing only its native context.
pub fn event(
    fixture: &Fixture,
    config: &Value,
    target: &str,
    instance: &str,
    session: &str,
) -> Value {
    let mut event = fixture.request(config.clone())["event"].clone();
    event["agent"]["adapter"] = config["adapter"].clone();
    event["agent"]["binding_id"] = json!(target);
    event["agent"]["instance_id"] = json!(instance);
    event["session_id"] = json!(session);
    event
}

/// Require an observe-only public response and return its sanitized diagnostic code.
pub fn invoke_host(host: &Host, event: &Value) -> String {
    let cancelled = AtomicBool::new(false);
    let selected = host
        .event(
            event.clone(),
            Instant::now() + Duration::from_secs(8),
            &cancelled,
        )
        .unwrap();
    let invocation = selected.invoke("shadow").unwrap();
    let stderr = &invocation.record.process.as_ref().unwrap().stderr;
    assert!(!String::from_utf8_lossy(stderr).contains(KEY));
    let diagnostic: Value = serde_json::from_slice(stderr).unwrap();
    let StepOutput::Provider(outcome) = invocation.result.unwrap() else {
        panic!("expected Provider output");
    };
    assert!(!outcome.requests_block());
    assert_eq!(outcome.as_value()["status"], "ok");
    assert_eq!(outcome.as_value()["effects"], json!([]));
    diagnostic["code"].as_str().unwrap().to_owned()
}

/// Start all native callbacks together and collect every actual model evaluation.
pub fn concurrent(jobs: &[(&Host, &Value)]) {
    let start = Barrier::new(jobs.len());
    thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|&(host, event)| {
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    invoke_host(host, event)
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), "shadow_evaluated");
        }
    });
}

fn identity(domain: &str, value: &Value) -> String {
    let mut hash = Sha256::new();
    hash.update(b"aw-provider-semantic-arbiter/v1\0");
    hash.update(domain.as_bytes());
    hash.update(b"\0");
    hash.update(serde_json::to_vec(value).unwrap());
    format!("sha256:{:x}", hash.finalize())
}

/// Derive the expected opaque session from all four native context components.
pub fn session(event: &Value) -> String {
    identity(
        "session",
        &json!([
            event["agent"]["adapter"],
            event["agent"]["binding_id"],
            event["agent"]["instance_id"],
            event["session_id"]
        ]),
    )
}

/// Locate exactly one accepted ingress without depending on directory name order.
pub fn record(fixture: &Fixture, event: &Value) -> PathBuf {
    let matching: Vec<_> = fixture
        .records()
        .into_iter()
        .filter(|dir| read(&dir.join("00-input.json"))["aw_request"]["event"] == *event)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "each event needs exactly one private record"
    );
    matching[0].clone()
}

/// Check every private file for the synthetic key and for accidental tool execution.
pub fn no_secrets(fixture: &Fixture) {
    for dir in fixture.records() {
        for entry in fs::read_dir(dir).unwrap() {
            let text = fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(!text.contains(KEY));
        }
    }
    assert!(!fixture.root.path().join("greeting.txt").exists());
}

/// Check the complete task/context projection and every successful inference stage.
pub fn evaluated(fixture: &Fixture, event: &Value, config: &Value, body: &Value) -> Value {
    let dir = record(fixture, event);
    let input = read(&dir.join("00-input.json"));
    assert_eq!(input["filesystem_attested"], false);
    assert_eq!(input["aw_request"]["config"], *config);
    let evaluation = read(&dir.join("10-evaluation.json"));
    let binding = &evaluation["binding"];
    assert_eq!(binding["session_id"], session(event));
    assert_eq!(
        binding["instance_id"],
        identity(
            "instance",
            &json!([
                event["agent"]["adapter"],
                event["agent"]["binding_id"],
                event["agent"]["instance_id"]
            ])
        )
    );
    assert_eq!(binding["workspace_id"], config["workspace_id"]);
    assert_eq!(binding["task_version"], config["task"]["version"]);
    assert_eq!(
        binding["config_revision"],
        identity(
            "aw-config-revision",
            &input["aw_request"]["config_revision"]
        )
    );
    assert_eq!(
        binding["candidate_id"],
        identity(
            "candidate",
            &json!([
                event["agent"],
                event["session_id"],
                event["tool"]["call_id"]
            ])
        )
    );
    let task = json!({
        "session_id":session(event),"version":config["task"]["version"],"origin":"operator",
        "text":config["task"]["text"],"allowed_resource_ids":config["task"]["allowed_resource_ids"],
        "preserve_resource_ids":config["task"]["preserve_resource_ids"]
    });
    assert_eq!(evaluation["task"], task);
    assert_eq!(evaluation["task"]["session_id"], binding["session_id"]);
    let text = config["task"]["text"].as_str().unwrap();
    let digest = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
    assert_eq!(
        read(&dir.join("05-task.json")),
        json!({
            "version":1,"origin":"operator","native_session_id":event["session_id"],
            "session_id":binding["session_id"],"adapter":event["agent"]["adapter"],
            "binding_id":event["agent"]["binding_id"],"instance_id":event["agent"]["instance_id"],
            "config_revision":binding["config_revision"],"task_version":config["task"]["version"],
            "source":config["task"]["provenance"]["source"],
            "captured_by":config["task"]["provenance"]["captured_by"],
            "captured_at_ms":config["task"]["provenance"]["captured_at_ms"],
            "declared_text_sha256":config["task"]["provenance"]["text_sha256"],
            "evaluated_text_sha256":digest,"text_bytes":text.len()
        })
    );
    let candidate = json!({"operation":"replace","resource_id":"greeting",
        "native_tool":{"name":"Write","input":event["tool"]["input"]}});
    assert_eq!(evaluation["candidate"], candidate);
    let typed =
        serde_json::from_value::<semantic_arbiter::contract::Candidate>(candidate.clone()).unwrap();
    assert_eq!(
        binding["candidate_revision"],
        semantic_arbiter::contract::candidate_revision(&typed).unwrap()
    );
    assert_eq!(
        body["state"],
        json!({"authoritative_task":task,"candidate":candidate,"evidence":[]})
    );
    assert!(!body["questions"].as_object().unwrap().is_empty());
    let projection = read(&dir.join("20-model-request.json"));
    assert_eq!(projection["state"], body["state"]);
    assert_eq!(projection["questions"], body["questions"]);
    assert_eq!(projection["backend"]["mode"], "loopback_test");
    assert_eq!(read(&dir.join("30-model-result.json"))["status"], "parsed");
    let result = read(&dir.join("40-result.json"));
    assert_eq!(result["status"], "evaluated");
    assert_eq!(result["code"], "shadow_evaluated");
    assert_eq!(result["evaluation"]["binding"], *binding);
    assert_eq!(
        result["evaluation"]["outcome"]["control"]["candidate"],
        "allow"
    );
    assert_eq!(result["evaluation"]["model_called"], true);
    let judgments = result["evaluation"]["judgments"].as_array().unwrap();
    assert!(!judgments.is_empty());
    for judgment in judgments {
        assert_eq!(judgment["model_used"], true);
        assert!(judgment["error"].is_null());
    }
    let mut files: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "00-input.json",
            "05-task.json",
            "10-evaluation.json",
            "20-model-request.json",
            "30-model-result.json",
            "40-result.json"
        ]
        .map(std::ffi::OsString::from)
    );
    no_secrets(fixture);
    evaluation
}

const WAIT: Duration = Duration::from_secs(3);
const BODY_LIMIT: usize = 1_048_576;

#[derive(Default)]
struct State {
    accepted: usize,
    requests: Vec<Value>,
    active: usize,
    max_overlap: usize,
    responses: usize,
    before_first_response: Option<usize>,
    released: bool,
    errors: Vec<String>,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

/// Require simultaneous inference arrivals before allowing any response.
pub struct ParallelServer {
    /// Literal-loopback endpoint; no external network or credentials are needed.
    pub endpoint: String,
    expected: usize,
    overlap: usize,
    shared: Shared,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ParallelServer {
    /// Bound accepted connections and require an initial wave of `overlap` requests.
    pub fn new(expected: usize, overlap: usize) -> Self {
        assert!((1..=16).contains(&expected));
        assert!((1..=expected).contains(&overlap));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/synthetic", listener.local_addr().unwrap());
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let state = shared.clone();
        let stopping = stop.clone();
        let thread = thread::spawn(move || {
            let mut workers = Vec::new();
            while !stopping.load(Ordering::SeqCst) {
                let (stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => {
                        state.0.lock().unwrap().errors.push(error.to_string());
                        break;
                    }
                };
                let mut current = state.0.lock().unwrap();
                current.accepted += 1;
                if current.accepted > expected {
                    current.errors.push("unexpected extra connection".into());
                    break;
                }
                drop(current);
                let shared = state.clone();
                let stop = stopping.clone();
                workers.push(thread::spawn(move || {
                    if let Err(error) = respond(stream, &shared, &stop, overlap) {
                        shared.0.lock().unwrap().errors.push(error.to_string());
                        shared.1.notify_all();
                    }
                }));
            }
            for worker in workers {
                if worker.join().is_err() {
                    state
                        .0
                        .lock()
                        .unwrap()
                        .errors
                        .push("connection worker panicked".into());
                }
            }
        });
        Self {
            endpoint,
            expected,
            overlap,
            shared,
            stop,
            thread: Some(thread),
        }
    }

    /// Collect completed requests and reject timeout-based or sequential pseudo-overlap.
    pub fn finish(&self) -> Vec<Value> {
        let deadline = Instant::now() + WAIT;
        let mut state = self.shared.0.lock().unwrap();
        while state.responses < self.expected && state.errors.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "inference responses did not complete");
            state = self.shared.1.wait_timeout(state, remaining).unwrap().0;
        }
        assert!(state.errors.is_empty(), "{:?}", state.errors);
        assert_eq!(state.accepted, self.expected);
        assert_eq!(state.requests.len(), self.expected);
        assert_eq!(state.responses, self.expected);
        assert_eq!(state.active, 0);
        assert!(state.max_overlap >= self.overlap);
        assert!(state.before_first_response.unwrap() >= self.overlap);
        state.requests.clone()
    }
}

impl Drop for ParallelServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.shared.1.notify_all();
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !thread::panicking() {
                result.unwrap();
            }
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_chunk(stream: &mut TcpStream, bytes: &mut Vec<u8>, deadline: Instant) -> io::Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    stream.set_read_timeout(Some(remaining))?;
    let mut buffer = [0; 4096];
    let length = stream.read(&mut buffer)?;
    if length == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    bytes.extend_from_slice(&buffer[..length]);
    if bytes.len() > BODY_LIMIT + 65_536 {
        return Err(invalid("request exceeds fixture byte limit"));
    }
    Ok(())
}

fn request(stream: &mut TcpStream) -> io::Result<Value> {
    let deadline = Instant::now() + WAIT;
    let mut bytes = Vec::new();
    let end = loop {
        read_chunk(stream, &mut bytes, deadline)?;
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        if bytes.len() > 65_536 {
            return Err(invalid("request headers exceed fixture limit"));
        }
    };
    let headers = std::str::from_utf8(&bytes[..end]).map_err(|_| invalid("non-UTF-8 headers"))?;
    let length = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
        })
        .ok_or_else(|| invalid("missing content length"))?
        .parse::<usize>()
        .map_err(|_| invalid("invalid content length"))?;
    if length > BODY_LIMIT {
        return Err(invalid("request body exceeds fixture limit"));
    }
    while bytes.len() < end + length {
        read_chunk(stream, &mut bytes, deadline)?;
    }
    serde_json::from_slice(&bytes[end..end + length]).map_err(|_| invalid("invalid JSON body"))
}

fn respond(
    mut stream: TcpStream,
    shared: &Shared,
    stop: &AtomicBool,
    overlap: usize,
) -> io::Result<()> {
    let body = request(&mut stream)?;
    let questions = body["questions"]
        .as_object()
        .ok_or_else(|| invalid("missing questions"))?;
    if questions.is_empty() {
        return Err(invalid("empty inference questions"));
    }
    let answers: serde_json::Map<String, Value> = questions
        .keys()
        .map(|key| (key.clone(), json!({"type":"noul","noul":0.99})))
        .collect();
    let reply = json!({"answers":answers}).to_string();
    let response = format!(
        "HTTP/1.1 200 Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let deadline = Instant::now() + WAIT;
    let mut state = shared.0.lock().unwrap();
    state.requests.push(body);
    state.active += 1;
    state.max_overlap = state.max_overlap.max(state.active);
    if state.requests.len() >= overlap {
        state.released = true;
        shared.1.notify_all();
    }
    while !state.released && !stop.load(Ordering::SeqCst) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            state.active -= 1;
            return Err(invalid("concurrent inference arrival barrier timed out"));
        }
        state = shared.1.wait_timeout(state, remaining).unwrap().0;
    }
    if !state.released {
        state.active -= 1;
        return Err(io::ErrorKind::Interrupted.into());
    }
    if state.before_first_response.is_none() {
        state.before_first_response = Some(state.requests.len());
    }
    drop(state);
    stream.set_write_timeout(Some(WAIT))?;
    let written = stream.write_all(response.as_bytes());
    let mut state = shared.0.lock().unwrap();
    state.active -= 1;
    if written.is_ok() {
        state.responses += 1;
    }
    shared.1.notify_all();
    written
}
