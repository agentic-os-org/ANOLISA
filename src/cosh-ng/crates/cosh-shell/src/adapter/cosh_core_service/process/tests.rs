use std::io::Cursor;
use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::types::{AgentEvent, CoshApprovalMode};

use nix::libc;

use super::super::super::cosh_core::{
    begin_session_attempt, SessionResumeAttempt, SessionRuntimeState,
};
use super::super::super::{AgentRunHandle, AgentRunPoll, RegistryQueryError};
use super::super::command::RegistryCommand;
use super::super::registry::{is_registry_response_for, log_registry_transport_failure};
use super::super::PersistentCoshCoreRuntime;
use super::{read_stdout_lines, terminate_marked, user_message_with_raw_input};

/// Serialize tests that start real child processes so a test-only spawn delay
/// does not bleed into concurrent runs.
static SPAWN_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn spawn_test_guard() -> std::sync::MutexGuard<'static, ()> {
    SPAWN_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn drain_until_cancelled_or_deadline(handle: &AgentRunHandle, deadline: Instant) -> bool {
    let mut saw_cancelled = false;
    loop {
        match handle.poll_event_timeout(Duration::from_millis(100)) {
            Ok(AgentRunPoll::Event(AgentEvent::AgentCancelled { .. })) => {
                saw_cancelled = true;
            }
            Ok(AgentRunPoll::Finished) => break,
            Ok(AgentRunPoll::Timeout) if Instant::now() < deadline => continue,
            Ok(AgentRunPoll::Timeout) => break,
            Ok(AgentRunPoll::Event(event)) => {
                eprintln!("got event: {event:?}");
            }
            Err(err) => {
                eprintln!("got error: {err:?}");
                break;
            }
        }
    }
    saw_cancelled
}

#[test]
fn registry_response_requires_discriminator_and_correlation_id() {
    let response = serde_json::json!({
        "type": "registry_response",
        "request_id": "reg-1",
        "success": true,
    });
    assert!(is_registry_response_for(&response, "reg-1"));

    let future_output = serde_json::json!({
        "type": "future_output",
        "request_id": "reg-1",
    });
    assert!(!is_registry_response_for(&future_output, "reg-1"));

    let other_request = serde_json::json!({
        "type": "registry_response",
        "request_id": "reg-2",
    });
    assert!(!is_registry_response_for(&other_request, "reg-1"));
}

#[test]
fn user_message_omits_raw_input_for_legacy_payloads() {
    let with_raw = user_message_with_raw_input("envelope", Some("raw"), Some("session-1"), "/tmp");
    let value: serde_json::Value = serde_json::from_str(&with_raw).unwrap();
    assert_eq!(value["message"]["content"], "envelope");
    assert_eq!(value["message"]["raw_user_input"], "raw");

    let without_raw = user_message_with_raw_input("legacy", None, None, "/tmp");
    let value: serde_json::Value = serde_json::from_str(&without_raw).unwrap();
    assert!(value["message"].get("raw_user_input").is_none());
}

#[test]
fn expected_stop_is_per_process() {
    // Controlled stop on the old process: its EOF is logged at debug.
    let old_stop = Arc::new(AtomicBool::new(true));
    capture_logs(|buf| {
        let (tx, rx) = std::sync::mpsc::channel();
        read_stdout_lines(Cursor::new(""), &tx, None, &old_stop);
        assert_eq!(
            rx.recv().unwrap().unwrap_err(),
            "cosh-core output reached EOF"
        );
        assert!(String::from_utf8(buf.lock().unwrap().clone())
            .unwrap()
            .contains("after expected stop"));
    });

    // A replacement process gets a fresh false flag; unexpected EOF warns.
    let new_stop = Arc::new(AtomicBool::new(false));
    capture_logs(|buf| {
        let (tx, rx) = std::sync::mpsc::channel();
        read_stdout_lines(Cursor::new(""), &tx, None, &new_stop);
        assert_eq!(
            rx.recv().unwrap().unwrap_err(),
            "cosh-core output reached EOF"
        );
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(output.contains("cosh-core output reached EOF"));
        assert!(!output.contains("after expected stop"));
    });

    // Forced cancellation marks the replacement; its EOF returns to debug.
    terminate_marked(Some(Arc::clone(&new_stop)), None);
    capture_logs(|buf| {
        let (tx, rx) = std::sync::mpsc::channel();
        read_stdout_lines(Cursor::new(""), &tx, None, &new_stop);
        assert_eq!(
            rx.recv().unwrap().unwrap_err(),
            "cosh-core output reached EOF"
        );
        assert!(String::from_utf8(buf.lock().unwrap().clone())
            .unwrap()
            .contains("after expected stop"));
    });
}

struct TestWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for TestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_logs<T>(test: impl FnOnce(Arc<Mutex<Vec<u8>>>) -> T) -> T {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer({
            let buf = Arc::clone(&buf);
            move || TestWriter(Arc::clone(&buf))
        })
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    test(buf)
}

#[test]
fn registry_transport_failure_is_logged_with_full_error() {
    for (error_text, diagnostic) in [
        ("live registry query timed out", "timed out"),
        ("cosh-core output stream disconnected", "disconnected"),
    ] {
        capture_logs(|buf| {
            let (tx, _) = mpsc::channel();
            let command = RegistryCommand {
                request_id: "req-1".to_string(),
                domain: "extensions".to_string(),
                action: "reload".to_string(),
                params: serde_json::Value::Null,
                response_tx: tx,
            };
            let result = Err(RegistryQueryError::Transport(error_text.to_string()));
            assert!(log_registry_transport_failure(&result, &command));
            let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
            assert!(output.contains("live registry transport failed"));
            assert!(output.contains(diagnostic));
            assert!(output.contains("extensions"));
            assert!(output.contains("reload"));
        });
    }
}

#[test]
fn drop_sets_expected_stop_before_reader_eof() {
    let expected_stop = Arc::new(AtomicBool::new(false));
    {
        let runtime = PersistentCoshCoreRuntime::default();
        {
            let mut current = runtime.current_process.lock().unwrap();
            current.expected_stop = Some(Arc::clone(&expected_stop));
            // child_pid is intentionally None: this test only needs the flag
            // to be marked; using a fake pid could accidentally signal an
            // unrelated process in the test namespace.
            current.child_pid = None;
        }
    }
    assert!(expected_stop.load(Ordering::SeqCst));
    capture_logs(|buf| {
        let (tx, rx) = std::sync::mpsc::channel();
        read_stdout_lines(Cursor::new(""), &tx, None, &expected_stop);
        assert_eq!(
            rx.recv().unwrap().unwrap_err(),
            "cosh-core output reached EOF"
        );
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(output.contains("cosh-core output reached EOF after expected stop"));
    });
}

#[test]
fn cancel_before_spawn_does_not_leave_stale_bindings() {
    let _guard = spawn_test_guard();
    let runtime = PersistentCoshCoreRuntime::default();
    let session_state = Arc::new(Mutex::new(SessionRuntimeState::default()));
    let resume_attempt = begin_session_attempt(&session_state, None, "/tmp");
    let handle = runtime.start_run(
        "run-cancel-before-spawn".to_string(),
        super::super::super::PreparedInvocation {
            program: "/nonexistent/cosh-core".to_string(),
            args: vec![],
            prompt: "hello".to_string(),
        },
        None,
        CoshApprovalMode::Recommend,
        session_state,
        "/tmp".to_string(),
        resume_attempt,
    );
    handle.cancel();
    thread::sleep(Duration::from_secs(2) + Duration::from_millis(50));
    let current = runtime.current_process.lock().unwrap();
    assert!(current.expected_stop.is_none());
    assert!(current.child_pid.is_none());
}

#[test]
fn cancellation_stops_a_valid_core_before_it_runs_a_turn() {
    let _guard = spawn_test_guard();
    let runtime = PersistentCoshCoreRuntime::default();
    let session_state = Arc::new(Mutex::new(SessionRuntimeState::default()));
    let resume_attempt = begin_session_attempt(&session_state, None, "/tmp");
    let handle = runtime.start_run(
        "run-cancel-valid-core".to_string(),
        super::super::super::PreparedInvocation {
            program: "/bin/sleep".to_string(),
            args: vec!["10".to_string()],
            prompt: "hello".to_string(),
        },
        None,
        CoshApprovalMode::Recommend,
        session_state,
        "/tmp".to_string(),
        resume_attempt,
    );
    handle.cancel();
    thread::sleep(Duration::from_secs(2) + Duration::from_millis(100));

    let deadline = Instant::now() + Duration::from_secs(3);
    assert!(
        drain_until_cancelled_or_deadline(&handle, deadline),
        "cancelled run should emit AgentCancelled"
    );

    let current = runtime.current_process.lock().unwrap();
    assert!(
        current.expected_stop.is_none(),
        "no expected-stop flag should remain bound after cancellation"
    );
    assert!(
        current.child_pid.is_none(),
        "no child pid should remain bound after cancellation"
    );
}

struct SpawnDelayReset;

impl Drop for SpawnDelayReset {
    fn drop(&mut self) {
        super::TEST_SPAWN_DELAY_MS.store(0, Ordering::SeqCst);
    }
}

#[test]
fn cancellation_during_slow_spawn_stops_core() {
    let _guard = spawn_test_guard();
    // Make spawn_process itself take longer than the forced-cancellation grace
    // window. The timer must still mark the reserved expected-stop flag, and
    // the turn must stop the child as soon as spawn returns.
    super::TEST_SPAWN_DELAY_MS.store(2500, Ordering::SeqCst);
    let _reset_delay = SpawnDelayReset;

    let runtime = PersistentCoshCoreRuntime::default();
    let session_state = Arc::new(Mutex::new(SessionRuntimeState::default()));
    let resume_attempt = begin_session_attempt(&session_state, None, "/tmp");
    let handle = runtime.start_run(
        "run-cancel-during-slow-spawn".to_string(),
        super::super::super::PreparedInvocation {
            program: "/bin/sleep".to_string(),
            args: vec!["10".to_string()],
            prompt: "hello".to_string(),
        },
        None,
        CoshApprovalMode::Recommend,
        session_state,
        "/tmp".to_string(),
        resume_attempt,
    );
    handle.cancel();
    // The spawn delay is longer than the cancel grace window; the test only
    // needs to wait long enough for spawn + teardown to finish.
    thread::sleep(Duration::from_millis(2600));

    let deadline = Instant::now() + Duration::from_secs(3);
    assert!(
        drain_until_cancelled_or_deadline(&handle, deadline),
        "cancelled run should emit AgentCancelled"
    );

    let current = runtime.current_process.lock().unwrap();
    assert!(
        current.expected_stop.is_none(),
        "no expected-stop flag should remain bound after cancellation"
    );
    assert!(
        current.child_pid.is_none(),
        "no child pid should remain bound after cancellation"
    );
}

#[test]
fn terminate_current_process_matches_flag_and_pid() {
    // The helper is what Drop and the cancellation timer actually call. It
    // must read expected_stop and child_pid under one lock so it cannot mark
    // one process and kill another if the bound process changes in between.
    let expected_stop = Arc::new(AtomicBool::new(false));
    // Match the production spawn path: put the child in its own session so
    // terminate_process_group(pid) targets the child's process group, not the
    // test process's group.
    let mut child = unsafe {
        std::process::Command::new("/bin/sleep")
            .arg("10")
            .pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            })
            .spawn()
            .expect("spawn test child")
    };
    let pid = child.id();
    let current_process = Arc::new(Mutex::new(super::ProcessBindings::default()));
    {
        let mut current = current_process.lock().unwrap();
        current.expected_stop = Some(Arc::clone(&expected_stop));
        current.child_pid = Some(pid);
    }
    super::super::runtime::terminate_current_process(&current_process);
    assert!(expected_stop.load(Ordering::SeqCst));

    // Reap the child and confirm it was actually terminated.
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut terminated = false;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            terminated = true;
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    if !terminated {
        let _ = child.kill();
    }
    let _ = child.wait();
    assert!(
        terminated,
        "terminate_current_process did not terminate the test child"
    );
}
