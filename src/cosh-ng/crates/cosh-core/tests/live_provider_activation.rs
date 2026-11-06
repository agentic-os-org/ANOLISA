use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

mod common;

#[tokio::test]
async fn live_auth_activate_updates_next_provider_request() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(home.join(".copilot-shell")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::write(
        home.join(".copilot-shell/config.toml"),
        format!(
            r#"
[ai]
active_provider = "provider-a"
active_model = "model-a"

[ai.providers.provider-a]
type = "openai_compat"
base_url = "http://{address}/provider-a/v1"
api_key = "sk-fixture-a"
model = "model-a"
extra_params = {{ fixture_marker = "params-a" }}

[ai.providers.provider-b]
type = "openai_compat"
base_url = "http://{address}/provider-b/v1"
api_key = "sk-fixture-b"
model = "model-b"
extra_params = {{ fixture_marker = "params-b" }}

[session]
auto_persist = false

[hooks]
enabled = false
"#
        ),
    )
    .unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let header_end = loop {
                let mut buffer = [0_u8; 4096];
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0, "HTTP request closed before headers");
                request.extend_from_slice(&buffer[..count]);
                if let Some(position) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    break position + 4;
                }
            };
            let headers = String::from_utf8(request[..header_end].to_vec()).unwrap();
            let content_length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .expect("JSON request content length");
            while request.len() < header_end + content_length {
                let mut buffer = [0_u8; 4096];
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0, "HTTP request closed before body");
                request.extend_from_slice(&buffer[..count]);
            }
            let body: Value = serde_json::from_slice(&request[header_end..]).unwrap();
            let response = "data: {\"choices\":[{\"delta\":{\"content\":\"fixture response\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            requests.push((headers, body));
        }
        requests
    });

    let mut command = common::cosh_core_command(&home);
    command
        .args(["--headless", "--workspace"])
        .arg(&workspace)
        .env_remove("COSH_AI_PROVIDER")
        .env_remove("COSH_MODEL")
        .env(
            "COSH_INSTALLATION_ID_PATH",
            home.join(".copilot-shell/installation_id"),
        )
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for message in [
        json!({"type": "user", "message": {"role": "user", "content": "before activation"}}),
        json!({"type": "registry_request", "request_id": "activate-b", "domain": "auth", "action": "activate", "params": {"provider_id": "provider-b"}}),
        json!({"type": "user", "message": {"role": "user", "content": "after activation"}}),
    ] {
        stdin
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
    }
    drop(stdin);
    let (output, requests) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(child.wait_with_output(), server)
    })
    .await
    .expect("live provider activation exceeded its deadline");
    let output = output.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let protocol: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        protocol
            .iter()
            .any(|message| message["type"] == "registry_response"
                && message["request_id"] == "activate-b"
                && message["success"] == true),
        "{protocol:?}"
    );
    assert_eq!(
        protocol
            .iter()
            .filter(|message| message["type"] == "result" && message["is_error"] == false)
            .count(),
        2,
        "{protocol:?}"
    );
    let requests = requests.unwrap();
    assert!(
        requests[0]
            .0
            .starts_with("POST /provider-a/v1/chat/completions "),
        "{}",
        requests[0].0
    );
    assert_eq!(requests[0].1["model"], "model-a");
    assert_eq!(requests[0].1["fixture_marker"], "params-a");
    assert!(
        requests[1]
            .0
            .starts_with("POST /provider-b/v1/chat/completions "),
        "{}",
        requests[1].0
    );
    assert!(requests[1]
        .0
        .to_ascii_lowercase()
        .contains("authorization: bearer sk-fixture-b"));
    assert_eq!(requests[1].1["model"], "model-b");
    assert_eq!(requests[1].1["fixture_marker"], "params-b");
    assert!(requests[1].1["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["content"] == "before activation"));
}
