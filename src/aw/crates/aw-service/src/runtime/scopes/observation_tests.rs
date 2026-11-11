//! Storage failure after committed acknowledgement must retain a cleanup owner.

use super::*;
use std::io::{Read, Write};

#[test]
fn acknowledged_scopes_record_distinct_starts_without_optional_observation() {
    let fixture = Fixture::new();
    let first = fixture.task(std::process::id(), std::process::id());
    let second = fixture.task(std::process::id(), std::process::id());
    for task in [&first, &second] {
        fixture.acknowledge(task).unwrap();
        assert!(fixture.acknowledge(task).is_err());
    }
    assert_eq!(
        fixture.call(Operation::Status).unwrap()["observations"],
        json!([])
    );
    let mut started = fs::read_dir(fixture.directory.path().join("audit"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .flat_map(|path| {
            fs::read_to_string(path)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .map(|line| serde_json::from_str::<Value>(&line).unwrap())
        .filter_map(|record| {
            let event = &record["record"]["plan"]["executionEvent"];
            (event["data"]["name"] == "instance.started").then(|| {
                event["data"]["subject"]["instanceId"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
        })
        .collect::<Vec<_>>();
    started.sort();
    let mut expected = vec![first.instance_id, second.instance_id];
    expected.sort();
    assert_eq!(started, expected);
}

#[test]
fn observation_start_failure_cancels_acknowledged_scope_and_drains_resources() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut fixture = Fixture::new();
    let runtime = Arc::get_mut(&mut fixture.runtime).unwrap();
    let mut document: Value = serde_json::from_slice(&runtime.bytes).unwrap();
    document["spec"]["agents"]["qoder"]["evidence"] = json!({
        "source_id":"fixture", "endpoint":format!("http://{address}"), "query_timeout_ms":1000,
        "query_bytes":1048576, "max_items":32, "max_bytes":262144,
    });
    runtime.bytes = serde_json::to_vec(&document).unwrap();
    let configuration = aw_config::Validator::new()
        .unwrap()
        .parse(&runtime.bytes)
        .unwrap();
    runtime.contexts = evidence::Contexts::new(&configuration).unwrap();
    let task = fixture.task(std::process::id(), std::process::id());
    let (entered, ready) = mpsc::sync_channel(1);
    let (resume, proceed) = mpsc::sync_channel(1);
    thread::scope(|threads| {
        let probe = threads.spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(TIMEOUT)).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            entered.send(()).unwrap();
            proceed.recv_timeout(TIMEOUT).unwrap();
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 21\r\nConnection: close\r\n\r\n{\"auth_enabled\":true}");
        });
        let pending = threads.spawn(|| fixture.acknowledge(&task));
        ready.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(fixture.acknowledgements(), 1);
        let path = fixture.directory.path();
        fs::rename(path.join("audit"), path.join("audit-original")).unwrap();
        fs::write(path.join("audit"), "unavailable").unwrap();
        assert!(fixture
            .runtime
            .audit
            .claim(&"0".repeat(64), json!({}))
            .is_err());
        resume.send(()).unwrap();
        assert!(
            matches!(pending.join().unwrap(), Err(Error::Rejected(code)) if code == "audit_unavailable")
        );
        probe.join().unwrap();
    });
    assert!(
        matches!(fixture.runtime.check_scope_owner(&task.instance_id),
        Err(Error::Rejected(code)) if code == "scope_closing")
    );
    assert!(fixture.runtime.maintain(false).is_err());
    let state = fixture.call(Operation::Status).unwrap();
    assert_eq!(state["bindings"], 1);
    assert_eq!(state["observations"], json!([]));
}
