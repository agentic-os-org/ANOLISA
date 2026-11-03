//! Native probe process behavior over real Unix sockets.

use std::{
    io::{BufRead as _, BufReader, Read as _, Write as _},
    os::unix::net::UnixListener,
    path::Path,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

use serde_json::json;

mod common;

fn probe(socket: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-sec-daemon-health"));
    command.env("AGENT_SEC_DAEMON_SOCKET", socket);
    command.env_remove("AGENT_SEC_HEALTH_TIMEOUT_SECONDS");
    command
}

fn exchange(response: &[u8]) -> Output {
    let directory = common::Directory::new();
    let path = directory.0.join("daemon.sock");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    thread::scope(|scope| {
        let server = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(3);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "probe did not connect");
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut stream = BufReader::new(stream);
            let mut request = String::new();
            stream.read_line(&mut request).unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&request).unwrap(),
                json!({"method":"daemon.health","params":{}})
            );
            stream.get_mut().write_all(response).unwrap();
        });
        let output = probe(&path).output().unwrap();
        server.join().unwrap();
        output
    })
}

#[test]
fn healthy_daemon_is_silent_and_unhealthy_or_invalid_responses_fail() {
    let output = exchange(b"{\"requestId\":\"health\",\"result\":{\"status\":\"ok\"}}\n");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    for response in [
        b"{\"requestId\":\"health\",\"result\":{\"status\":\"stopping\"}}\n".as_slice(),
        b"{\"requestId\":\"health\",\"result\":{}}\n",
        b"not json\n",
    ] {
        let output = exchange(response);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn help_obsolete_options_missing_socket_and_invalid_timeout() {
    let directory = common::Directory::new();
    let socket = directory.0.join("missing.sock");
    let output = probe(&socket).arg("--help").output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("daemon.health"));
    for option in ["--require-model", "--require-same-cgroup"] {
        let output = probe(&socket).arg(option).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("unknown argument"));
    }
    let output = probe(&socket).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains(socket.to_str().unwrap()));
    for timeout in ["0", "-1", "NaN", "inf", "invalid"] {
        let output = probe(&socket)
            .env("AGENT_SEC_HEALTH_TIMEOUT_SECONDS", timeout)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("positive finite duration"));
    }
}

#[test]
fn nonroot_server_is_rejected_before_health_request_is_sent() {
    let uid = rustix::process::geteuid().as_raw();
    if uid == 0 {
        // Installed E2E covers a root probe against a different-UID fake daemon.
        return;
    }
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let output = probe(&socket)
        .env_remove("AGENT_SEC_TEST_SKIP_SERVER_UID_CHECK")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains(&format!("expected daemon UID 0, got {uid}"))
    );
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
}
