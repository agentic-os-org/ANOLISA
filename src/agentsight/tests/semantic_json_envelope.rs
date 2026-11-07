use agentsight::semantic_search::{
    SemanticSearchCandidate, SemanticSearchRequest, handle_semantic_search,
};
use agentsight_opt::LlmClient;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

async fn search_with_answer(answer: Value) -> (Value, Vec<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::clone(&stop);
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        while !done.load(Ordering::Relaxed) {
            let (mut stream, _) = match listener.accept() {
                Ok(stream) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(e) => panic!("fixture listener: {e}"),
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(offset) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
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
            requests.push(serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap());
            let response = json!({
                "id":"fixture-completion", "object":"chat.completion", "created":0, "model":"fixture-model",
                "choices":[{"index":0, "finish_reason":"stop", "message":{"role":"assistant", "content":answer.to_string()}}],
                "usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}
            }).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        }
        requests
    });
    let client = LlmClient::with_config(url, "fixture-placeholder", "fixture-model");
    let request = SemanticSearchRequest {
        query: "find fixture work".into(),
        candidates: (1..=6)
            .map(|n| SemanticSearchCandidate {
                session_id: format!("s{n}"),
                first_message: Some("fixture work".into()),
                last_message: None,
                project: None,
            })
            .collect(),
    };
    let response = handle_semantic_search(&client, &request, Duration::from_secs(10)).await;
    stop.store(true, Ordering::Relaxed);
    let requests = server.join().unwrap();
    assert!(response.status().is_success());
    let body = actix_web::body::to_bytes(response.into_body())
        .await
        .unwrap();
    (serde_json::from_slice(&body).unwrap(), requests)
}

#[actix_web::test]
async fn object_rankings_survive_json_object_mode_without_repair() {
    let entries = json!([
        {"session_id":"s1", "relevance":"medium", "reason":"first match"},
        {"session_id":"s2", "relevance":"high", "reason":"best match"},
        {"session_id":"unknown", "relevance":"high", "reason":"not a candidate"},
        {"session_id":"s1", "relevance":"high", "reason":"duplicate"}
    ]);
    let (body, requests) = search_with_answer(json!({"results":entries})).await;
    assert_eq!(body["results"].as_array().unwrap().len(), 2);
    assert_eq!(body["results"][0]["session_id"], "s2");
    assert_eq!(body["results"][1]["reason"], "first match");
    assert_eq!(requests.len(), 1, "a valid ranking needs no repair request");
    assert_eq!(requests[0]["response_format"]["type"], "json_object");
    let prompt = requests[0]["messages"].to_string();
    assert!(
        prompt.contains("results"),
        "the prompt must request the same envelope"
    );
}

#[actix_web::test]
async fn empty_object_rankings_do_not_spend_repair_requests() {
    let (body, requests) = search_with_answer(json!({"results":[]})).await;
    assert_eq!(body, json!({"results":[]}));
    assert_eq!(requests.len(), 1);
}

#[actix_web::test]
async fn legacy_array_rankings_remain_accepted() {
    let (body, requests) = search_with_answer(json!([
        {"session_id":"s3", "relevance":"high", "reason":"legacy provider"}
    ]))
    .await;
    assert_eq!(body["results"][0]["session_id"], "s3");
    assert_eq!(requests.len(), 1);
}
