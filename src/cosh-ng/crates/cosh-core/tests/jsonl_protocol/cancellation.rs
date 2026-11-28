//! Exercise request cancellation on the v1 wire and capture the next model request.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn model_request(stream: &mut TcpStream) -> Value {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut length = None;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let mut body = vec![0; length.expect("model request content length")];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn exchange(tool: Value, shell: bool, typed_cancel: bool) -> (Vec<Value>, Vec<Value>) {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    configure(home.path(), listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(15);
        for index in 0..if shell && !typed_cancel { 1 } else { 2 } {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "model request {index} timed out");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept model request: {error}"),
                }
            };
            requests.push(model_request(&mut stream));
            let delta = if index == 0 {
                json!({"tool_calls":[tool]})
            } else {
                json!({"content":"next request completed"})
            };
            let chunk = json!({"choices":[{"index":0,"delta":delta}]});
            let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        requests
    });

    let mut capabilities = json!({
        "can_handle_can_use_tool": true,
        "can_handle_host_executed_shell": true,
        "can_handle_ecs_trial_v1": true,
    });
    if typed_cancel {
        capabilities["can_handle_turn_cancelled"] = json!(true);
    }
    let user = |text| json!({"type":"user","message":{"role":"user","content":text}});
    let control = |id, request| json!({"type":"control_request","request_id":id,"request":request});
    let input = [
        control(
            "init-1",
            json!({"subtype":"initialize","protocol_version":1,"capabilities":capabilities}),
        ),
        user("first request"),
        control("interrupt-1", json!({"subtype":"interrupt"})),
        user("next request"),
        control("shutdown-1", json!({"subtype":"shutdown"})),
    ]
    .map(|message| message.to_string());
    let mut args = vec![
        "--headless",
        "--workspace",
        workspace.path().to_str().unwrap(),
    ];
    if shell {
        args.push("--cosh-shell-transport");
    }
    let output =
        super::run_process_at_home_args(home.path(), &args, &input.each_ref().map(String::as_str));
    assert!(output.status.success(), "{output:?}");
    let frames = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect();
    (frames, server.join().unwrap())
}

fn configure(home: &Path, address: std::net::SocketAddr) {
    let directory = home.join(".copilot-shell");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("config.toml"),
        format!(
            r#"
[ai]
active_provider = "fixture"
[ai.providers.fixture]
type = "openai"
base_url = "http://{address}/compatible-mode/v1"
api_key = "synthetic-test-key"
model = "fixture-qwen"
[agent]
approval_mode = "recommend"
[session]
auto_persist = false
"#
        ),
    )
    .unwrap();
}

#[test]
fn negotiated_cancellation_keeps_the_next_model_request_compatible() {
    assert_negotiated_cancellation_reuses_process(false);
    assert_negotiated_cancellation_reuses_process(true);
}

fn assert_negotiated_cancellation_reuses_process(shell: bool) {
    let tool = json!({"index":0,"id":"question-one","type":"function","function":{
        "name":"ask_user_question","arguments":r#"{"question":"Which branch?","options":[{"label":"main"}]}"#
    }});
    let (frames, requests) = exchange(tool, shell, true);
    assert!(
        frames
            .iter()
            .any(|frame| frame["request"]["subtype"] == "ask_user"),
        "{frames:?}"
    );
    let results = frames
        .iter()
        .filter(|frame| frame["type"] == "result")
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2, "{results:?}");
    assert_eq!(results[0]["error_code"], "turn_cancelled");
    assert_eq!(results[1]["subtype"], "success");
    assert!(results[0]["session_id"].is_string());
    assert_eq!(results[0]["session_id"], results[1]["session_id"]);

    let messages = requests[1]["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert!(
        messages[1..]
            .iter()
            .all(|message| message["role"] != "system"),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == "question-one"),
        "{messages:?}"
    );
    assert_eq!(messages.last().unwrap()["content"], "next request");
}

#[test]
fn old_v1_shell_approval_cancel_ends_with_eof() {
    let tool = json!({"index":0,"id":"write-one","type":"function","function":{
        "name":"write_file","arguments":r#"{"file_path":"fixture.txt","content":"fixture"}"#
    }});
    let (frames, requests) = exchange(tool, true, false);
    assert!(
        frames
            .iter()
            .any(|frame| frame["response"]["response"]["protocol_version"] == 1),
        "{frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|frame| frame["request"]["subtype"] == "can_use_tool"),
        "{frames:?}"
    );
    assert_eq!(requests.len(), 1);
    assert!(
        !frames.iter().any(|frame| frame["type"] == "result"),
        "{frames:?}"
    );
}
