//! Bounded mutable HTTP history, with teardown independent of test success.

use crate::common::{self, Fixture, TIMEOUT};
use aw_service::{Binding, Client, EventHandle, Operation};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const SECRET: &str = "private-evidence-bearer";
pub const HISTORY: &str = "private-source-history";

pub struct Source {
    pub endpoint: String,
    pub body: Arc<Mutex<Value>>,
    pub delay: Arc<Mutex<Duration>>,
    pub probe_delay: Arc<Mutex<Duration>>,
    pub auth_status: Arc<Mutex<u16>>,
    pub status: Arc<Mutex<u16>>,
    pub paths: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Source {
    pub fn new(_fixture: &Fixture, body: Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let body = Arc::new(Mutex::new(body));
        let delay = Arc::new(Mutex::new(Duration::ZERO));
        let status = Arc::new(Mutex::new(200));
        let probe_delay = Arc::new(Mutex::new(Duration::ZERO));
        let probe_lag = probe_delay.clone();
        let auth_status = Arc::new(Mutex::new(200));
        let auth_code = auth_status.clone();
        let paths = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (data, lag, code, captured, stopped) = (
            body.clone(),
            delay.clone(),
            status.clone(),
            paths.clone(),
            stop.clone(),
        );
        let worker = thread::spawn(move || {
            // Keep the source alive across service startup and two foreground runs.
            // This bounds fixture ownership, not the configured one-second query budget.
            let deadline = Instant::now() + Duration::from_secs(120);
            while Instant::now() < deadline && !stopped.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("source accept: {e}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
                    let mut byte = [0];
                    match stream.read(&mut byte) {
                        Ok(1) => request.push(byte[0]),
                        _ => break,
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(!request.to_lowercase().contains("authorization:"));
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                captured.lock().unwrap().push(path.clone());
                let (status, response, wait) = match path.as_str() {
                    "/api/auth/status" => (
                        *auth_code.lock().unwrap(),
                        json!({"auth_enabled":true}),
                        *probe_lag.lock().unwrap(),
                    ),
                    "/health" => panic!("the common query contract must not call /health"),
                    path if path.starts_with("/api/trajectories/") => (
                        *code.lock().unwrap(),
                        data.lock().unwrap().clone(),
                        *lag.lock().unwrap(),
                    ),
                    _ => panic!("unexpected source query: {path}"),
                };
                let until = Instant::now() + wait;
                while Instant::now() < until && Instant::now() < deadline {
                    if stopped.load(Ordering::Acquire) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                let body = response.to_string();
                let header = format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        Self {
            endpoint,
            body,
            delay,
            probe_delay,
            auth_status,
            status,
            paths,
            stop,
            worker: Some(worker),
        }
    }

    pub fn configure(&self, document: &mut Value) {
        document["spec"]["agents"]["target"]["evidence"] = json!({
            "source_id":"fixture", "endpoint":self.endpoint,
            "query_timeout_ms":1000, "query_bytes":1048576, "max_items":32, "max_bytes":262144,
        });
    }

    pub fn wait_for(&self, requests: usize) {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            if self.paths.lock().unwrap().len() >= requests {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("source query did not arrive");
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

pub fn trajectory(steps: usize) -> Value {
    json!({"schema_version":"ATIF-v1.7", "session_id":"native-session", "agent":{"name":"Qoder"},
        "steps":(1..=steps).map(|id| json!({"step_id":id,"source":"agent", "message":HISTORY,
            "timestamp":"2026-10-15T00:00:00Z", "observation":{"results":[{"source_call_id":"native-call", "content":{"ratio":0.125,"中文":HISTORY}}]},
            "is_copied_context":true})).collect::<Vec<_>>()})
}

pub fn native_event(binding: &Binding, name: &str) -> Value {
    let mut value = common::event(binding, name, "allow");
    value["tool"]["name"] = value["tool"]["native_name"].clone();
    value["native"] = json!({
        "hook_event_name":if name == "tool.after" { "PostToolUse" } else { "PreToolUse" },
        "session_id":value["session_id"], "tool_use_id":value["tool"]["call_id"],
        "tool_name":value["tool"]["native_name"], "tool_input":value["tool"]["input"],
        "tool_response":value["tool"]["result"], "scenario":"allow",
    });
    value
}

pub fn hook(client: &Client, binding: &Binding, value: Value) -> EventHandle {
    let input = serde_json::to_vec(&value["native"]).unwrap();
    serde_json::from_value(
        client
            .call(
                Operation::OpenHookEvent {
                    instance_id: binding.instance_id.clone(),
                    event: value,
                    native_input: input,
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap(),
    )
    .unwrap()
}

pub fn freeze(handle: &EventHandle) -> Operation {
    Operation::FreezeContext {
        event_id: handle.event_id.clone(),
        instance_id: handle.instance_id.clone(),
        candidate_revision: handle.candidate_revision.clone(),
    }
}

pub fn snapshot(client: &Client, handle: &EventHandle) -> Value {
    client
        .call(freeze(handle), Instant::now() + TIMEOUT)
        .unwrap()
}

pub fn gap(value: &Value, code: &str) -> bool {
    value["gaps"].as_array().unwrap().iter().any(|v| v == code)
}
