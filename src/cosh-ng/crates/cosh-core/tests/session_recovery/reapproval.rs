use super::*;
use std::time::{Duration, Instant};

fn read_provider_request(stream: &mut std::net::TcpStream) {
    let mut request = Vec::new();
    let header_end = loop {
        let mut bytes = [0; 4096];
        let count = stream.read(&mut bytes).unwrap();
        assert!(count > 0, "provider request ended before headers");
        request.extend_from_slice(&bytes[..count]);
        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    assert!(headers.starts_with("POST /v1/chat/completions "));
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse().unwrap())
        })
        .unwrap();
    while request.len() < header_end + length {
        let mut bytes = [0; 4096];
        let count = stream.read(&mut bytes).unwrap();
        assert!(count > 0, "provider request body ended early");
        request.extend_from_slice(&bytes[..count]);
    }
}

fn write_provider() -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        for tool_call in [true, false] {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "provider request did not arrive");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept provider request: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            read_provider_request(&mut stream);
            let delta = if tool_call {
                json!({"tool_calls":[{"index":0,"id":"write-effect","type":"function","function":{"name":"write_file","arguments":json!({"path":"effect","content":"unapproved"}).to_string()}}]})
            } else {
                json!({"content":"done"})
            };
            let chunk = json!({"choices":[{"index":0,"delta":delta,"finish_reason":if tool_call {"tool_calls"} else {"stop"}}]});
            let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (format!("http://{address}/v1"), server)
}

#[test]
fn resumed_failed_effect_requires_fresh_approval() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&workspace).unwrap();
    configure(&home, &store);
    let initial = json_lines(&run_prompt(&home, &workspace, &["start task"]));
    let session_id = initial
        .iter()
        .find(|message| message["type"] == "result")
        .unwrap()["session_id"]
        .as_str()
        .unwrap();
    let path = fs::read_dir(&store)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(format!("{session_id}.json")))
        .find(|path| path.is_file())
        .unwrap();
    let mut envelope: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    envelope["failed_side_effect_pending_approval"] = json!(true);
    fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    let (base_url, server) = write_provider();
    fs::write(
        home.join(".copilot-shell/config.toml"),
        format!(
            r#"
[ai]
active_provider = "test"
[ai.providers.test]
type = "openai"
api_key = "local-test"
base_url = "{base_url}"
model = "mock-history"
[session]
auto_persist = true
persist_dir = "{}"
"#,
            store.display()
        ),
    )
    .unwrap();
    let output = run_protocol(
        &home,
        &workspace,
        &[
            "--resume",
            session_id,
            "--approval-mode",
            "auto",
            "continue task",
        ],
        &[
            json!({"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"deny"}}}),
        ],
    );
    server.join().unwrap();
    let messages = json_lines(&output);
    assert!(
        messages
            .iter()
            .any(|message| message["type"] == "control_request"
                && message["request"]["subtype"] == "can_use_tool"),
        "{messages:?}"
    );
    assert!(
        !workspace.join("effect").exists(),
        "resumed task silently ran a new side effect"
    );
    let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["failed_side_effect_pending_approval"], true);
    assert_eq!(
        fs::read(path.parent().unwrap().join(format!(".{session_id}.live"))).unwrap(),
        [1]
    );
}

struct RunningCore(std::process::Child);

impl Drop for RunningCore {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn configure_effect_provider(home: &Path, store: &Path, base_url: &str) {
    fs::write(
        home.join(".copilot-shell/config.toml"),
        format!(
            r#"
[ai]
active_provider = "test"
[ai.providers.test]
type = "openai"
api_key = "local-test"
base_url = "{base_url}"
model = "mock-history"
[session]
auto_persist = true
persist_dir = "{}"
"#,
            store.display()
        ),
    )
    .unwrap();
}

fn interrupted_failed_effect_requires_approval(signal: &str, auto_persist: bool) {
    interrupted_effect_requires_approval(signal, auto_persist, "allow");
}

fn interrupted_effect_requires_approval(signal: &str, auto_persist: bool, behavior: &str) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let store = temp.path().join("sessions");
    fs::create_dir_all(&workspace).unwrap();
    configure(&home, &store);
    let initial = json_lines(&run_prompt(&home, &workspace, &["start task"]));
    let session_id = initial
        .iter()
        .find(|message| message["type"] == "result")
        .unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (failed_tx, failed_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        for index in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "provider request did not arrive");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept provider request: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            read_provider_request(&mut stream);
            if index == 1 {
                // Receiving the next model request proves the failed effect finished,
                // while withholding its response prevents the turn commit.
                failed_tx.send(()).unwrap();
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
                return;
            }
            let chunk = json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"failed-effect","type":"function","function":{"name":"shell","arguments":json!({"command":"printf original > effect; exit 23"}).to_string()}}]},"finish_reason":"tool_calls"}]});
            let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    configure_effect_provider(&home, &store, &base_url);
    if !auto_persist {
        let path = home.join(".copilot-shell/config.toml");
        fs::write(
            &path,
            fs::read_to_string(&path)
                .unwrap()
                .replace("auto_persist = true", "auto_persist = false"),
        )
        .unwrap();
    }
    let mut child = RunningCore(
        isolated_command(&home)
            .args(["--headless", "--workspace"])
            .arg(&workspace)
            .args([
                "--resume",
                &session_id,
                "--approval-mode",
                "auto",
                "perform effect",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    writeln!(child.0.stdin.as_mut().unwrap(), "{}", json!({"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":behavior}}})).unwrap();
    failed_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("failed effect reached next provider request");
    if behavior == "allow" {
        assert_eq!(fs::read(workspace.join("effect")).unwrap(), b"original");
    } else {
        assert!(!workspace.join("effect").exists());
    }
    assert!(Command::new("kill")
        .args([format!("-{signal}"), child.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(3);
    while child.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "interrupted Core did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(child);
    drop(release_tx);
    server.join().unwrap();
    let (base_url, server) = write_provider();
    configure_effect_provider(&home, &store, &base_url);
    let output = run_protocol(
        &home,
        &workspace,
        &[
            "--resume",
            &session_id,
            "--approval-mode",
            "auto",
            "continue task",
        ],
        &[
            json!({"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"deny"}}}),
        ],
    );
    server.join().unwrap();
    let messages = json_lines(&output);
    let requested = messages.iter().any(|message| {
        message["type"] == "control_request" && message["request"]["subtype"] == "can_use_tool"
    });
    if behavior == "allow" {
        assert!(
            requested,
            "SIG{signal} lost failed-effect approval requirement: {messages:?}"
        );
        assert_eq!(
            fs::read(workspace.join("effect")).unwrap(),
            b"original",
            "SIG{signal} resumed session executed an unapproved write"
        );
    } else {
        assert!(
            !requested,
            "an unexecuted denial created a persistent failure: {messages:?}"
        );
        assert_eq!(fs::read(workspace.join("effect")).unwrap(), b"unapproved");
    }
}

#[cfg(unix)]
#[test]
fn sigint_before_turn_commit_preserves_failed_effect_reapproval() {
    interrupted_failed_effect_requires_approval("INT", true);
}

#[cfg(unix)]
#[test]
fn killed_before_turn_commit_preserves_failed_effect_reapproval() {
    interrupted_failed_effect_requires_approval("KILL", true);
}

#[cfg(unix)]
#[test]
fn resumed_nonpersisting_turn_preserves_failed_effect_reapproval() {
    interrupted_failed_effect_requires_approval("KILL", false);
}

#[cfg(unix)]
#[test]
fn denied_unexecuted_effect_does_not_arm_resumed_session() {
    interrupted_effect_requires_approval("INT", true, "deny");
}

#[path = "approval_exposure.rs"]
mod approval_exposure;
