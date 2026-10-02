//! End-to-end checks that the daemon binary emits application logs on its
//! lifecycle paths (regression coverage for the zero-log daemon reported in
//! issue #3131) and honours `RUST_LOG` filtering.

#![cfg(all(feature = "mock-backend", not(feature = "actplane")))]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Runs the real daemon binary until it binds its socket, stops it with
/// SIGINT, and returns everything written to stderr.
fn run_daemon(rust_log: Option<&str>) -> String {
    let socket_path =
        std::env::temp_dir().join(format!("enforcer-log-test-{}.sock", uuid::Uuid::new_v4()));
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentsight-enforcer"));
    command
        .env("AGENTSIGHT_ENFORCER_SOCKET", &socket_path)
        .stderr(Stdio::piped());
    if let Some(spec) = rust_log {
        command.env("RUST_LOG", spec);
    } else {
        command.env_remove("RUST_LOG");
    }
    let child = command.spawn().expect("enforcer binary should spawn");

    wait_for_socket(&socket_path);
    // Graceful stop: SIGINT drives the ctrlc handler out of serve_until.
    // SAFETY: `kill` only delivers a signal to the spawned child pid; no
    // memory is dereferenced.
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let output = child
        .wait_with_output()
        .expect("enforcer should exit after SIGINT");
    let _ = std::fs::remove_file(&socket_path);
    String::from_utf8(output.stderr).expect("daemon logs should be UTF-8")
}

fn wait_for_socket(socket_path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket_path.exists() {
        assert!(
            Instant::now() < deadline,
            "enforcer did not bind {socket_path:?} within 10s"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn daemon_logs_startup_and_graceful_stop() {
    let stderr = run_daemon(None);
    assert!(
        stderr.contains("mock backend"),
        "mock warning must be logged, got: {stderr:?}"
    );
    assert!(
        stderr.contains("listening on"),
        "startup bind must be logged, got: {stderr:?}"
    );
    assert!(
        stderr.contains("stopped"),
        "graceful shutdown must be logged, got: {stderr:?}"
    );
}

#[test]
fn daemon_honours_rust_log_filter() {
    let stderr = run_daemon(Some("error"));
    assert!(
        stderr.is_empty(),
        "error-level filter must silence warn/info lifecycle logs, got: {stderr:?}"
    );
}

#[test]
fn daemon_falls_back_to_info_on_invalid_rust_log() {
    let stderr = run_daemon(Some("listening=not-a-level"));
    assert!(
        stderr.contains("invalid RUST_LOG"),
        "invalid spec must be reported on stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("listening on"),
        "fallback to info must keep lifecycle logs, got: {stderr:?}"
    );
}

#[test]
fn daemon_honours_rust_log_message_regex() {
    let stderr = run_daemon(Some("info/listening"));
    assert!(stderr.contains("listening on"), "got: {stderr:?}");
    assert!(
        !stderr.contains("mock backend") && !stderr.contains("stopped"),
        "message regex must suppress non-matching records, got: {stderr:?}"
    );
}
