use super::*;
use crate::diagnostics::health::runtime::process::*;
use std::process::Child;
use std::sync::atomic::Ordering;

#[test]
fn output_limit_stops_running_command_before_timeout() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let started = Instant::now();
    let result = run_health_command(
        "/bin/sh",
        &["-c", "printf 123456789; /bin/sleep 1"],
        HealthCommandConfig {
            timeout: Duration::from_millis(500),
            output_limit_bytes: 4,
        },
    );
    assert_eq!(result, Err(HealthCommandError::OutputLimit));
    assert_eq!(
        HealthCommandError::OutputLimit.unavailable_reason(),
        HealthUnavailableReason::ParseError
    );
    assert!(started.elapsed() < Duration::from_millis(300));
}

#[test]
fn timeout_stops_descendants_before_they_can_write() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("temporary directory");
    let marker = directory.path().join("late-output");
    let result = run_health_command(
        "/bin/sh",
        &[
            "-c",
            "(printf ready > \"$1\"; /bin/sleep 0.3; printf escaped >> \"$1\") & wait",
            "sh",
            marker.to_str().expect("path"),
        ],
        HealthCommandConfig {
            timeout: Duration::from_millis(150),
            output_limit_bytes: 1024,
        },
    );
    assert_eq!(result, Err(HealthCommandError::Timeout));
    assert_eq!(
        std::fs::read_to_string(&marker).expect("child started"),
        "ready"
    );
    std::thread::sleep(Duration::from_millis(350));
    assert_eq!(std::fs::read_to_string(&marker).expect("marker"), "ready");
}

#[test]
fn exited_command_preserves_all_output_within_limit() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let output = run_health_command(
        "/bin/sh",
        &["-c", "/usr/bin/head -c 131071 /dev/zero"],
        HealthCommandConfig {
            timeout: Duration::from_secs(2),
            output_limit_bytes: 256 * 1024,
        },
    )
    .expect("complete command");
    assert_eq!(output.stdout.len(), 131071);
    assert!(!output.stdout_truncated);
    assert_eq!(output.exit_code, Some(0));
}

#[test]
fn successful_exit_stops_descendant_holding_output_pipes() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("temporary directory");
    let marker = directory.path().join("late-output");
    let started = Instant::now();
    let output = run_health_command(
        "/bin/sh",
        &[
            "-c",
            "(printf ready > \"$1\"; /bin/sleep 0.4; printf escaped >> \"$1\") & /bin/sleep 0.1; printf done",
            "sh",
            marker.to_str().expect("path"),
        ],
        HealthCommandConfig {
            timeout: Duration::from_secs(2),
            output_limit_bytes: 1024,
        },
    )
    .expect("parent exits before descendant");
    assert_eq!(output.stdout, "done");
    assert_eq!(output.exit_code, Some(0));
    assert!(started.elapsed() < Duration::from_millis(350));
    assert_eq!(
        std::fs::read_to_string(&marker).expect("child started"),
        "ready"
    );
    std::thread::sleep(Duration::from_millis(450));
    assert_eq!(std::fs::read_to_string(&marker).expect("marker"), "ready");
}

#[test]
fn command_reads_empty_stdin_and_no_terminal_streams() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let output = run_health_command(
        "/bin/sh",
        &[
            "-c",
            "if read value; then exit 9; fi; test ! -t 0 && test ! -t 1 && test ! -t 2",
        ],
        HealthCommandConfig::default(),
    )
    .expect("empty stdin");
    assert_eq!(output.exit_code, Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn bounded_available_output_is_not_silently_dropped() {
    let source = vec![b'x'; 96 * 1024];
    let mut stream = io::Cursor::new(&source);
    let mut output = Vec::new();
    read_available(
        &mut stream,
        &mut output,
        source.len(),
        Instant::now() + Duration::from_secs(1),
    )
    .expect("read output");
    assert_eq!(output.len(), source.len());
    assert!(output.iter().all(|byte| *byte == b'x'));
}

#[test]
fn continuously_ready_output_obeys_deadline() {
    struct ReadyOutput {
        reads: usize,
    }
    impl Read for ReadyOutput {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.reads == 50 {
                return Ok(0);
            }
            self.reads += 1;
            std::thread::sleep(Duration::from_millis(2));
            bytes.fill(b'x');
            Ok(bytes.len())
        }
    }
    let mut stream = ReadyOutput { reads: 0 };
    let started = Instant::now();
    let result = read_available(
        &mut stream,
        &mut Vec::new(),
        1024 * 1024,
        started + Duration::from_millis(5),
    );
    assert_eq!(result, Err(HealthCommandError::Timeout));
    assert!(stream.reads < 50);
}

#[test]
fn command_cannot_open_controlling_terminal() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let output = run_health_command(
        "/bin/sh",
        &["-c", "if (exec 3<>/dev/tty) 2>/dev/null; then exit 9; fi"],
        HealthCommandConfig::default(),
    )
    .expect("terminal access probe");
    assert_eq!(output.exit_code, Some(0));
}

fn detached_child(script: &str) -> Child {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().expect("detached test child")
}

#[test]
fn observing_exit_keeps_the_leader_waitable() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let owner = ReapOwner::new();
    let child = detached_child("exit 7");
    let pid = child.id();
    let mut guard = owner.reserve().expect("reap capacity").register(
        child,
        CleanupScope::OwnedGroup,
        Instant::now() + Duration::from_secs(1),
    );
    while !child_has_exited(pid).expect("observe unreaped child") {
        assert!(Instant::now() < guard.deadline);
        thread::sleep(COMMAND_POLL_INTERVAL);
    }
    assert!(child_has_exited(pid).expect("WNOWAIT must retain exit status"));
    let _ = guard.cleanup();
    assert!(guard.is_released());
    assert_eq!(
        child_has_exited(pid)
            .expect_err("child finally reaped")
            .raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn child_capacity_is_reserved_before_spawn_and_released() {
    let owner = ReapOwner::new();
    let mut permits: Vec<_> = (0..HEALTH_COMMAND_CHILD_CAP)
        .map(|_| owner.reserve().expect("available child slot"))
        .collect();
    assert!(owner.reserve().is_none());
    drop(permits.pop());
    assert!(owner.reserve().is_some());
    drop(permits);
    assert!(owner
        .slots
        .iter()
        .all(|slot| !slot.reserved.load(Ordering::Acquire)));
}

#[test]
fn expired_cleanup_transfers_child_without_releasing_its_slot() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let owner = ReapOwner::new();
    let child = detached_child("/bin/sleep 1");
    let pid = child.id();
    let permit = owner.reserve().expect("reap capacity");
    let index = permit.index;
    let mut guard = permit.register(child, CleanupScope::OwnedGroup, Instant::now());
    assert!(guard.cleanup().expect("termination requested").is_none());
    drop(guard);
    assert!(owner.slots[index].reserved.load(Ordering::Acquire));
    assert!(owner.slot_has_child(index));
    let deadline = Instant::now() + Duration::from_secs(1);
    while owner.reap_pending() {
        assert!(Instant::now() < deadline, "test child was not reaped");
        thread::sleep(COMMAND_POLL_INTERVAL);
    }
    assert!(!owner.slots[index].reserved.load(Ordering::Acquire));
    assert!(!owner.slot_has_child(index));
    assert_eq!(
        child_has_exited(pid)
            .expect_err("supervised child reaped")
            .raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "helper subprocess requiring a controlling terminal"]
fn controlling_terminal_child() {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .expect("helper must have an actual controlling terminal");
    command_cannot_open_controlling_terminal();
    std::process::exit(23);
}

#[cfg(target_os = "linux")]
#[test]
fn parent_controlling_terminal_does_not_reach_command() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let terminal = nix::pty::openpty(None, None).expect("test terminal");
    let fd = terminal.slave.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args([
            "--ignored",
            "--exact",
            "diagnostics::health::collectors::command::tests::controlling_terminal_child",
            "--test-threads=1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(fd, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let owner = ReapOwner::global().expect("supervised helper reaper");
    let permit = owner.reserve().expect("child capacity");
    let child = command.spawn().expect("controlling terminal helper");
    let mut guard = permit.register(
        child,
        CleanupScope::OwnedGroup,
        Instant::now() + Duration::from_secs(3),
    );
    while !guard.has_exited().expect("observe terminal helper") {
        assert!(Instant::now() < guard.deadline, "terminal helper timed out");
        thread::sleep(COMMAND_POLL_INTERVAL);
    }
    assert_eq!(
        guard
            .cleanup()
            .expect("terminal helper cleanup")
            .expect("reaped helper")
            .code(),
        Some(23)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn darwin_group_query_rejects_a_second_member() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let owner = ReapOwner::new();
    let permit = owner.reserve().expect("child capacity");
    let child = detached_child("/bin/sleep 1 & /bin/sleep 0.05; exit 0");
    let pid = child.id();
    let mut guard = permit.register(
        child,
        CleanupScope::OwnedGroup,
        Instant::now() + Duration::from_secs(1),
    );
    while !child_has_exited(pid).expect("observe group leader") {
        assert!(Instant::now() < guard.deadline);
        thread::sleep(COMMAND_POLL_INTERVAL);
    }
    assert!(!darwin_group_contains_only_leader(pid));
    assert_eq!(
        guard
            .cleanup()
            .expect("group cleanup")
            .expect("leader reaped")
            .code(),
        Some(0)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn darwin_group_query_rejects_unknown_group() {
    assert!(!darwin_group_contains_only_leader(u32::MAX));
}
