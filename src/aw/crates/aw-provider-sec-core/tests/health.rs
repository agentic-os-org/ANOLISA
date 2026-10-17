#![cfg(target_os = "linux")]

use std::{path::Path, time::Duration};

use aw_provider_sec_core::health::query;
use serde_json::{json, Value};

#[path = "support/fixture.rs"]
mod support;

fn checked(value: Value) -> Value {
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/health-v1alpha1.schema.json")).unwrap();
    assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&value));
    value
}

#[test]
fn healthy_probe_is_not_admission_or_a_scan_verdict() {
    let directory = support::Directory::new(json!({"record_health_environment":true,
        "stderr":"private diagnostic", "stdout":{"verdict":"deny"}}));
    let result = checked(
        query(
            &directory.cli(),
            Path::new("/run/private.sock"),
            Duration::from_secs(1),
        )
        .unwrap(),
    );
    assert_eq!(result["backend"], "authenticated_reachable");
    assert_eq!(result["admission"], "unknown");
    assert_eq!(result["check_result"], "unknown");
    assert_eq!(result["code"], Value::Null);
    assert_eq!(directory.argv(), json!([]));
    let environment: Value =
        serde_json::from_slice(&std::fs::read(directory.0.join("environment.json")).unwrap())
            .unwrap();
    assert_eq!(environment["AGENT_SEC_DAEMON_SOCKET"], "/run/private.sock");
    assert_eq!(environment["AGENT_SEC_HEALTH_TIMEOUT_SECONDS"], "1");
    assert_eq!(
        environment["AGENT_SEC_TEST_SKIP_SERVER_UID_CHECK"],
        Value::Null
    );
    assert!(!result.to_string().contains("private diagnostic"));
    assert!(!result.to_string().contains("deny"));
    directory.assert_stopped();
}

#[test]
fn missing_failed_oversized_and_timed_out_probes_never_report_healthy() {
    let socket = Path::new("/run/private.sock");
    let result =
        checked(query(Path::new("/missing/helper"), socket, Duration::from_secs(1)).unwrap());
    assert_eq!(result["helper_installed"], false);
    assert_eq!(result["code"], "component_missing");
    for (case, code) in [
        (
            json!({"exit":1,"stdout":{"status":"ok"},"stderr":"expected daemon UID 0"}),
            "probe_failed",
        ),
        (json!({"stdout_repeat":8192}), "probe_transport_error"),
        (json!({"sleep":30}), "deadline_exceeded"),
    ] {
        let directory = support::Directory::new(case);
        let result = checked(query(&directory.cli(), socket, Duration::from_millis(200)).unwrap());
        assert_eq!(result["backend"], "unavailable");
        assert_eq!(result["code"], code);
        directory.assert_stopped();
    }
}

#[test]
fn invalid_arguments_do_not_execute_a_probe() {
    let directory = support::Directory::new(json!({}));
    assert!(query(
        &directory.cli(),
        Path::new("relative.sock"),
        Duration::from_secs(1)
    )
    .is_err());
    assert!(query(&directory.cli(), Path::new("/run/socket"), Duration::ZERO).is_err());
    assert!(!directory.0.join("cli.pid").exists());
}

#[test]
fn explicit_cli_health_mode_needs_no_event_or_stdin() {
    let directory = support::Directory::new(json!({}));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aw-provider-sec-core"))
        .args(["--health", "--helper"])
        .arg(directory.cli())
        .args(["--socket", "/run/health.sock"])
        .env("AGENT_SEC_TEST_SKIP_SERVER_UID_CHECK", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    let result = checked(serde_json::from_slice(&output.stdout).unwrap());
    assert_eq!(result["backend"], "authenticated_reachable");
    directory.assert_stopped();
}

#[test]
fn published_fixtures_satisfy_the_independent_health_schema() {
    for fixture in [
        include_str!("fixtures/health-reachable.json"),
        include_str!("fixtures/health-unavailable.json"),
    ] {
        checked(serde_json::from_str(fixture).unwrap());
    }
}

#[test]
#[ignore = "requires AW_SEC_HEALTH_TEST_HELPER pointing to the real public helper"]
fn real_public_helper_authenticates_peer_and_only_sends_health() {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        os::unix::net::UnixListener,
        time::Instant,
    };
    let helper =
        std::env::var_os("AW_SEC_HEALTH_TEST_HELPER").expect("set AW_SEC_HEALTH_TEST_HELPER");
    let directory = support::Directory::new(json!({}));
    // Keep the socket path within sockaddr_un regardless of checkout length.
    let socket = Path::new("/tmp").join(format!("aw-sec-health-{}.sock", std::process::id()));
    struct SocketCleanup(std::path::PathBuf);
    impl Drop for SocketCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let listener = UnixListener::bind(&socket).unwrap();
    let _cleanup = SocketCleanup(socket.clone());
    listener.set_nonblocking(true).unwrap();
    // SAFETY: geteuid only reads this test process's effective identity.
    let root = unsafe { libc::geteuid() } == 0;
    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(3);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "helper did not connect");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            if root {
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                assert_eq!(
                    serde_json::from_str::<Value>(&request).unwrap(),
                    json!({"method":"daemon.health","params":{}})
                );
                reader
                    .get_mut()
                    .write_all(b"{\"requestId\":\"health\",\"result\":{\"status\":\"ok\"}}\n")
                    .unwrap();
            } else {
                assert_eq!(
                    reader.read(&mut [0; 1]).unwrap(),
                    0,
                    "non-root peer received a request"
                );
            }
        });
        let result = checked(query(Path::new(&helper), &socket, Duration::from_secs(1)).unwrap());
        assert_eq!(
            result["backend"],
            if root {
                "authenticated_reachable"
            } else {
                "unavailable"
            }
        );
        server.join().unwrap();
    });
    assert!(!directory.0.join("cli.pid").exists());
}
