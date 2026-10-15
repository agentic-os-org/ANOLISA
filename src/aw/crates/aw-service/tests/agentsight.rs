//! Real HTTP fixtures exercise optional read-only queries without AgentSight or eBPF.
#![cfg(target_os = "linux")]

use aw_service::agentsight::{Client, Error, Limits, Outcome, MAX_STEPS};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::{symlink, PermissionsExt},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const SECRET: &str = "fixture-private-token";
const PRIVATE: &str = "fixture-private-body";

struct Reply {
    status: u16,
    body: String,
    headers: String,
    delay: Duration,
    body_delay: Duration,
    omit_length: bool,
}

impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            body: value.to_string(),
            headers: String::new(),
            delay: Duration::ZERO,
            body_delay: Duration::ZERO,
            omit_length: false,
        }
    }
}

struct Fixture {
    endpoint: String,
    requests: Arc<Mutex<Vec<String>>>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let captured = requests.clone();
        let stop = stopped.clone();
        let worker = thread::spawn(move || {
            let expires = Instant::now() + Duration::from_secs(10);
            for reply in replies {
                let mut stream = loop {
                    if stop.load(Ordering::Acquire) || Instant::now() >= expires {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2))
                        }
                        Err(e) => panic!("fixture accept: {e}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                captured.lock().unwrap().push(read_request(&mut stream));
                let until = Instant::now() + reply.delay;
                while Instant::now() < until {
                    if stop.load(Ordering::Acquire) || Instant::now() >= expires {
                        return;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                let length = if reply.omit_length {
                    String::new()
                } else {
                    format!("Content-Length: {}\r\n", reply.body.len())
                };
                let header = format!("HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\n{}Connection: close\r\n{}\r\n", reply.status, length, reply.headers);
                let _ = stream.write_all(header.as_bytes());
                let until = Instant::now() + reply.body_delay;
                while Instant::now() < until {
                    if stop.load(Ordering::Acquire) || Instant::now() >= expires {
                        return;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                // Timeouts deliberately close the client before this bounded fixture responds.
                let _ = stream.write_all(reply.body.as_bytes());
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let token = directory.path().join("token");
        fs::write(&token, format!("{SECRET}\n")).unwrap();
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            endpoint,
            requests,
            stopped,
            worker: Some(worker),
            directory,
        }
    }

    fn client(&self) -> Client {
        Client::connect(&self.endpoint, &self.directory.path().join("token")).unwrap()
    }

    fn query(&self, session: &str) -> Result<aw_service::agentsight::QueryResult, Error> {
        self.client().trajectory(session, Limits::default())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") && bytes.len() < 8192 {
        let mut byte = [0];
        if stream.read(&mut byte).unwrap() == 0 {
            break;
        }
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).unwrap()
}

fn trajectory(session: &str, steps: usize) -> Value {
    json!({"schema_version":"ATIF-v1.7", "session_id":session,
        "agent":{"name":"qoder", "version":"fixture"},
        "steps":(1..=steps).map(|id| json!({"step_id":id,"source":"agent","message":PRIVATE})).collect::<Vec<_>>()})
}

fn replies(value: Value) -> Vec<Reply> {
    vec![
        Reply::json(json!({"auth_enabled":true,"capabilities":["tokens","security"]})),
        Reply::json(value),
    ]
}

#[test]
fn selected_query_is_read_only_scoped_and_explicitly_unassociated() {
    let session = "qoder:session /?#%中文";
    let fixture = Fixture::new(replies(trajectory(session, 2)));
    let result = serde_json::to_value(fixture.query(session).unwrap()).unwrap();
    assert_eq!(result["schema_version"], "aw.agentsight.query/v1alpha1");
    assert!(result["source"]["version"].is_null());
    assert_eq!(
        result["source"]["authentication"],
        "loopback_unauthenticated"
    );
    assert_eq!(result["association"], "unassociated");
    assert_eq!(result["coverage"], "unknown");
    assert_eq!(result["status"], "available");
    assert_eq!(result["step_count"], 2);
    assert_eq!(result["trajectory"]["session_id"], session);
    assert!(!result.to_string().contains(SECRET));
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert!(request.starts_with("GET "));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        assert!(!request.contains(SECRET));
    }
    assert!(requests[0].starts_with("GET /api/auth/status "));
    assert!(requests[1]
        .starts_with("GET /api/trajectories/qoder:session%20%2F%3F%23%25%E4%B8%AD%E6%96%87 "));
    assert_eq!(fs::read_dir(fixture.directory.path()).unwrap().count(), 1);
}

#[test]
fn empty_content_is_available_but_missing_record_or_store_is_unknown() {
    let fixture = Fixture::new(replies(trajectory("s", 0)));
    let result = fixture.query("s").unwrap();
    assert!(matches!(
        result.outcome,
        Outcome::Available { step_count: 0, .. }
    ));
    assert_eq!(result.coverage, "unknown");
    for message in ["Trajectory store not available", "Trajectory not found"] {
        let mut responses = replies(json!({"error":"not_found","message":message}));
        responses[1].status = 404;
        let fixture = Fixture::new(responses);
        let result = fixture.query("s").unwrap();
        assert!(matches!(
            result.outcome,
            Outcome::Unknown {
                reason_code: "record_or_store_unavailable"
            }
        ));
    }
}

#[test]
fn auth_versions_and_payload_identity_are_checked_before_content_is_accepted() {
    let mut cases = Vec::new();
    let mut disabled = replies(trajectory("s", 1));
    disabled[0].body = json!({"auth_enabled":false}).to_string();
    cases.push((disabled, "authentication", 1));
    for auth in [json!({}), json!({"auth_enabled":"true"})] {
        let mut malformed = replies(trajectory("s", 1));
        malformed[0].body = auth.to_string();
        cases.push((malformed, "response invalid", 1));
    }
    cases.push((replies(trajectory("different", 1)), "response invalid", 2));
    let mut atif = trajectory("s", 1);
    atif["schema_version"] = json!("ATIF-v9");
    cases.push((replies(atif), "version unsupported", 2));
    let mut malformed = replies(trajectory("s", 1));
    malformed[1].body = format!("<html>{PRIVATE}</html>");
    cases.push((malformed, "response invalid", 2));
    let mut invalid_steps = trajectory("s", 1);
    invalid_steps["steps"][0]["step_id"] = json!(2);
    cases.push((replies(invalid_steps), "response invalid", 2));
    for (responses, category, calls) in cases {
        let fixture = Fixture::new(responses);
        let error = fixture.query("s").err().unwrap().to_string();
        assert!(error.contains(category), "{error}");
        assert!(!error.contains(PRIVATE));
        assert!(!error.contains(SECRET));
        assert_eq!(fixture.requests.lock().unwrap().len(), calls);
    }
}

#[test]
fn failures_and_redirects_do_not_become_empty_success_or_retries() {
    for status in [401, 403, 500, 302] {
        let mut responses = replies(json!({"error":PRIVATE}));
        responses[1].status = status;
        responses[1].headers = "Location: http://127.0.0.1:1/stolen\r\n".into();
        let fixture = Fixture::new(responses);
        let error = fixture.query("s").err().unwrap();
        assert!(matches!(
            error,
            Error::Authentication | Error::Http(500) | Error::Redirect
        ));
        assert!(!error.to_string().contains(PRIVATE));
        assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    }
}

#[test]
fn shared_deadline_and_aggregate_bytes_and_nested_steps_are_bounded() {
    let mut responses = replies(trajectory("s", 0));
    responses[0].delay = Duration::from_millis(120);
    responses[1].delay = Duration::from_millis(120);
    let fixture = Fixture::new(responses);
    let start = Instant::now();
    let error = fixture
        .client()
        .trajectory(
            "s",
            Limits {
                timeout: Duration::from_millis(180),
                ..Limits::default()
            },
        )
        .err()
        .unwrap();
    assert!(matches!(error, Error::Deadline), "{error}");
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(fixture.requests.lock().unwrap().len() <= 2);
    let body = trajectory("s", 1);
    let responses = replies(body);
    let bytes = responses.iter().map(|r| r.body.len()).sum::<usize>() - 1;
    let fixture = Fixture::new(responses);
    assert!(matches!(
        fixture.client().trajectory(
            "s",
            Limits {
                bytes,
                ..Limits::default()
            }
        ),
        Err(Error::Limit)
    ));
    let mut root = trajectory("s", 1);
    root["subagent_trajectories"] = json!([trajectory("child", MAX_STEPS)]);
    let fixture = Fixture::new(replies(root));
    assert!(matches!(fixture.query("s"), Err(Error::Limit)));
}

#[test]
fn unsafe_endpoints_credentials_and_session_paths_never_send_requests() {
    let fixture = Fixture::new(vec![]);
    let token = fixture.directory.path().join("token");
    for url in [
        "http://example.com",
        "http://localhost",
        "http://127.1",
        "http://2130706433",
        "http://127.0.0.1:0",
        "https://example.com/api",
        "https://user:secret@example.com",
        "https://example.com?token=secret",
        "https://example.com#secret",
        "ftp://127.0.0.1",
        "https://example.com\\@other",
        "https://example.com\n",
    ] {
        assert!(
            matches!(Client::connect(url, &token), Err(Error::Configuration(_))),
            "{url}"
        );
    }
    for session in ["", ".", "..", "s\n"] {
        assert!(matches!(
            fixture.query(session),
            Err(Error::Configuration(_))
        ));
    }
    let link = fixture.directory.path().join("link");
    symlink(&token, &link).unwrap();
    assert!(matches!(
        Client::connect("https://agentsight.invalid", &link),
        Err(Error::Credential)
    ));
    fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        Client::connect("https://agentsight.invalid", &token),
        Err(Error::Credential)
    ));
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    for bad in [
        String::new(),
        format!("{SECRET}\nX-header: leaked"),
        "x".repeat(4097),
    ] {
        fs::write(&token, bad).unwrap();
        assert!(matches!(
            Client::connect("https://agentsight.invalid", &token),
            Err(Error::Credential)
        ));
    }
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[test]
fn streaming_body_limits_and_body_timeouts_remain_visible() {
    let mut responses = replies(trajectory("s", 2));
    let bytes = responses.iter().map(|r| r.body.len()).sum::<usize>() - 1;
    responses[1].omit_length = true;
    let fixture = Fixture::new(responses);
    assert!(matches!(
        fixture.client().trajectory(
            "s",
            Limits {
                bytes,
                ..Limits::default()
            }
        ),
        Err(Error::Limit)
    ));
    let mut responses = replies(trajectory("s", 2));
    responses[1].body_delay = Duration::from_millis(300);
    let fixture = Fixture::new(responses);
    let error = fixture
        .client()
        .trajectory(
            "s",
            Limits {
                timeout: Duration::from_millis(100),
                ..Limits::default()
            },
        )
        .err()
        .unwrap();
    assert!(matches!(error, Error::Deadline), "{error}");
}

#[test]
fn unavailable_source_and_invalid_limits_never_look_available() {
    let fixture = Fixture::new(vec![]);
    assert!(matches!(fixture.query("s"), Err(Error::Unavailable)));
    for limits in [
        Limits {
            timeout: Duration::ZERO,
            ..Limits::default()
        },
        Limits {
            timeout: Duration::from_secs(61),
            ..Limits::default()
        },
        Limits {
            bytes: 0,
            ..Limits::default()
        },
        Limits {
            bytes: aw_service::agentsight::MAX_BYTES + 1,
            ..Limits::default()
        },
    ] {
        assert!(matches!(
            fixture.client().trajectory("s", limits),
            Err(Error::Configuration(_))
        ));
    }
}
