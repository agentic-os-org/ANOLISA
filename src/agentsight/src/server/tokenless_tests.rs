use super::*;
use std::io::Write;
use std::net::TcpListener;

#[test]
fn client_pagination_authentication_and_config_boundaries() {
    let directory = std::env::temp_dir().join(format!("tokenless-client-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let credential = directory.join("credential");
    std::fs::write(&credential, "fixture-token\n").unwrap();
    let configuration = directory.join("agentsight.json");
    let absent = QueryClient::from_config_path(Some(&configuration)).unwrap();
    assert!(absent.is_none());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::write(
        &configuration,
        json!({"tokenless_api":{"url":format!("http://{address}"),"credential_file":credential}})
            .to_string(),
    )
    .unwrap();
    let worker = std::thread::spawn(move || {
        for (id, next) in [(2, Some(2)), (1, None)] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let request = String::from_utf8(headers).unwrap();
            let lower = request.to_ascii_lowercase();
            assert!(lower.contains("authorization: bearer fixture-token"));
            assert!(request.contains("session_id=fixture"));
            if id == 1 {
                assert!(request.contains("before_id=2"));
            }
            let body = json!({"schema_version":1,"records":[{"id":id,"content_included":false}],"next_before_id":next}).to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    let client = QueryClient::from_config_path(Some(&configuration))
        .unwrap()
        .unwrap();
    let rows = client
        .fetch(&Filter {
            session_id: Some("fixture".into()),
            ..Default::default()
        })
        .unwrap()
        .0;
    assert_eq!(
        rows.iter()
            .map(|row| row["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
    worker.join().unwrap();
    assert!(matches!(
        client.fetch(&Filter {
            session_id: Some(String::new()),
            ..Default::default()
        }),
        Err(QueryError::Invalid)
    ));
    std::fs::write(&configuration, "{}").unwrap();
    let absent = QueryClient::from_config_path(Some(&configuration)).unwrap();
    assert!(absent.is_none());
    std::fs::write(&configuration,json!({"tokenless_api":{"url":"http://user:password@localhost","credential_file":credential}}).to_string()).unwrap();
    assert!(matches!(
        QueryClient::from_config_path(Some(&configuration)),
        Err(QueryError::Invalid)
    ));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn tool_queries_deduplicate_ids_and_share_a_budget() {
    let directory = std::env::temp_dir().join(format!("tokenless-tools-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let credential = directory.join("credential");
    std::fs::write(&credential, "fixture-token").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let configuration = directory.join("config.json");
    std::fs::write(
        &configuration,
        json!({"tokenless_api": {
            "url": format!("http://{}", listener.local_addr().unwrap()),
            "credential_file": credential
        }})
        .to_string(),
    )
    .unwrap();
    let worker = std::thread::spawn(move || {
        let mut seen = BTreeSet::new();
        let mut pages = 0;
        while seen.len() < 1004 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            assert!(headers.starts_with("post /v1/stats/records/query "));
            assert!(headers.contains("authorization: bearer fixture-token"));
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            let query: Value = serde_json::from_slice(&body).unwrap();
            let batch = query["tool_use_ids"].as_array().unwrap();
            assert!(batch.len() <= 128);
            let mut records = Vec::new();
            let mut next = None;
            for tool in batch {
                let tool = tool.as_str().unwrap();
                if tool == "0000" {
                    if query["before_id"].is_null() {
                        records.extend([
                            json!({"id":3,"tool_use_id":tool,"content_included":false}),
                            json!({"id":2,"tool_use_id":tool,"content_included":false}),
                        ]);
                        next = Some(2);
                    } else {
                        assert_eq!(query["before_id"], 2);
                        records.push(json!({"id":1,"tool_use_id":tool,"content_included":false}));
                    }
                } else if tool == "a" || tool == "b" {
                    records.push(json!({"id":if tool == "a" {5} else {4},"tool_use_id":tool,"content_included":false}));
                }
            }
            if next.is_none() {
                seen.extend(batch.iter().map(|id| id.as_str().unwrap().to_owned()));
            }
            pages += 1;
            let body =
                json!({"schema_version":1,"records":records,"next_before_id":next}).to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        assert_eq!(pages, 9);
    });
    let client = QueryClient::from_config_path(Some(&configuration))
        .unwrap()
        .unwrap();
    let mut ids: Vec<String> = (0..1002).map(|n| format!("{n:04}")).collect();
    ids.extend(["b".to_owned(), "a".to_owned(), "a".to_owned()]);
    assert_eq!(
        client
            .fetch_tools(&ids.iter().map(String::as_str).collect::<Vec<_>>())
            .unwrap()
            .len(),
        5
    );
    worker.join().unwrap();
    assert!(client.fetch_tools(&[]).unwrap().is_empty());
    for id in ["".to_owned(), "x".repeat(1025)] {
        assert!(matches!(
            client.fetch_tools(&[&id]),
            Err(QueryError::Invalid)
        ));
    }
    let mut budget = QueryBudget {
        started: Instant::now() - Duration::from_secs(6),
        ..Default::default()
    };
    assert!(matches!(
        client.fetch_bounded(&Filter::default(), &mut budget, None),
        Err(QueryError::Endpoint)
    ));
    std::fs::remove_dir_all(directory).unwrap();
}
