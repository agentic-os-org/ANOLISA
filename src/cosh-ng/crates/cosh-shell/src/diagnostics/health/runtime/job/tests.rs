use super::*;
use nix::libc;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn absolute_deadline_does_not_restart_a_previously_expired_budget() {
    let absolute = Instant::now() + Duration::from_millis(5);
    thread::sleep(Duration::from_millis(10));
    let deadline = JobDeadline::at_instant(absolute, Duration::from_millis(350)).unwrap();
    assert!(deadline.remaining().unwrap().is_zero());
}
use std::fs;
use std::process::Stdio;
use std::thread;

use super::super::process::{
    child_has_exited, CleanupScope, CommandChild, ReapOwner, COMMAND_POLL_INTERVAL,
};

fn own_child(command: &mut Command, deadline: JobDeadline) -> CommandChild<'static> {
    let owner = ReapOwner::global().expect("health reaper");
    let permit = owner.reserve().expect("child capacity");
    let guard_deadline = deadline.instant().expect("deadline");
    let child = command.spawn().expect("isolated child");
    permit.register(child, CleanupScope::OwnedGroup, guard_deadline)
}

fn wait_exit(child: &CommandChild<'_>) {
    let limit = Instant::now() + Duration::from_secs(3);
    while !child.has_exited().expect("observe child") {
        assert!(Instant::now() < limit, "child did not stop");
        thread::sleep(COMMAND_POLL_INTERVAL);
    }
}

#[test]
fn deadline_covers_slow_exec_before_handler() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("test directory");
    let marker = directory.path().join("escaped");
    let deadline = JobDeadline::after(Duration::from_millis(100)).expect("deadline");
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "printf escaped > \"$1\"",
            "sh",
            marker.to_str().expect("path"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_isolated_worker(&mut command, deadline);
    unsafe {
        command.pre_exec(|| {
            let duration = libc::timespec {
                tv_sec: 0,
                tv_nsec: 300_000_000,
            };
            libc::nanosleep(&duration, std::ptr::null_mut());
            Ok(())
        });
    }
    let mut child = own_child(&mut command, deadline);
    wait_exit(&child);
    let _ = child.cleanup();
    assert!(!marker.exists(), "deadline failed to cover pre-exec delay");
}

#[test]
#[ignore = "isolated deadline helper subprocess"]
#[allow(clippy::zombie_processes)] // The supervised process group must reclaim an orphaned descendant.
fn deadline_job_child() {
    install_worker_timeout_handler().expect("worker deadline handler");
    let marker = std::env::var("COSH_HEALTH_DEADLINE_TEST_MARKER").expect("test marker");
    if std::env::var_os("COSH_HEALTH_TEST_OUTPUT_LIMIT").is_some() {
        use crate::diagnostics::health::collectors;
        collectors::enter_worker_job(JobDeadline::after(Duration::from_secs(2)).unwrap()).unwrap();
        let result = collectors::run_health_command(
            "/bin/sh",
            &["-c", "printf 1234567890"],
            collectors::HealthCommandConfig {
                timeout: Duration::from_millis(500),
                output_limit_bytes: 1,
            },
        );
        fs::write(
            &marker,
            format!(
                "{result:?};stopped={}",
                collectors::worker_command_stopped()
            ),
        )
        .unwrap();
        return;
    }
    if std::env::var_os("COSH_HEALTH_DEADLINE_TEST_COMMAND").is_some() {
        crate::diagnostics::health::collectors::enter_worker_job(
            JobDeadline::after(Duration::from_secs(2)).expect("command scope"),
        )
        .expect("inherited group scope");
        fs::write(&marker, "ready").expect("ready marker");
        let _ = crate::diagnostics::health::collectors::run_health_command(
            "/bin/sh",
            &[
                "-c",
                "/bin/sleep 0.4; printf escaped >> \"$1\"; /bin/sleep 1",
                "sh",
                &marker,
            ],
            crate::diagnostics::health::collectors::HealthCommandConfig {
                timeout: Duration::from_secs(2),
                output_limit_bytes: 1024,
            },
        );
        return;
    }
    let delay = std::env::var("COSH_HEALTH_DEADLINE_TEST_DELAY").unwrap_or_else(|_| "0.4".into());
    let _child = Command::new("/bin/sh")
        .args([
            "-c",
            "/bin/sleep \"$1\"; printf escaped >> \"$2\"",
            "sh",
            &delay,
            &marker,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("inherited-group descendant");
    fs::write(&marker, "ready").expect("ready marker");
    if std::env::var_os("COSH_HEALTH_DEADLINE_TEST_EXIT").is_some() {
        std::process::exit(17);
    }
    thread::sleep(Duration::from_secs(2));
}

#[test]
fn one_command_output_limit_does_not_cancel_unrelated_worker_checks() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("output-limit-state");
    let deadline = JobDeadline::after(Duration::from_secs(2)).unwrap();
    let mut command = helper_command(&marker, deadline);
    command.env("COSH_HEALTH_TEST_OUTPUT_LIMIT", "1");
    let mut child = own_child(&mut command, deadline);
    wait_exit(&child);
    let _ = child.cleanup();
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "Err(OutputLimit);stopped=false"
    );
}

#[test]
fn early_worker_exit_is_supervised_even_when_parent_stops_polling() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("early-exit-no-poll");
    let deadline = JobDeadline::after(Duration::from_secs(1)).unwrap();
    let mut command = helper_command(&marker, deadline);
    command
        .env("COSH_HEALTH_DEADLINE_TEST_EXIT", "1")
        .env("COSH_HEALTH_DEADLINE_TEST_DELAY", "1.2");
    let mut child = own_child(&mut command, deadline);
    let ready = Instant::now() + Duration::from_secs(2);
    while !marker.exists() {
        assert!(Instant::now() < ready, "helper did not start");
        thread::sleep(COMMAND_POLL_INTERVAL);
    }
    thread::sleep(Duration::from_millis(1500));
    let _ = child.cleanup();
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "ready",
        "descendant survived leader exit and the original deadline"
    );
}

fn helper_command(marker: &std::path::Path, deadline: JobDeadline) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args([
            "--ignored",
            "--exact",
            "diagnostics::health::runtime::job::tests::deadline_job_child",
            "--test-threads=1",
        ])
        .env("COSH_HEALTH_DEADLINE_TEST_MARKER", marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_isolated_worker(&mut command, deadline);
    command
}

fn await_ready(marker: &std::path::Path) {
    let limit = Instant::now() + Duration::from_secs(1);
    while !marker.exists() {
        assert!(Instant::now() < limit, "helper was not ready");
        thread::sleep(COMMAND_POLL_INTERVAL);
    }
}

#[test]
fn worker_deadline_stops_descendants_without_parent_polling() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("test directory");
    let marker = directory.path().join("state");
    let deadline = JobDeadline::after(Duration::from_millis(200)).expect("deadline");
    let mut command = helper_command(&marker, deadline);
    command.env("COSH_HEALTH_DEADLINE_TEST_COMMAND", "1");
    let mut child = own_child(&mut command, deadline);
    await_ready(&marker);
    thread::sleep(Duration::from_millis(500));
    let observed = fs::read_to_string(&marker).expect("marker");
    let _ = child.cleanup();
    assert_eq!(observed, "ready", "descendant escaped an unpolled deadline");
}

#[test]
fn early_worker_exit_still_cleans_its_descendants() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("test directory");
    let marker = directory.path().join("state");
    let deadline = JobDeadline::after(Duration::from_secs(1)).expect("deadline");
    let mut command = helper_command(&marker, deadline);
    command.env("COSH_HEALTH_DEADLINE_TEST_EXIT", "1");
    let mut child = own_child(&mut command, deadline);
    await_ready(&marker);
    wait_exit(&child);
    let _ = child.cleanup();
    thread::sleep(Duration::from_millis(500));
    assert_eq!(fs::read_to_string(&marker).expect("marker"), "ready");
}

#[test]
fn cancelling_worker_cleans_its_descendants() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("test directory");
    let marker = directory.path().join("state");
    let deadline = JobDeadline::after(Duration::from_secs(1)).expect("deadline");
    let mut command = helper_command(&marker, deadline);
    let mut child = own_child(&mut command, deadline);
    await_ready(&marker);
    let _ = child.cleanup();
    thread::sleep(Duration::from_millis(500));
    assert_eq!(fs::read_to_string(&marker).expect("marker"), "ready");
}

fn inherited_alarm_state_is_reset(ignore: bool, blocked: bool) {
    let directory = tempfile::tempdir().expect("test directory");
    let marker = directory.path().join("escaped");
    let deadline = JobDeadline::after(Duration::from_millis(100)).expect("deadline");
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "printf escaped > \"$1\"",
            "sh",
            marker.to_str().expect("path"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(move || {
            if ignore {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = libc::SIG_IGN;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(libc::SIGALRM, &action, std::ptr::null_mut()) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if blocked {
                let mut mask: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut mask);
                libc::sigaddset(&mut mask, libc::SIGALRM);
                if libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    configure_isolated_worker(&mut command, deadline);
    unsafe {
        command.pre_exec(|| {
            let delay = libc::timespec {
                tv_sec: 0,
                tv_nsec: 300_000_000,
            };
            libc::nanosleep(&delay, std::ptr::null_mut());
            Ok(())
        });
    }
    let mut child = own_child(&mut command, deadline);
    wait_exit(&child);
    let _ = child.cleanup();
    assert!(!marker.exists());
}

#[test]
fn inherited_ignored_sigalrm_cannot_disable_the_deadline() {
    let _guard = crate::diagnostics::test_env::env_guard();
    inherited_alarm_state_is_reset(true, false);
}

#[test]
fn inherited_blocked_sigalrm_cannot_disable_the_deadline() {
    let _guard = crate::diagnostics::test_env::env_guard();
    inherited_alarm_state_is_reset(false, true);
}

#[test]
fn sub_microsecond_budget_cannot_disable_the_timer() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("test directory");
    let marker = directory.path().join("escaped");
    let deadline = JobDeadline::after(Duration::from_nanos(1)).expect("deadline");
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "printf escaped > \"$1\"",
            "sh",
            marker.to_str().expect("path"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_isolated_worker(&mut command, deadline);
    let mut child = own_child(&mut command, deadline);
    wait_exit(&child);
    let _ = child.cleanup();
    assert!(!marker.exists());
}
