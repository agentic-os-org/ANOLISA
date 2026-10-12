use agentsight_opt::llm::ChatMessage;
use agentsight_opt::LlmClient;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Debug, Deserialize)]
struct Ranking {
    session_id: String,
    relevance: String,
    reason: String,
}

#[tokio::test]
async fn wrapped_rankings_parse_without_a_repair_completion() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::clone(&stop);
    let server = std::thread::spawn(move || {
        let mut requests = 0;
        while !done.load(Ordering::Relaxed) {
            let (mut stream, _) = match listener.accept() {
                Ok(stream) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("fixture listener failed: {error}"),
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    break offset + 4;
                }
            };
            let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            while bytes.len() < header_end + length {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
            }
            let request: Value =
                serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
            assert_eq!(request["model"], "fixture-model");
            requests += 1;
            let response = json!({
                "id":"fixture-completion", "object":"chat.completion", "created":0, "model":"fixture-model",
                "choices":[{"index":0, "finish_reason":"stop", "message":{"role":"assistant", "content":
                    "Here are the rankings: [{\"session_id\":\"a\",\"relevance\":\"high\",\"reason\":\"fixture match\"}]"}}],
                "usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}
            }).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        }
        requests
    });
    let client = LlmClient::with_config(url, "fixture-placeholder", "fixture-model");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        client.chat_json_parsed::<Vec<Ranking>>(vec![
            ChatMessage::system("Return a JSON array with session_id, relevance, and reason."),
            ChatMessage::user("Rank the fixture session."),
        ]),
    )
    .await;
    stop.store(true, Ordering::Relaxed);
    let requests = server.join().unwrap();
    let ranking = result
        .expect("the loopback completion fixture must finish within its bound")
        .expect("a valid wrapped object array must parse on its first completion");
    assert_eq!(requests, 1, "no automatic repair completion is needed");
    assert_eq!(ranking.len(), 1);
    assert_eq!(ranking[0].session_id, "a");
    assert_eq!(ranking[0].relevance, "high");
    assert_eq!(ranking[0].reason, "fixture match");
}
