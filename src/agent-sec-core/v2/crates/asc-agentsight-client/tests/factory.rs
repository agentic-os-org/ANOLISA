use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use asc_agentsight_client::AgentSightClientFactory;
use asc_policy_target_contracts::TargetDeploymentClientFactory;
use asc_policy_types::target::{Failure, FailureKind, PreparedApply, Presence};

struct Directory(std::path::PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn factory_defers_credentials_and_refreshes_them_between_attempts() {
    let directory =
        std::env::temp_dir().join(format!("asc-client-factory-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let directory = Directory(directory);
    let token_file = directory.0.join("token");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let factory = AgentSightClientFactory::new(
        format!("http://{}/api", listener.local_addr().unwrap()),
        &token_file,
    );
    assert_eq!(
        factory.open().err(),
        Some(Failure::new(
            FailureKind::Retryable,
            "AGENTSIGHT_CREDENTIAL_UNAVAILABLE"
        ))
    );
    std::fs::write(&token_file, "\n").unwrap();
    assert_eq!(
        factory.open().err(),
        Some(Failure::new(
            FailureKind::Retryable,
            "AGENTSIGHT_INVALID_CREDENTIAL"
        ))
    );
    std::fs::write(&token_file, "first-token\n").unwrap();
    let first = factory.open().unwrap();
    std::fs::write(&token_file, "second-token\n").unwrap();
    let second = factory.open().unwrap();
    let prepared: PreparedApply = serde_json::from_str(include_str!(
        "../../../fixtures/clients/agentsight/file-deletion/prepared-7.json"
    ))
    .unwrap();
    let target = prepared.target;
    let expected_path = format!(
        "DELETE /api/enforcement/bindings/{} HTTP/1.1\r\n",
        target.id
    );
    let server = std::thread::spawn(move || {
        for token in ["first-token", "second-token"] {
            let deadline = Instant::now() + Duration::from_secs(5);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    other => panic!("missing deployment request: {other:?}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, expected_path);
            let mut authorization = None;
            loop {
                line.clear();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.trim().split_once(':').unwrap();
                if name.eq_ignore_ascii_case("authorization") {
                    authorization = Some(value.trim().to_owned());
                }
            }
            assert_eq!(authorization, Some(format!("Bearer {token}")));
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        }
    });
    for client in [first, second] {
        let report = client.delete(std::slice::from_ref(&target));
        assert_eq!(report.error, None);
        assert_eq!(
            report.observations,
            vec![asc_policy_types::target::Observation {
                target: target.clone(),
                presence: Presence::Absent
            }]
        );
    }
    server.join().unwrap();
}
