use super::*;
use std::io::{BufRead, BufReader};

fn read_only_provider() -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "read-only provider request did not arrive"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept read-only provider request: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        read_provider_request(&mut stream);
        let chunk = json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"read-only","type":"function","function":{"name":"read_file","arguments":json!({"path":"read-only"}).to_string()}}]},"finish_reason":"tool_calls"}]});
        let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    (format!("http://{address}/v1"), server)
}

fn killed_before_completion_preserves_exposure(prompt: bool) {
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
    let provider = if prompt {
        None
    } else {
        Some(read_only_provider())
    };
    if let Some((base_url, _)) = provider.as_ref() {
        configure_effect_provider(&home, &store, base_url);
    }
    let config_path = home.join(".copilot-shell/config.toml");
    let hook = if prompt {
        "UserPromptSubmit"
    } else {
        "PreToolUse"
    };
    let mut config = fs::read_to_string(&config_path).unwrap();
    config.push_str(&format!(
        r#"
[hooks]
enabled = true
[[hooks.{hook}]]
name = "exposure-kill"
command = "printf '%s\\n' '{{\"decision\":\"ask\",\"reason\":\"test exposure\"}}'"
"#
    ));
    fs::write(config_path, config).unwrap();
    fs::write(workspace.join("read-only"), "read me").unwrap();
    let mut child = RunningCore(
        isolated_command(&home)
            .args(["--headless", "--workspace"])
            .arg(&workspace)
            .args([
                "--resume",
                &session_id,
                "--approval-mode",
                "auto",
                "perform action",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
    let stderr_reader = std::thread::spawn(move || {
        let mut diagnostics = String::new();
        stderr.read_to_string(&mut diagnostics).unwrap();
        diagnostics
    });
    let (delivered_tx, delivered_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut frames = Vec::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            frames.push(line.clone());
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if frame["type"] == "control_request" && frame["request"]["subtype"] == "can_use_tool" {
                let _ = delivered_tx.send(());
                return frames;
            }
        }
        frames
    });
    if let Err(error) = delivered_rx.recv_timeout(Duration::from_secs(10)) {
        let _ = child.0.kill();
        let status = child.0.wait().unwrap();
        let frames = reader.join().unwrap();
        let diagnostics = stderr_reader.join().unwrap();
        panic!("approval request did not reach Host: {error:?}, prompt={prompt}, status={status}, stdout={frames:?}, stderr={diagnostics}");
    }
    reader.join().unwrap();
    // The test is the hostile Host: execute, but never send a completion.
    fs::write(workspace.join("effect"), "host-effect").unwrap();
    assert!(Command::new("kill")
        .args(["-KILL", &child.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(3);
    while child.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "killed Core did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    let diagnostics = stderr_reader.join().unwrap();
    assert!(
        !diagnostics.contains("panicked"),
        "child panic before kill: {diagnostics}"
    );
    drop(child);
    if let Some((_, server)) = provider {
        server.join().unwrap();
    }
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
    assert!(
        json_lines(&output)
            .iter()
            .any(|frame| frame["type"] == "control_request"
                && frame["request"]["subtype"] == "can_use_tool"),
        "prompt={prompt}: pre-completion kill lost exposure"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("effect")).unwrap(),
        "host-effect",
        "prompt={prompt}: resume executed an unapproved write after Host exposure"
    );
}

#[cfg(unix)]
#[test]
fn killed_before_prompt_completion_preserves_approval_exposure() {
    killed_before_completion_preserves_exposure(true);
}

#[cfg(unix)]
#[test]
fn killed_before_read_only_completion_preserves_approval_exposure() {
    killed_before_completion_preserves_exposure(false);
}
