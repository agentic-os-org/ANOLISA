//! Real CLI evaluation against deadline-bounded loopback HTTP fixtures only.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::evaluation::{input_digest, EvaluationRequest, EVALUATION_API_VERSION};
use serde_json::{json, Value};

const PRESERVATION: &str = "preservation-v1";
const TASK_SCOPE: &str = "task-scope-v2";
const KEY_ENV: &str = "SEMANTIC_ARBITER_EVALUATION_CLI_SYNTHETIC_KEY_D7630C4B";
const SYNTHETIC_KEY: &str = "synthetic-cli-only-not-a-real-key-d7630c4b";
const TASK_MARKER: &str = "PRIVATE_SYNTHETIC_TASK_d7630c4b: replace only the generated result.";
const BODY_MARKER: &str = "PRIVATE_SYNTHETIC_UPSTREAM_BODY_d7630c4b";
static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

struct TemporaryFiles {
    directory: PathBuf,
    files: Vec<PathBuf>,
}

impl TemporaryFiles {
    fn new() -> Self {
        let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::SeqCst);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "semantic-arbiter-private-cli-{}-{nonce}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        Self {
            directory,
            files: Vec::new(),
        }
    }

    fn write(&mut self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.directory.join(name);
        std::fs::write(&path, bytes).unwrap();
        self.files.push(path.clone());
        path
    }

    fn json(&mut self, name: &str, value: &Value) -> PathBuf {
        self.write(name, &serde_json::to_vec(value).unwrap())
    }
}

impl Drop for TemporaryFiles {
    fn drop(&mut self) {
        for path in &self.files {
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_dir(&self.directory);
    }
}

// Deliberately no Debug implementation: assertion failures must not print headers.
struct CapturedRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

fn read_request(stream: &mut TcpStream) -> Result<CapturedRequest, &'static str> {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|_| "read timeout setup failed")?;
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut bytes = Vec::new();
    let mut header_end = None;
    let mut body_length = None;
    loop {
        if Instant::now() >= deadline {
            return Err("request read deadline exceeded");
        }
        let mut chunk = [0; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => return Err("request ended before complete body"),
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(_) => return Err("request read failed"),
        }
        if bytes.len() > 262144 {
            return Err("request fixture limit exceeded");
        }
        if header_end.is_none() {
            if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let end = index + 4;
                let headers =
                    std::str::from_utf8(&bytes[..index]).map_err(|_| "invalid headers")?;
                for line in headers.lines().skip(1) {
                    let (name, value) = line.split_once(':').ok_or("invalid header line")?;
                    if name.eq_ignore_ascii_case("content-length") {
                        if body_length.is_some() {
                            return Err("duplicate content length");
                        }
                        body_length = Some(
                            value
                                .trim()
                                .parse::<usize>()
                                .map_err(|_| "invalid content length")?,
                        );
                    }
                }
                header_end = Some(end);
                if body_length.is_none() {
                    return Err("missing content length");
                }
            }
        }
        if let (Some(end), Some(length)) = (header_end, body_length) {
            if length > 262144 {
                return Err("body fixture limit exceeded");
            }
            if bytes.len() >= end + length {
                let header_text =
                    std::str::from_utf8(&bytes[..end - 4]).map_err(|_| "invalid headers")?;
                let mut lines = header_text.lines();
                let mut first = lines
                    .next()
                    .ok_or("missing request line")?
                    .split_whitespace();
                let method = first.next().ok_or("missing method")?.to_owned();
                let path = first.next().ok_or("missing path")?.to_owned();
                let mut headers = BTreeMap::new();
                for line in lines {
                    let (name, value) = line.split_once(':').ok_or("invalid header line")?;
                    headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
                }
                let body = serde_json::from_slice(&bytes[end..end + length])
                    .map_err(|_| "invalid request body")?;
                return Ok(CapturedRequest {
                    method,
                    path,
                    headers,
                    body,
                });
            }
        }
    }
}

struct Server {
    base_url: String,
    received: mpsc::Receiver<Result<CapturedRequest, &'static str>>,
    calls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    fn new(status: u16, response_body: Vec<u8>) -> Self {
        Self::delayed(status, response_body, Duration::ZERO)
    }

    fn delayed(status: u16, response_body: Vec<u8>, response_delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let (sender, received) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_stop = stop.clone();
        let worker_calls = calls.clone();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !worker_stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        worker_calls.fetch_add(1, Ordering::SeqCst);
                        let captured = read_request(&mut stream);
                        let valid = captured.is_ok();
                        let _ = sender.send(captured);
                        if valid {
                            thread::sleep(response_delay);
                            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                            let headers = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response_body.len());
                            let _ = stream.write_all(headers.as_bytes());
                            let _ = stream.write_all(&response_body);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => {
                        let _ = sender.send(Err("accept failed"));
                        break;
                    }
                }
            }
        });
        Self {
            base_url,
            received,
            calls,
            stop,
            worker: Some(worker),
        }
    }

    fn receive(&self) -> CapturedRequest {
        self.received
            .recv_timeout(Duration::from_secs(5))
            .expect("CLI did not send a complete request")
            .expect("invalid captured request")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn request() -> Value {
    let mut request: Value =
        serde_json::from_slice(include_bytes!("../examples/preservation-allow.json")).unwrap();
    request.as_object_mut().unwrap().remove("check_at_ms");
    request["api_version"] = json!(EVALUATION_API_VERSION);
    request["mode"] = json!("model_evaluation");
    request["scenarios"] = json!(["preservation", "task_scope"]);
    request["task"]["text"] = json!(TASK_MARKER);
    let now = Ingress::capture().unwrap().now_ms();
    request["budget"]["opened_at_ms"] = json!(now - 1000);
    request["budget"]["deadline_ms"] = json!(now + 10000);
    request["evidence"][0]["captured_at_ms"] = json!(now - 500);
    request["evidence"][0]["valid_until_ms"] = json!(now + 20000);
    request
}

fn config(kind: &str, base_url: &str) -> Value {
    let backend = if kind == "bailian" {
        json!({"kind": kind, "endpoint_url": format!("{base_url}/custom/decision/suffix"), "model": "cli-test-model", "key_env": KEY_ENV})
    } else {
        json!({"kind": kind, "base_url": format!("{base_url}/gateway/"), "model": "cli-test-model", "key_env": KEY_ENV})
    };
    json!({
        "config_revision": "policy-v1", "backend": backend,
        "timeout_ms": 10000, "max_response_bytes": 8192,
        "thresholds": {"preservation_ppm": 900000, "task_scope_ppm": 900000}, "transport": "loopback_test"
    })
}

fn binary() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_semantic-arbiter"));
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Never inspect or modify the parent environment, or use a standard key name.
    command.env(KEY_ENV, SYNTHETIC_KEY);
    command
}

fn evaluate_command(config: &Path, input: &OsStr) -> Command {
    let mut command = binary();
    command
        .args(["evaluate", "--config"])
        .arg(config)
        .arg(input);
    command
}

fn wait_bounded(mut child: Child, cap: Duration) -> Output {
    let end = Instant::now() + cap;
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait_with_output();
            panic!("CLI failed to exit within the test deadline");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn run(mut command: Command, bytes: Option<&[u8]>) -> Output {
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if let Some(bytes) = bytes {
        if let Err(error) = stdin.write_all(bytes) {
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
    }
    drop(stdin);
    wait_bounded(child, Duration::from_secs(6))
}

fn assert_private(output: &Output, files: &TemporaryFiles) {
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        for forbidden in [
            SYNTHETIC_KEY,
            KEY_ENV,
            TASK_MARKER,
            BODY_MARKER,
            files.directory.to_str().unwrap(),
        ] {
            assert!(
                !text.contains(forbidden),
                "CLI output exposed private fixture data"
            );
        }
    }
}

fn assert_error(output: &Output, code: &str, files: &TemporaryFiles) {
    assert_private(output, files);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap(),
        json!({"api_version": EVALUATION_API_VERSION, "error": code})
    );
}

fn assert_capture(captured: CapturedRequest, kind: &str, request: &Value) {
    assert_eq!(captured.method, "POST");
    assert_eq!(
        captured.path,
        if kind == "bailian" {
            "/custom/decision/suffix"
        } else {
            "/gateway/v1/systemone"
        }
    );
    assert!(
        captured
            .headers
            .get("authorization")
            .is_some_and(|value| value == &format!("Bearer {SYNTHETIC_KEY}")),
        "missing or incorrect synthetic authorization"
    );
    assert_eq!(
        captured.headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
    assert_eq!(
        captured.headers.get("accept").map(String::as_str),
        Some("application/json")
    );
    assert_eq!(captured.body.as_object().unwrap().len(), 3);
    assert_eq!(captured.body["model"], "cli-test-model");
    assert_eq!(
        captured.body["state"],
        json!({"authoritative_task": request["task"], "candidate": request["candidate"], "evidence": request["evidence"]})
    );
    let questions = captured.body["questions"].as_object().unwrap();
    assert_eq!(
        questions.keys().map(String::as_str).collect::<Vec<_>>(),
        [PRESERVATION, TASK_SCOPE]
    );
    for question in questions.values() {
        assert_eq!(question.as_object().unwrap().len(), 2);
        assert_eq!(question["type"], "noul");
        let instructions = question["instructions"].as_str().unwrap();
        assert!(instructions.contains("authoritative_task.text"));
        assert!(!instructions.contains(TASK_MARKER));
    }
}

fn provider_reply(preservation: f64, task_scope: f64) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "answers": {PRESERVATION: {"type": "noul", "noul": preservation}, TASK_SCOPE: {"type": "noul", "noul": task_scope}},
        "model": "untrusted-server-model-claim", "request_id": "untrusted-server-request-id",
        "usage": {"ignored": BODY_MARKER}
    }))
    .unwrap()
}

#[test]
fn both_http_adapters_emit_golden_advisory_allow_preserve_snapshot_scope_and_unknown_results() {
    for kind in ["bailian", "typesafe"] {
        for (case, preservation, task_scope, unique, values, reasons, control) in [
            (
                "allow",
                0.0,
                1.0,
                false,
                ["allow", "allow"],
                ["allowed", "allowed"],
                "allow",
            ),
            (
                "preserve",
                1.0,
                1.0,
                false,
                ["preserve", "allow"],
                ["preserved", "allowed"],
                "block",
            ),
            (
                "snapshot",
                0.0,
                1.0,
                true,
                ["needs_snapshot", "allow"],
                ["needs_snapshot", "allowed"],
                "allow",
            ),
            (
                "outside",
                0.0,
                0.0,
                false,
                ["allow", "outside_scope"],
                ["allowed", "outside_scope"],
                "block",
            ),
            (
                "unknown",
                0.5,
                0.5,
                false,
                ["unknown", "unknown"],
                ["undetermined", "undetermined"],
                "nop",
            ),
        ] {
            let server = Server::new(200, provider_reply(preservation, task_scope));
            let mut files = TemporaryFiles::new();
            let config_path = files.json("config.json", &config(kind, &server.base_url));
            let mut request = request();
            if unique {
                request["evidence"][0]["fact"]["parameters"]["result"] = json!("unique");
            }
            let request_path = files.json("request.json", &request);
            let started = Ingress::capture().unwrap().now_ms();
            let output = if case == "snapshot" {
                run(
                    evaluate_command(&config_path, request_path.as_os_str()),
                    None,
                )
            } else {
                run(
                    evaluate_command(&config_path, OsStr::new("-")),
                    Some(&serde_json::to_vec(&request).unwrap()),
                )
            };
            assert_private(&output, &files);
            assert_eq!(output.status.code(), Some(0), "{kind}/{case}");
            assert!(output.stderr.is_empty());
            assert_eq!(output.stdout.last(), Some(&b'\n'));
            let mut actual: Value = serde_json::from_slice(&output.stdout).unwrap();
            let typed_request: EvaluationRequest = serde_json::from_value(request.clone()).unwrap();
            assert_eq!(
                actual["input_digest"],
                input_digest(&typed_request).unwrap()
            );
            let digest = actual["config_digest"].as_str().unwrap();
            assert!(digest.starts_with("sha256:") && digest.len() == 71);
            assert!(digest[7..].bytes().all(|byte| byte.is_ascii_hexdigit()));
            let evaluated = actual["evaluated_at_ms"].as_u64().unwrap();
            assert!(evaluated >= started);
            assert!(evaluated <= Ingress::capture().unwrap().now_ms());
            assert!(actual["elapsed_ms"].as_u64().unwrap() < 10000);
            for key in [
                "input_digest",
                "config_digest",
                "evaluated_at_ms",
                "elapsed_ms",
            ] {
                actual.as_object_mut().unwrap().remove(key);
            }
            let probability = |p: f64| json!({"raw": p, "yes_ppm": (p * 1000000.0) as u32, "no_ppm": ((1.0 - p) * 1000000.0) as u32});
            let confidence = if case == "unknown" { 0 } else { 1000000 };
            let mut obligations = Vec::new();
            if unique {
                obligations.push(json!({"id": "snapshot", "critical": true, "action": {"kind": "snapshot", "parameters": {"workspace_id": request["binding"]["workspace_id"]}}}));
            }
            obligations.push(json!({"id": "audit", "critical": true, "action": {"kind": "audit", "parameters": {}}}));
            assert_eq!(
                actual,
                json!({
                    "api_version": EVALUATION_API_VERSION, "request_id": request["request_id"], "mode": "model_evaluation",
                    "binding": request["binding"], "backend": {"kind": kind, "mode": "loopback_test", "model": "cli-test-model"},
                    "normalization_revision": "noul-floor-ppm/v2", "model_called": true,
                    "judgments": [
                        {"unit": "preservation", "unit_revision": "semantic-units/v1", "question_id": PRESERVATION, "threshold_ppm": 900000, "probability": probability(preservation), "error": null, "model_used": true, "evidence_refs": ["rebuild-1"]},
                        {"unit": "task_scope", "unit_revision": "semantic-task-scope/v2", "question_id": TASK_SCOPE, "threshold_ppm": 900000, "probability": probability(task_scope), "error": null, "model_used": true, "evidence_refs": []}
                    ],
                    "answers": [
                        {"unit": "preservation", "value": values[0], "confidence_ppm": confidence, "reason": reasons[0], "evidence_refs": ["rebuild-1"]},
                        {"unit": "task_scope", "value": values[1], "confidence_ppm": confidence, "reason": reasons[1], "evidence_refs": []}
                    ],
                    "outcome": {"control": {"candidate": control, "subject": []}, "obligations": obligations, "evidence_refs": ["rebuild-1"]}
                }),
                "{kind}/{case}"
            );
            assert_capture(server.receive(), kind, &request);
            assert_eq!(
                server.calls.load(Ordering::SeqCst),
                1,
                "CLI must not retry or split the batch"
            );
        }
    }
}

#[test]
fn upstream_auth_rate_limit_http_and_malformed_responses_are_sanitized_nop_judgments() {
    for (status, body, expected_error) in [
        (401, BODY_MARKER.as_bytes().to_vec(), "authentication"),
        (403, BODY_MARKER.as_bytes().to_vec(), "authentication"),
        (429, BODY_MARKER.as_bytes().to_vec(), "rate_limited"),
        (500, BODY_MARKER.as_bytes().to_vec(), "http"),
        (
            200,
            format!("{{\"private\":\"{BODY_MARKER}\", malformed").into_bytes(),
            "invalid_response",
        ),
        (
            200,
            serde_json::to_vec(&json!({"answers": {PRESERVATION: {"noul": 0.0}}})).unwrap(),
            "invalid_response",
        ),
        (
            200,
            serde_json::to_vec(
                &json!({"answers": {PRESERVATION: {"noul": 0.0}, TASK_SCOPE: {"noul": 1.0}}, "action": "allow"}),
            )
            .unwrap(),
            "invalid_response",
        ),
    ] {
        let server = Server::new(status, body);
        let mut files = TemporaryFiles::new();
        let config_path = files.json("config.json", &config("bailian", &server.base_url));
        let request = request();
        let output = run(
            evaluate_command(&config_path, OsStr::new("-")),
            Some(&serde_json::to_vec(&request).unwrap()),
        );
        assert_private(&output, &files);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["model_called"], true);
        assert_eq!(response["outcome"]["control"]["candidate"], "nop");
        assert_eq!(response["backend"]["mode"], "loopback_test");
        for answer in response["answers"].as_array().unwrap() {
            assert_eq!(answer["value"], "unknown");
            assert_eq!(answer["confidence_ppm"], 0);
        }
        for judgment in response["judgments"].as_array().unwrap() {
            assert_eq!(judgment["error"], expected_error);
            assert_eq!(judgment["probability"], Value::Null);
            assert_eq!(judgment["model_used"], false);
        }
        assert_capture(server.receive(), "bailian", &request);
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn model_timeout_is_emitted_as_nop_before_the_event_deadline() {
    let server = Server::delayed(200, provider_reply(0.0, 1.0), Duration::from_millis(300));
    let mut files = TemporaryFiles::new();
    let mut configuration = config("bailian", &server.base_url);
    configuration["timeout_ms"] = json!(100);
    let config_path = files.json("config.json", &configuration);
    let mut request = request();
    request["evidence"][0]["fact"]["parameters"]["result"] = json!("unique");
    let output = run(
        evaluate_command(&config_path, OsStr::new("-")),
        Some(&serde_json::to_vec(&request).unwrap()),
    );
    assert_private(&output, &files);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["outcome"]["control"]["candidate"], "nop");
    assert_eq!(
        response["outcome"]["obligations"],
        json!([{"id": "audit", "critical": true, "action": {"kind": "audit", "parameters": {}}}])
    );
    for answer in response["answers"].as_array().unwrap() {
        assert_eq!(answer["value"], "unknown");
        assert_eq!(answer["confidence_ppm"], 0);
    }
    for judgment in response["judgments"].as_array().unwrap() {
        assert_eq!(judgment["error"], "timeout");
        assert_eq!(judgment["model_used"], false);
    }
    assert_capture(server.receive(), "bailian", &request);
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn malformed_input_and_configuration_errors_do_not_echo_paths_bodies_or_credentials() {
    let server = Server::new(200, provider_reply(0.0, 1.0));
    let mut files = TemporaryFiles::new();
    let valid_config = config("bailian", &server.base_url);
    let config_path = files.json("config.json", &valid_config);
    let malformed = format!("{{\"task\":\"{TASK_MARKER}\", malformed");
    let output = run(
        evaluate_command(&config_path, OsStr::new("-")),
        Some(malformed.as_bytes()),
    );
    assert_error(&output, "invalid_json", &files);

    let malformed_config = files.write(
        "private-malformed-config.json",
        format!("{{\"private\":\"{BODY_MARKER}\", malformed").as_bytes(),
    );
    let output = run(evaluate_command(&malformed_config, OsStr::new("-")), None);
    assert_error(&output, "invalid_json", &files);

    let mut invalid_config = valid_config.clone();
    invalid_config["thresholds"]["preservation_ppm"] = json!(500000);
    let invalid_path = files.json("private-invalid-config.json", &invalid_config);
    let output = run(evaluate_command(&invalid_path, OsStr::new("-")), None);
    assert_error(&output, "invalid_input", &files);

    let mut injected_config = valid_config.clone();
    injected_config["backend"]["api_key"] = json!(SYNTHETIC_KEY);
    let injected_path = files.json("private-key-config.json", &injected_config);
    let output = run(evaluate_command(&injected_path, OsStr::new("-")), None);
    assert_error(&output, "invalid_json", &files);

    let missing = files.directory.join("private-missing-request.json");
    let output = run(evaluate_command(&config_path, missing.as_os_str()), None);
    assert_error(&output, "input_error", &files);
    let output = run(evaluate_command(&missing, OsStr::new("-")), None);
    assert_error(&output, "input_error", &files);
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn missing_credentials_are_removed_from_the_child_only_and_never_trigger_http() {
    let server = Server::new(200, provider_reply(0.0, 1.0));
    let mut files = TemporaryFiles::new();
    let config_path = files.json("config.json", &config("bailian", &server.base_url));
    let mut command = evaluate_command(&config_path, OsStr::new("-"));
    command.env_remove(KEY_ENV);
    let output = run(command, Some(&serde_json::to_vec(&request()).unwrap()));
    assert_error(&output, "missing_credentials", &files);
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn bad_evaluate_arguments_fail_closed_without_echoing_values() {
    let files = TemporaryFiles::new();
    for args in [
        vec!["evaluate"],
        vec!["evaluate", "--config"],
        vec!["evaluate", "--config", "-", "-"],
        vec!["evaluate", "--backend", BODY_MARKER, "-"],
        vec!["evaluate", "--config", BODY_MARKER, "-", "extra"],
        vec!["evaluate", "--fixture", "allow"],
    ] {
        let mut command = binary();
        command.args(args);
        let output = run(command, None);
        assert_error(&output, "invalid_arguments", &files);
    }
}

#[test]
fn held_open_stdin_exits_at_configuration_cap_without_waiting_for_eof() {
    let server = Server::new(200, provider_reply(0.0, 1.0));
    let mut files = TemporaryFiles::new();
    let mut configuration = config("bailian", &server.base_url);
    configuration["timeout_ms"] = json!(200);
    let config_path = files.json("config.json", &configuration);
    let start = Instant::now();
    let mut child = evaluate_command(&config_path, OsStr::new("-"))
        .spawn()
        .unwrap();
    let mut held_open = child.stdin.take().unwrap();
    if let Err(error) = held_open.write_all(b"{") {
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
    // Keep the parent's pipe handle alive until after the child has exited.
    let output = wait_bounded(child, Duration::from_secs(2));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_error(&output, "deadline_exceeded", &files);
    drop(held_open);
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn cli_enforces_hard_preservation_without_contacting_the_configured_server() {
    let server = Server::new(200, provider_reply(0.0, 1.0));
    let mut files = TemporaryFiles::new();
    let config_path = files.json("config.json", &config("bailian", &server.base_url));
    let mut request = request();
    request["task"]["preserve_resource_ids"] = json!(["result-1"]);
    let output = run(
        evaluate_command(&config_path, OsStr::new("-")),
        Some(&serde_json::to_vec(&request).unwrap()),
    );
    assert_private(&output, &files);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["model_called"], false);
    assert_eq!(response["outcome"]["control"]["candidate"], "block");
    for answer in response["answers"].as_array().unwrap() {
        assert_eq!(answer["value"], "preserve");
    }
    for judgment in response["judgments"].as_array().unwrap() {
        assert_eq!(judgment["model_used"], false);
        assert_eq!(judgment["probability"], Value::Null);
    }
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn stdout_backpressure_cannot_extend_event_or_evidence_validity() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    for (evidence_expiry, preservation, task_scope) in [
        (false, 0.0, 1.0),
        (true, 0.0, 1.0),
        (false, 0.5, 0.5),
        (true, 0.5, 0.5),
    ] {
        let server = Server::new(200, provider_reply(preservation, task_scope));
        let mut files = TemporaryFiles::new();
        let config_path = files.json("config.json", &config("bailian", &server.base_url));
        let mut input = request();
        let expires = Ingress::capture().unwrap().now_ms() + 300;
        if evidence_expiry {
            input["evidence"][0]["valid_until_ms"] = json!(expires);
        } else {
            input["budget"]["deadline_ms"] = json!(expires);
        }
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        let mut filled = 0;
        loop {
            match writer.write(&[b'x'; 4096]) {
                Ok(count) => filled += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("cannot fill output socket: {error}"),
            }
        }
        writer.set_nonblocking(false).unwrap();
        let mut command = evaluate_command(&config_path, OsStr::new("-"));
        command.stdout(Stdio::from(OwnedFd::from(writer.try_clone().unwrap())));
        let mut child = command.spawn().unwrap();
        drop(command);
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&input).unwrap())
            .unwrap();
        let output = wait_bounded(child, Duration::from_secs(2));
        assert_error(&output, "deadline_exceeded", &files);
        drop(writer);
        reader
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut captured = Vec::new();
        reader.read_to_end(&mut captured).unwrap();
        assert_eq!(captured, vec![b'x'; filled]);
    }
}
