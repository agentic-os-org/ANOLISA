use super::*;
use std::path::Path;
use std::sync::{Arc, Mutex};
use wait_timeout::ChildExt;

fn provider_fixture(
    home: &Path,
    replies: Vec<serde_json::Value>,
) -> (
    Arc<Mutex<Vec<serde_json::Value>>>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    std::fs::write(home.join(".copilot-shell/config.toml"), format!(
        "[ai]\nactive_provider='fixture'\n[ai.providers.fixture]\ntype='openai'\nbase_url='http://{address}/v1'\napi_key='fixture'\nmodel='fixture'\n"
    )).expect("fixture provider config");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let worker = std::thread::spawn(move || {
        for reply in replies {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => return,
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("read deadline");
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                let count = stream.read(&mut buffer).expect("read provider request");
                if count == 0 {
                    return;
                }
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|chunk| chunk == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().expect("content length"))
                        })
                        .expect("request content length");
                    if bytes.len() >= end + 4 + length {
                        captured.lock().expect("capture lock").push(
                            serde_json::from_slice(&bytes[end + 4..end + 4 + length])
                                .expect("provider JSON"),
                        );
                        break;
                    }
                }
            }
            let body = format!("data: {reply}\n\ndata: [DONE]\n\n");
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).expect("provider reply");
        }
    });
    (requests, worker)
}

fn text_reply() -> serde_json::Value {
    serde_json::json!({"choices":[{"index":0,"delta":{"content":"fixture-complete"},"finish_reason":"stop"}]})
}

#[test]
fn public_preserves_configured_provider_parameters_like_legacy() {
    for configured in [false, true] {
        for mode in ["none", "read", "write"] {
            let home = tempfile::tempdir().expect("home");
            configure_mock(home.path());
            let workspace = tempfile::tempdir().expect("workspace");
            let (requests, worker) =
                provider_fixture(home.path(), vec![text_reply(), text_reply()]);
            let config_path = home.path().join(".copilot-shell/config.toml");
            let mut config = std::fs::read_to_string(&config_path)
                .expect("provider config")
                .replace("type='openai'", "type='generic'");
            if configured {
                config.push_str("extra_params = { temperature = 0.125, max_tokens = 123 }\n");
            }
            std::fs::write(config_path, config).expect("configured provider parameters");
            let legacy = common::cosh_core_command(home.path())
                .args(["--headless", "hello"])
                .current_dir(workspace.path())
                .stdin(Stdio::null())
                .output()
                .expect("legacy provider request");
            let public = run(
                home.path(),
                &[
                    "--output=json",
                    &format!("--file-tools={mode}"),
                    "--workspace",
                    workspace.path().to_str().expect("workspace"),
                    "hello",
                ],
                None,
            );
            worker.join().expect("provider worker");
            assert_eq!(legacy.status.code(), Some(0), "{legacy:?}");
            assert_eq!(public.status.code(), Some(0), "{public:?}");
            let captured = requests.lock().expect("capture");
            assert_eq!(captured.len(), 2);
            let expected_tokens = if configured { 123 } else { 4096 };
            assert_eq!(captured[0]["max_tokens"], expected_tokens);
            assert_eq!(captured[1]["max_tokens"], expected_tokens, "mode={mode}");
            if configured {
                assert_eq!(captured[0]["temperature"], 0.125);
                assert_eq!(captured[1]["temperature"], 0.125, "mode={mode}");
            } else {
                assert!(captured[0].get("temperature").is_none());
                assert!(captured[1].get("temperature").is_none());
            }
        }
    }
}

#[test]
fn public_never_sends_implicit_global_or_project_context() {
    use std::os::unix::fs::symlink;
    for mode in ["none", "read", "write"] {
        for shape in ["file", "symlink", "parent-symlink"] {
            let home = tempfile::tempdir().expect("home");
            configure_mock(home.path());
            let workspace = tempfile::tempdir().expect("workspace");
            let outside = tempfile::tempdir().expect("outside");
            std::fs::write(
                home.path().join(".copilot-shell/CONTEXT.md"),
                "GLOBAL-PRIVATE-MARKER",
            )
            .expect("global context");
            std::fs::write(outside.path().join("CONTEXT.md"), "OUTSIDE-PRIVATE-MARKER")
                .expect("outside context");
            let context = workspace.path().join(".copilot-shell");
            if shape == "parent-symlink" {
                symlink(outside.path(), &context).expect("directory symlink");
            } else {
                std::fs::create_dir(&context).expect("context directory");
                if shape == "symlink" {
                    symlink(
                        outside.path().join("CONTEXT.md"),
                        context.join("CONTEXT.md"),
                    )
                    .expect("file symlink");
                } else {
                    std::fs::write(context.join("CONTEXT.md"), "PROJECT-PRIVATE-MARKER")
                        .expect("project context");
                }
            }
            let (requests, worker) = provider_fixture(home.path(), vec![text_reply()]);
            let output = run(
                home.path(),
                &[
                    "--output=json",
                    &format!("--file-tools={mode}"),
                    "--workspace",
                    workspace.path().to_str().expect("workspace"),
                    "hello",
                ],
                None,
            );
            worker.join().expect("provider worker");
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            let captured = requests.lock().expect("capture");
            assert_eq!(captured.len(), 1);
            let body = captured[0].to_string();
            assert!(
                !body.contains("PRIVATE-MARKER"),
                "mode={mode} shape={shape}: unauthorized Context sent"
            );
        }
    }
}

#[test]
fn public_shell_policy_rules_prevent_command_startup() {
    for outcome in ["Deny", "RequireApproval"] {
        let home = tempfile::tempdir().expect("home");
        configure_model(home.path(), "mock-public-command-failure");
        let workspace = tempfile::tempdir().expect("workspace");
        let policy = home.path().join("shell-policy.toml");
        std::fs::write(&policy, format!("version='fixture'\ndefault='Allow'\n[[rules]]\nname='shell-rule'\nmatches.subsystem='shell'\nmatches.operation='sh'\noutcome='{outcome}'\n")).expect("shell policy");
        let output = run_with_env(
            home.path(),
            &[
                "--output=json",
                "--exec=host",
                "--workspace",
                workspace.path().to_str().expect("workspace"),
                "run",
            ],
            None,
            &[("COSH_AUDIT_POLICY", policy.to_str().expect("policy"))],
        );
        assert!(
            !workspace.path().join("command-started").exists(),
            "shell {outcome} was bypassed"
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("result");
        if outcome == "RequireApproval" {
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(result["error_code"], "approval_required");
        } else {
            assert!(result["answer"]
                .as_str()
                .expect("answer")
                .contains("denied"));
        }
    }
}

#[test]
fn public_missing_explicit_policy_stops_before_provider_contact() {
    use std::os::unix::fs::symlink;
    for shape in [
        "missing",
        "dangling",
        "directory",
        "same-as-default",
        "whitespace",
        "non-utf8",
        "fifo",
        "malformed",
    ] {
        let home = tempfile::tempdir().expect("home");
        configure_mock(home.path());
        let workspace = tempfile::tempdir().expect("workspace");
        let listener = TcpListener::bind("127.0.0.1:0").expect("provider sentinel");
        listener
            .set_nonblocking(true)
            .expect("nonblocking sentinel");
        let address = listener.local_addr().expect("address");
        std::fs::write(home.path().join(".copilot-shell/config.toml"), format!("[ai]\nactive_provider='fixture'\n[ai.providers.fixture]\ntype='openai'\nbase_url='http://{address}/v1'\napi_key='fixture'\nmodel='fixture'\n")).expect("provider config");
        let policy = if shape == "same-as-default" {
            home.path().join(".copilot-shell/cosh/audit.toml")
        } else {
            home.path().join("explicit.toml")
        };
        if shape == "dangling" {
            symlink(home.path().join("absent"), &policy).expect("dangling policy");
        }
        if shape == "directory" {
            std::fs::create_dir(&policy).expect("policy directory");
        }
        if shape == "fifo" {
            nix::unistd::mkfifo(
                &policy,
                nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
            )
            .expect("policy fifo");
        }
        if shape == "malformed" {
            std::fs::write(&policy, "invalid policy").expect("bad policy");
        }
        let mut command = public_command(home.path());
        command
            .args([
                "--output=json",
                "--timeout=1s",
                "--exec=host",
                "--workspace",
                workspace.path().to_str().expect("workspace"),
                "run",
            ])
            .env("COSH_AUDIT_POLICY", &policy);
        if shape == "whitespace" {
            command.env("COSH_AUDIT_POLICY", "   ");
        }
        if shape == "non-utf8" {
            use std::os::unix::ffi::OsStringExt;
            command.env(
                "COSH_AUDIT_POLICY",
                std::ffi::OsString::from_vec(vec![0xff]),
            );
        }
        let output = command
            .stdin(Stdio::null())
            .output()
            .expect("public invalid policy");
        assert_eq!(output.status.code(), Some(1), "shape={shape} {output:?}");
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("result");
        assert_eq!(result["error_code"], "policy_unavailable", "shape={shape}");
        assert_eq!(
            listener
                .accept()
                .expect_err("provider must not be contacted")
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(!workspace.path().join("command-started").exists());
    }
}

#[test]
fn public_policy_tracks_existing_shell_argv_and_audit_provenance() {
    let home = tempfile::tempdir().expect("home");
    configure_mock(home.path());
    let workspace = tempfile::tempdir().expect("workspace");
    let policy = home.path().join("argument-policy.toml");
    std::fs::write(&policy, "version='argv-fixture'\ndefault='Allow'\n[[rules]]\nname='argument-rule'\noutcome='Deny'\nmatches.subsystem='shell'\nmatches.operation='git'\nmatches.target='push'\n[[rules.matches.arg]]\nkey='--force'\n").expect("argument policy");
    let tool_reply = serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"argv-policy-call","type":"function","function":{"name":"run_command","arguments":"{\"program\":\"git\",\"args\":[\"push\",\"--force\",\"literal with whitespace\"]}"}}]},"finish_reason":"tool_calls"}]});
    let (requests, worker) = provider_fixture(home.path(), vec![tool_reply, text_reply()]);
    let output = run_with_env(
        home.path(),
        &[
            "--output=json",
            "--exec=host",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "run",
        ],
        None,
        &[("COSH_AUDIT_POLICY", policy.to_str().expect("policy"))],
    );
    worker.join().expect("worker");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[1]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .any(|message| message["role"] == "tool"
            && message["content"]
                .as_str()
                .is_some_and(|text| text.contains("denied"))));
    let audit = home.path().join(".local/state/cosh/audit/v1/segments");
    let mut decisions = Vec::new();
    for date in std::fs::read_dir(audit).expect("audit dates") {
        for file in std::fs::read_dir(date.expect("date").path()).expect("segments") {
            for line in std::fs::read_to_string(file.expect("segment").path())
                .expect("audit bytes")
                .lines()
            {
                let event: serde_json::Value = serde_json::from_str(line).expect("audit event");
                if event["event_type"] == "policy.decision" {
                    decisions.push(event);
                }
            }
        }
    }
    assert!(decisions
        .iter()
        .any(|event| event["data"]["decision"] == "deny"
            && event["data"]["policy_version"]
                .as_str()
                .is_some_and(|version| version.starts_with("user@argv-fixture+sha256:"))));
    assert!(decisions
        .iter()
        .all(|event| !event.to_string().contains("literal with whitespace")));
}

#[test]
fn legacy_context_loading_remains_enabled() {
    let home = tempfile::tempdir().expect("home");
    configure_mock(home.path());
    let workspace = tempfile::tempdir().expect("workspace");
    let context = workspace.path().join(".copilot-shell");
    std::fs::create_dir(&context).expect("context dir");
    std::fs::write(context.join("CONTEXT.md"), "LEGACY-CONTEXT-MARKER").expect("legacy context");
    let (requests, worker) = provider_fixture(home.path(), vec![text_reply()]);
    let output = common::cosh_core_command(home.path())
        .args([
            "--headless",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "hello",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("legacy run");
    worker.join().expect("worker");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(requests.lock().expect("requests")[0]
        .to_string()
        .contains("LEGACY-CONTEXT-MARKER"));
}

#[test]
fn legacy_mock_models_do_not_activate_public_tool_fixtures() {
    let home = tempfile::tempdir().expect("home");
    for model in [
        "mock-public-write",
        "mock-public-escape",
        "mock-public-symlink",
        "mock-public-command-failure",
        "mock-public-background",
        "mock-public-command-timeout",
        "mock-public-command-long",
        "mock-public-no-ctty",
        "mock-public-ask",
        "mock-public-in-band-ask",
        "mock-public-loop",
    ] {
        configure_model(home.path(), model);
        let output = common::cosh_core_command(home.path())
            .args(["--headless", "hello"])
            .stdin(Stdio::null())
            .output()
            .expect("legacy mock model");
        assert_eq!(output.status.code(), Some(0), "{model}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("mock history: hello"),
            "{model}: {output:?}"
        );
    }
}

#[test]
fn public_slow_reader_and_broken_pipe_preserve_transport_contract() {
    for format in ["text", "json"] {
        let home = tempfile::tempdir().expect("home");
        configure_mock(home.path());
        let (requests, worker) = provider_fixture(
            home.path(),
            vec![
                serde_json::json!({"choices":[{"index":0,"delta":{"content":"x".repeat(128 * 1024)},"finish_reason":"stop"}]}),
            ],
        );
        let mut child = public_command(home.path())
            .args([&format!("--output={format}"), "--timeout=5s", "hello"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let mut stdout = child.stdout.take().expect("stdout");
        let reader = std::thread::spawn(move || {
            let mut collected = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = stdout.read(&mut buffer).expect("read");
                if count == 0 {
                    break;
                }
                collected.extend_from_slice(&buffer[..count]);
                std::thread::sleep(Duration::from_millis(5));
            }
            collected
        });
        assert_eq!(
            child
                .wait_timeout(Duration::from_secs(7))
                .expect("wait")
                .expect("bounded exit")
                .code(),
            Some(0)
        );
        let bytes = reader.join().expect("reader");
        worker.join().expect("worker");
        assert_eq!(requests.lock().expect("requests").len(), 1);
        if format == "json" {
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).expect("JSON")["answer"]
                    .as_str()
                    .expect("answer")
                    .len(),
                128 * 1024
            );
        } else {
            assert_eq!(bytes.len(), 128 * 1024 + 1);
        }
        configure_mock(home.path());
        let mut child = public_command(home.path())
            .args([&format!("--output={format}"), "hello"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn broken pipe");
        drop(child.stdout.take());
        assert_ne!(
            child
                .wait_timeout(Duration::from_secs(3))
                .expect("wait")
                .expect("bounded exit")
                .code(),
            Some(0)
        );
    }
}

#[test]
fn public_stdout_backpressure_respects_task_deadline() {
    for format in ["text", "json"] {
        let home = tempfile::tempdir().expect("home");
        configure_mock(home.path());
        let config = home.path().join(".copilot-shell/config.toml");
        let mut content = std::fs::read_to_string(&config).expect("config");
        content.push_str("\n[session.compaction]\nmodel_context_window=1000000\n");
        std::fs::write(config, content).expect("large context");
        let input = home.path().join("input.txt");
        std::fs::write(&input, vec![b'x'; 256 * 1024]).expect("large input");
        let mut child = public_command(home.path())
            .args([&format!("--output={format}"), "--timeout=1s", "-"])
            .stdin(std::fs::File::open(input).expect("input file"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let status = child
            .wait_timeout(Duration::from_secs(3))
            .expect("bounded wait");
        if status.is_none() {
            child.kill().expect("cleanup stuck child");
            child.wait().expect("reap");
        }
        assert!(
            status.is_some(),
            "format={format}: stdout backpressure exceeded the deadline"
        );
        assert_ne!(status.expect("exit").code(), Some(0));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn public_sigterm_cleans_command_group_with_stalled_output() {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    let home = tempfile::tempdir().expect("home");
    configure_model(home.path(), "mock-public-command-long");
    let workspace = tempfile::tempdir().expect("workspace");
    let (output_read, output_write) = std::os::unix::net::UnixStream::pair().expect("output pair");
    output_write
        .set_nonblocking(true)
        .expect("prefill without blocking");
    let mut fill = &output_write;
    loop {
        match fill.write(&[b'x'; 4096]) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("prefill failed: {error}"),
        }
    }
    output_write
        .set_nonblocking(false)
        .expect("blocking output");
    let mut child = public_command(home.path())
        .args([
            "--output=json",
            "--exec=host",
            "--timeout=20s",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "run",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::os::fd::OwnedFd::from(output_write)))
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let marker = workspace.path().join("long-child.pid");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.exists());
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).expect("cancel");
    let status = child.wait_timeout(Duration::from_secs(2)).expect("wait");
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(output_read);
    assert!(status.is_some(), "cancellation stalled on final output");
    assert_eq!(status.expect("status").code(), Some(1));
    let descendant = read_pid(workspace.path(), "long-child.pid");
    let state = std::fs::read_to_string(format!("/proc/{descendant}/stat"));
    assert!(state.as_ref().map_or(true, |value| value
        .split_once(") ")
        .is_some_and(|(_, rest)| rest.starts_with('Z'))));
}

#[cfg(target_os = "linux")]
#[test]
fn public_signal_cancellation_cleans_command_group_and_returns_json() {
    use nix::sys::signal::{kill, killpg, Signal};
    use nix::unistd::Pid;
    use std::os::unix::process::CommandExt;
    for signal in [Signal::SIGINT, Signal::SIGTERM] {
        for target_group in [false, true] {
            let home = tempfile::tempdir().expect("home");
            configure_model(home.path(), "mock-public-command-long");
            let workspace = tempfile::tempdir().expect("workspace");
            let mut command = public_command(home.path());
            command.process_group(0).args([
                "--output=json",
                "--exec=host",
                "--workspace",
                workspace.path().to_str().expect("workspace"),
                "run",
            ]);
            let mut child = command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn");
            let marker = workspace.path().join("long-child.pid");
            let deadline = Instant::now() + Duration::from_secs(3);
            while !marker.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(marker.exists(), "command startup");
            let descendant = read_pid(workspace.path(), "long-child.pid");
            let core = Pid::from_raw(child.id() as i32);
            if target_group {
                killpg(core, signal).expect("signal group");
            } else {
                kill(core, signal).expect("signal Core");
            }
            let status = child.wait_timeout(Duration::from_secs(3)).expect("wait");
            if status.is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
            let cleanup_deadline = Instant::now() + Duration::from_secs(1);
            let stopped = loop {
                let state = std::fs::read_to_string(format!("/proc/{descendant}/stat"));
                if state.as_ref().map_or(true, |value| {
                    value
                        .split_once(") ")
                        .is_some_and(|(_, rest)| rest.starts_with('Z'))
                }) {
                    break true;
                }
                if Instant::now() >= cleanup_deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            if !stopped {
                let _ = kill(Pid::from_raw(descendant), Signal::SIGKILL);
            }
            assert!(
                stopped,
                "{signal:?}: supervised descendant survived cancellation"
            );
            assert!(status.is_some());
            let output = child.wait_with_output().expect("output");
            assert_ne!(output.status.code(), Some(0));
            let result: serde_json::Value =
                serde_json::from_slice(&output.stdout).expect("cancellation JSON result");
            assert_eq!(result["error_code"], "cancelled");
        }
    }
}
