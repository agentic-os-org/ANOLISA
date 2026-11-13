//! Valid wrapped JSON must not trigger paid repair requests for trailing prose.
use agentsight_opt::llm::{ChatMessage, LlmClient};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn request_fixture(content: &str) -> (anyhow::Result<Value>, usize) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let body = json!({"id":"fixture","object":"chat.completion","created":0,"model":"fixture",
        "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":content}}],
        "usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}).to_string();
    let requests = Arc::new(AtomicUsize::new(0));
    let received = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        // Keep accepting repair requests so the baseline fails on its response,
        // not on a connection error or an artificial test timeout.
        loop {
            let mut request = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let read = stream.read(&mut buf).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..read]);
                if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..header_end]).to_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse()
                        .unwrap();
                    if request.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            let count = received.fetch_add(1, Ordering::SeqCst) + 1;
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
            if count == 3 {
                break;
            }
            let (next, _) = listener.accept().await.unwrap();
            stream = next;
        }
    });
    let client = LlmClient::with_config(url, "fixture", "fixture");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        client.chat_json_parsed::<Value>(vec![ChatMessage::user("Return the result as JSON")]),
    )
    .await;
    server.abort();
    (result.unwrap(), requests.load(Ordering::SeqCst))
}

async fn parse_once(content: &str) -> Value {
    let (result, requests) = request_fixture(content).await;
    assert_eq!(requests, 1, "valid output needs no paid repair request");
    result.expect("the complete first JSON document must parse")
}

#[tokio::test]
async fn trailing_prose_braces_do_not_extend_an_object() {
    assert_eq!(
        parse_once(
            "Result: {\"ok\":true,\"text\":\"quoted } and \\\" [\"}\nThe schema uses {braces}."
        )
        .await,
        json!({"ok":true,"text":"quoted } and \" ["})
    );
}

#[tokio::test]
async fn trailing_brackets_do_not_turn_an_array_into_an_inner_object() {
    assert_eq!(
        parse_once("Result: [{\"id\":1},{\"id\":2}]\nReferences: [1, 2].").await,
        json!([{"id":1},{"id":2}])
    );
}

#[tokio::test]
async fn incidental_bracketed_prose_does_not_hide_the_following_document() {
    assert_eq!(
        parse_once("[Analysis] {\"ok\":true}\nReferences: [1].").await,
        json!({"ok":true})
    );
}

#[tokio::test]
async fn fenced_nested_json_keeps_its_whole_structure() {
    assert_eq!(
        parse_once("```json\n{\"items\":[{\"value\":\"a}b[c\"}]}\n```").await,
        json!({"items":[{"value":"a}b[c"}]})
    );
}

#[tokio::test]
async fn incomplete_roots_keep_the_repair_path_instead_of_promoting_inner_values() {
    for raw in [r#"{"items":[{"id":1}]"#, r#"[{"id":1}"#] {
        let (result, requests) = request_fixture(raw).await;
        assert!(
            result.is_err(),
            "an incomplete outer document must not become an inner value"
        );
        assert_eq!(
            requests, 3,
            "malformed output still uses the existing repair attempts"
        );
    }
}
