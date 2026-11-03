use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;
use wait_timeout::ChildExt;

mod common;

fn configure(home: &Path, root: &Path, provider: &str) {
    let directory = home.join(".copilot-shell");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("config.toml"),
        format!(
            r#"
[ai]
active_provider = "test"
[ai.providers.test]
{provider}
model = "mock-compact-summary"
[session]
auto_persist = true
persist_dir = "{}"
[session.compaction]
enabled = true
auto = false
model_context_window = 2000000
"#,
            root.display()
        ),
    )
    .unwrap();
}

fn command(home: &Path, workspace: &Path) -> std::process::Command {
    let mut command = common::cosh_core_command(home);
    command
        .args(["--headless", "--workspace"])
        .arg(workspace)
        .env_remove("OPENAI_API_KEY")
        .env_remove("DASHSCOPE_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn finish(mut child: Child) -> Output {
    if child
        .wait_timeout(Duration::from_secs(15))
        .unwrap()
        .is_none()
    {
        child.kill().unwrap();
        let output = child.wait_with_output().unwrap();
        panic!(
            "core timed out: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    child.wait_with_output().unwrap()
}

fn json_lines(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn session_path(root: &Path, id: &str) -> PathBuf {
    fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(format!("{id}.json")))
        .find(|path| path.is_file())
        .expect("persisted session")
}

#[test]
fn blocked_compaction_survives_another_process_age_sweep() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    let root = temp.path().join("sessions");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    configure(&home, &root, "type = \"mock\"");
    let output = finish(
        command(&home, &first)
            .arg("inspect memory pressure ".repeat(400))
            .spawn()
            .unwrap(),
    );
    assert!(output.status.success());
    let lines = json_lines(&output);
    let id = lines.iter().find(|line| line["type"] == "result").unwrap()["session_id"]
        .as_str()
        .unwrap();
    let path = session_path(&root, id);
    let mut old: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    old["updated_at_ms"] = Value::from(1);
    fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "summary request did not arrive");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept summary request: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut request = Vec::new();
        let header_end = loop {
            let mut bytes = [0; 4096];
            let count = stream.read(&mut bytes).unwrap();
            assert!(count > 0, "summary request ended before headers");
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
                    .map(str::trim)
                    .map(str::to_owned)
            })
            .unwrap()
            .parse()
            .unwrap();
        while request.len() < header_end + length {
            let mut bytes = [0; 4096];
            let count = stream.read(&mut bytes).unwrap();
            assert!(count > 0, "summary request body ended early");
            request.extend_from_slice(&bytes[..count]);
        }
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Memory pressure investigation continues.\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    configure(
        &home,
        &root,
        &format!("type = \"openai\"\napi_key = \"local-test\"\nbase_url = \"http://{address}/v1\""),
    );
    let compact = command(&home, &first)
        .args(["--resume", id, "--compact"])
        .spawn()
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(12)).unwrap();
    configure(&home, &root, "type = \"mock\"");
    let swept = finish(
        command(&home, &second)
            .arg("trigger age sweep")
            .spawn()
            .unwrap(),
    );
    let protected = path.exists();
    release_tx.send(()).unwrap();
    let compacted = finish(compact);
    server.join().unwrap();
    assert!(swept.status.success(), "{swept:?}");
    let result = json_lines(&compacted);
    assert!(
        protected,
        "age sweep removed the in-flight compaction session: {result:?}"
    );
    assert_eq!(result[0]["ok"], true, "{result:?}");
    let committed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(committed["messages"], old["messages"]);
    assert_eq!(committed["compaction_revision"], 1);
    assert!(committed["compaction"].is_object());
}
