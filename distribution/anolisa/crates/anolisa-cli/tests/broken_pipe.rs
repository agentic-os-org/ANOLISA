//! Subprocess regression coverage for stdout consumers and failures.

#![cfg(unix)]

use std::io::{self, Write as _};
use std::net::Shutdown;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::Stdio;

#[cfg(target_os = "linux")]
use std::fs::OpenOptions;

mod common;

fn disconnected_stdout() -> Stdio {
    let (reader, mut writer) = UnixStream::pair().expect("stdout socket pair must be created");
    reader
        .shutdown(Shutdown::Both)
        .expect("stdout peer must shut down before spawning the CLI");
    drop(reader);
    let error = writer
        .write_all(b"disconnection probe")
        .expect_err("stdout peer must be disconnected before spawning the CLI");
    assert_eq!(io::ErrorKind::BrokenPipe, error.kind());
    Stdio::from(OwnedFd::from(writer))
}

fn assert_graceful_closed_stdout(arguments: &[&str], expected_code: i32) {
    let output = common::run_with_stdout(arguments, disconnected_stdout());
    let diagnostic = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        Some(expected_code),
        output.status.code(),
        "stderr: {diagnostic}"
    );
    assert!(output.stdout.is_empty());
    assert!(!diagnostic.contains("panicked at"), "stderr: {diagnostic}");
}

#[test]
fn normal_human_and_json_env_output_remains_valid() {
    let human = common::run(&["env", "--no-color"]);
    assert_eq!(Some(0), human.status.code());
    assert!(String::from_utf8_lossy(&human.stdout).contains("os:"));
    assert!(human.stderr.is_empty());

    let json = common::run(&["env", "--json", "--no-color"]);
    assert_eq!(Some(0), json.status.code());
    assert!(json.stderr.is_empty());
    let envelope: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("env output must be valid JSON");
    assert_eq!(Some(true), envelope["ok"].as_bool());
    assert_eq!(Some("env"), envelope["command"].as_str());
}

#[test]
fn successful_stdout_paths_survive_an_immediately_closed_reader() {
    for arguments in [
        &["env", "--no-color"][..],
        &["env", "--json", "--no-color"][..],
        &["--help"][..],
        &["--version"][..],
    ] {
        assert_graceful_closed_stdout(arguments, 0);
    }
}

#[test]
fn json_business_failure_keeps_exit_code_when_stdout_reader_closes() {
    // A targetless update fails before repository or state discovery, keeping
    // this stdout regression independent of the invoking user's HOME.
    assert_graceful_closed_stdout(&["--json", "update"], 2);
}

#[cfg(target_os = "linux")]
#[test]
fn stdout_device_failure_overrides_every_success_output_path() {
    if !std::path::Path::new("/dev/full").exists() {
        return;
    }

    for arguments in [
        &["env", "--no-color"][..],
        &["env", "--json", "--no-color"][..],
        &["--help"][..],
        &["--version"][..],
    ] {
        let full = OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .expect("/dev/full must be writable");
        let output = common::run_with_stdout(arguments, Stdio::from(full));
        let diagnostic = String::from_utf8_lossy(&output.stderr);

        assert_eq!(Some(1), output.status.code(), "stderr: {diagnostic}");
        assert!(
            diagnostic.contains("error: failed writing to stdout:"),
            "stderr: {diagnostic}"
        );
    }
}
