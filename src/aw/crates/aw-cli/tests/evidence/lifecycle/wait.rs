//! Phase-aware waits keep fixture scheduling separate from product deadlines.

use super::launch::{Fixture, Process, Service, TIMEOUT};
use aw_service::Operation;
use serde_json::{json, Value};

pub fn ready(process: &mut Process, fixture: &Fixture, id: &str) {
    process
        .wait_until(&format!("Agent {id} callbacks complete"), TIMEOUT, |_| {
            let exists = |name: &str| fixture.root.join(name).exists();
            let ready = exists(&format!("ready-{id}"));
            Ok((
                ready,
                json!({
                    "agent_started": exists(&format!("started-{id}")),
                    "before_callback": exists(&format!("payload-{id}-PreToolUse.bin")),
                    "after_callback": exists(&format!("payload-{id}-PostToolUse.bin")),
                    "callbacks_complete": ready,
                })
                .to_string(),
            ))
        })
        .unwrap_or_else(|error| panic!("{error}"));
}

pub fn observation(
    process: &mut Process,
    service: &Service,
    expected: &str,
    matches: impl Fn(&Value) -> bool,
) {
    process
        .wait_until(&format!("observation {expected}"), TIMEOUT, |deadline| {
            let state = service
                .client
                .call(Operation::Status, deadline)
                .map_err(|error| error.to_string())?;
            Ok((matches(&state), state.to_string()))
        })
        .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn premature_exit_reports_phase_status_and_output() {
    let fixture = Fixture::new();
    let mut command = std::process::Command::new(&fixture.python);
    command.args([
        "-c",
        "import sys; print('fixture output'); print('fixture error', file=sys.stderr); sys.exit(7)",
    ]);
    let mut process = Process::spawn(&fixture, command);
    let error = process
        .wait_until("callbacks complete", TIMEOUT, |_| {
            Ok((false, String::from("no ready marker")))
        })
        .unwrap_err();
    for expected in [
        "callbacks complete",
        "child exited: exit status: 7",
        "fixture output",
        "fixture error",
        "process:",
    ] {
        assert!(error.contains(expected), "{error}");
    }
    assert_eq!(process.finish().status.code(), Some(7));
}

#[test]
fn stalled_phase_reports_last_state_and_leaves_cleanup_owned() {
    let fixture = Fixture::new();
    let mut command = std::process::Command::new(&fixture.python);
    command.args(["-c", "import time; time.sleep(60)"]);
    let mut process = Process::spawn(&fixture, command);
    let error = process
        .wait_until(
            "observation available",
            std::time::Duration::from_secs(1),
            |_| Ok((false, String::from("source_state=pending"))),
        )
        .unwrap_err();
    for expected in [
        "observation available",
        "deadline exceeded",
        "source_state=pending",
    ] {
        assert!(error.contains(expected), "{error}");
    }
    process.signal(libc::SIGTERM);
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(process.finish().status.signal(), Some(libc::SIGTERM));
}

#[test]
fn held_agent_waits_for_release_beyond_the_old_five_second_limit() {
    let fixture = Fixture::new();
    let service = Service::start(&fixture, &fixture.document());
    let mut process = Process::spawn(&fixture, fixture.launch(&["--hold"], None));
    ready(&mut process, &fixture, "one");
    let started = std::time::Instant::now();
    process
        .wait_until("delayed test release", TIMEOUT, |_| {
            Ok((
                started.elapsed() > std::time::Duration::from_secs(6),
                String::from("held"),
            ))
        })
        .unwrap();
    std::fs::write(fixture.root.join("release-one"), "exit").unwrap();
    let output = process.finish();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    service.released(&fixture);
}

#[test]
fn missing_release_still_fails_with_the_marker_name() {
    let fixture = Fixture::new();
    let service = Service::start(&fixture, &fixture.document());
    let mut command = fixture.launch(&["--hold"], None);
    command.env("FAKE_HOLD_TIMEOUT_SECONDS", "1");
    let output = fixture.run(command);
    assert_eq!(output.status.code(), Some(91));
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("fixture timed out waiting for release-one"));
    service.released(&fixture);
}
