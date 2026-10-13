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
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
    worker.join().unwrap();
    assert!(matches!(
        client.fetch(&Filter::default()),
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
