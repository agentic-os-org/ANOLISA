//! Runtime coverage uses only local children, with bounded fixture lifetimes.

#![cfg(target_os = "linux")]

#[path = "support/fixture.rs"]
mod fixture;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use aw_exec::{run, CommandSpec, Error, Stream};
use fixture::*;

#[test]
fn argv_cwd_and_environment_are_passed_literally() {
    let directory = Directory::new();
    let mut command = directory.command("context");
    let arguments = [
        OsString::from("quote'\" and whitespace"),
        OsString::from("; touch accidental-shell"),
        OsString::from("$AW_TEST"),
        OsString::from("*"),
        OsString::from(""),
        OsString::from_vec(b"non-utf8-\xff".to_vec()),
    ];
    command.args.extend(arguments.iter().cloned());
    command
        .environment
        .insert("AW_TEST".into(), "literal value".into());
    command
        .environment
        .insert("AW_BYTES".into(), OsString::from_vec(vec![0xff]));
    let output = execute(&command, b"", limits(0, 4096, 0)).unwrap();
    let mut records = vec![directory.0.as_os_str().as_bytes().to_vec()];
    records.extend(
        arguments
            .iter()
            .map(|argument| argument.as_bytes().to_vec()),
    );
    records.push(vec![]);
    records.extend(command.environment.iter().map(|(key, value)| {
        let mut record = key.as_bytes().to_vec();
        record.push(b'=');
        record.extend_from_slice(value.as_bytes());
        record
    }));
    assert_eq!(output.stdout, records.join(&0));
    assert!(output.stderr.is_empty());
    assert!(output.status.success());
    assert_eq!(output.input_bytes_written, 0);
    assert!(!directory.0.join("accidental-shell").exists());
    directory.assert_reaped();
}

#[test]
fn explicit_shell_is_supported() {
    let directory = Directory::new();
    let command = CommandSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "printf '%s' \"$VALUE\"".into()],
        cwd: directory.0.clone(),
        environment: BTreeMap::from([("VALUE".into(), "explicit shell".into())]),
    };
    let output = execute(&command, b"", limits(0, 14, 0)).unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"explicit shell");
}

#[test]
fn binary_streams_and_stdin_eof_are_preserved() {
    for input in [vec![], vec![0, 0xff, b'\n', 0x80, b'A']] {
        let directory = Directory::new();
        let command = directory.command("binary");
        let output = execute(
            &command,
            &input,
            limits(input.len(), input.len() + 6, input.len() + 1),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, [input.as_slice(), b"\0\xfftail"].concat());
        let mut expected_stderr: Vec<_> = input.iter().rev().copied().collect();
        expected_stderr.push(0xfe);
        assert_eq!(output.stderr, expected_stderr);
        assert_eq!(output.input_bytes_written, input.len());
        directory.assert_reaped();
    }
}

#[test]
fn duplex_payloads_larger_than_pipe_buffers_do_not_deadlock() {
    let directory = Directory::new();
    let input: Vec<_> = (0..524288).map(|index| (index % 256) as u8).collect();
    let output = execute(
        &directory.command("duplex"),
        &input,
        limits(input.len(), 262144 + input.len(), 262144),
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, [vec![b'O'; 262144], input.clone()].concat());
    assert_eq!(output.stderr, vec![b'E'; 262144]);
    assert_eq!(output.input_bytes_written, input.len());
    directory.assert_reaped();
}

#[test]
fn bulk_streams_are_not_throttled_by_the_poll_interval() {
    // Pacing every 8 KiB chunk by the 2 ms poll interval needs over 12 s for
    // these 72 MiB of pipe traffic, far beyond the 5 s fixture deadline.
    let directory = Directory::new();
    let input: Vec<_> = (0..24 << 20).map(|index| (index % 251) as u8).collect();
    let output = execute(
        &directory.command("binary"),
        &input,
        limits(input.len(), input.len() + 6, input.len() + 1),
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.input_bytes_written, input.len());
    assert_eq!(output.stdout.len(), input.len() + 6);
    assert!(output.stdout.starts_with(&input));
    assert_eq!(output.stderr.len(), input.len() + 1);
    directory.assert_reaped();
}

#[test]
fn exact_stream_limits_including_zero_are_allowed() {
    for length in [0, 1, 65536] {
        let directory = Directory::new();
        let mut command = directory.command("emit");
        command
            .args
            .extend([length.to_string().into(), length.to_string().into()]);
        let output = execute(&command, b"", limits(0, length, length)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, vec![b'O'; length]);
        assert_eq!(output.stderr, vec![b'E'; length]);
        directory.assert_reaped();
    }
}

#[test]
fn stream_limits_are_independent_and_one_excess_byte_fails() {
    for limit in [0, 65536] {
        for stdout in [true, false] {
            let directory = Directory::new();
            let mut command = directory.command("emit");
            let counts = if stdout {
                [limit + 1, 0]
            } else {
                [0, limit + 1]
            };
            command
                .args
                .extend(counts.map(|count| count.to_string().into()));
            let result = execute(&command, b"", limits(0, limit, limit));
            assert!(
                matches!(
                    result,
                    Err(Error::OutputLimit { stream: Stream::Stdout, limit: actual })
                        if stdout && actual == limit
                ) || matches!(
                    result,
                    Err(Error::OutputLimit { stream: Stream::Stderr, limit: actual })
                        if !stdout && actual == limit
                )
            );
            directory.assert_reaped();
        }
    }
}

#[test]
fn rejected_input_expired_deadline_and_pre_cancellation_do_not_spawn() {
    let directory = Directory::new();
    let mut command = directory.command("exit");
    command.args.push("0".into());
    assert!(matches!(
        execute(&command, b"too much", limits(3, 0, 0)),
        Err(Error::InputLimit {
            limit: 3,
            actual: 8
        })
    ));
    assert!(!directory.0.join("leader.pid").exists());
    assert!(matches!(
        run(
            &command,
            b"",
            limits(0, 0, 0),
            Instant::now(),
            &AtomicBool::new(false)
        ),
        Err(Error::DeadlineExceeded)
    ));
    assert!(!directory.0.join("leader.pid").exists());
    assert!(matches!(
        run(
            &command,
            b"",
            limits(0, 0, 0),
            Instant::now() + NORMAL_TIMEOUT,
            &AtomicBool::new(true)
        ),
        Err(Error::Cancelled)
    ));
    assert!(!directory.0.join("leader.pid").exists());
}

#[test]
fn spawn_errors_are_reported_without_starting_a_child() {
    let directory = Directory::new();
    let mut command = directory.command("exit");
    command.program = directory.0.join("missing-executable");
    assert!(matches!(
        execute(&command, b"", limits(0, 0, 0)),
        Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound
    ));
    assert!(!directory.0.join("leader.pid").exists());
}

#[test]
fn nonzero_and_signal_exit_statuses_are_not_transport_errors() {
    let directory = Directory::new();
    let mut command = directory.command("exit");
    command.args.push("23".into());
    let output = execute(&command, b"", limits(0, 0, 0)).unwrap();
    assert_eq!(output.status.code(), Some(23));
    directory.assert_reaped();
    let output = execute(&directory.command("signal"), b"", limits(0, 0, 0)).unwrap();
    assert_eq!(output.status.signal(), Some(libc::SIGTERM));
    directory.assert_reaped();
}

#[test]
fn early_stdin_close_reports_partial_delivery_and_keeps_output() {
    let directory = Directory::new();
    let input = vec![b'I'; 4 * 1024 * 1024];
    let output = execute(
        &directory.command("close-stdin"),
        &input,
        limits(input.len(), 12, 0),
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"stdin closed");
    assert!(output.input_bytes_written < input.len());
    directory.assert_reaped();
}

#[test]
fn runtime_cancellation_kills_and_reaps_the_started_child() {
    let directory = Directory::new();
    let cancelled = AtomicBool::new(false);
    let (result, started, cancelled_at) = thread::scope(|scope| {
        let setter = scope.spawn(|| {
            let started = wait_until_exists(
                &directory.0.join("leader.pid"),
                Instant::now() + NORMAL_TIMEOUT,
            );
            let cancelled_at = Instant::now();
            cancelled.store(true, Ordering::Release);
            (started, cancelled_at)
        });
        let result = run(
            &directory.command("wait"),
            b"",
            limits(0, 0, 0),
            Instant::now() + NORMAL_TIMEOUT,
            &cancelled,
        );
        let (started, cancelled_at) = setter.join().unwrap();
        (result, started, cancelled_at)
    });
    assert!(started, "fixture never reached runtime cancellation point");
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(cancelled_at.elapsed() < CLEANUP_ALLOWANCE);
    directory.assert_reaped();
}

#[test]
fn stdout_and_stderr_floods_observe_the_absolute_deadline() {
    for descriptor in [1, 2] {
        let directory = Directory::new();
        let mut command = directory.command("flood");
        command.args.push(descriptor.to_string().into());
        let deadline = Instant::now() + Duration::from_millis(500);
        let result = run(
            &command,
            b"",
            limits(0, 64 * 1024 * 1024, 64 * 1024 * 1024),
            deadline,
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(Error::DeadlineExceeded)));
        assert!(Instant::now() < deadline + CLEANUP_ALLOWANCE);
        directory.assert_reaped();
    }
}

#[test]
fn inherited_output_pipes_retain_the_leader_until_timeout_cleanup() {
    let directory = Directory::new();
    let mut command = directory.command("descendant");
    command.args.extend(["inherited".into(), "exit".into()]);
    let deadline = Instant::now() + Duration::from_secs(1);
    let (result, retained) = thread::scope(|scope| {
        let execution = scope.spawn(|| {
            run(
                &command,
                b"",
                limits(0, 0, 0),
                deadline,
                &AtomicBool::new(false),
            )
        });
        let ready = wait_until_exists(&directory.0.join("descendant.pid"), deadline);
        let mut retained = false;
        if ready {
            let leader = directory.process("leader");
            while Instant::now() < deadline {
                match leader.state() {
                    Some('Z') => {
                        retained = true;
                        break;
                    }
                    None => break,
                    _ => thread::sleep(Duration::from_millis(5)),
                }
            }
        }
        (execution.join().unwrap(), retained)
    });
    assert!(
        retained,
        "leader PID was not retained while its output pipes remained open"
    );
    assert!(matches!(result, Err(Error::DeadlineExceeded)));
    assert!(Instant::now() < deadline + CLEANUP_ALLOWANCE);
    directory.assert_descendant_stopped();
}

#[test]
fn success_still_kills_descendants_that_closed_their_pipes() {
    let directory = Directory::new();
    let mut command = directory.command("descendant");
    command.args.extend(["closed".into(), "exit".into()]);
    let output = execute(&command, b"", limits(0, 0, 0)).unwrap();
    assert!(output.status.success());
    directory.assert_descendant_stopped();
}

#[test]
fn timeout_kills_descendants_even_after_they_close_their_pipes() {
    let directory = Directory::new();
    let mut command = directory.command("descendant");
    command.args.extend(["closed".into(), "wait".into()]);
    let deadline = Instant::now() + Duration::from_secs(1);
    let result = run(
        &command,
        b"",
        limits(0, 0, 0),
        deadline,
        &AtomicBool::new(false),
    );
    assert!(matches!(result, Err(Error::DeadlineExceeded)));
    assert!(Instant::now() < deadline + CLEANUP_ALLOWANCE);
    directory.assert_descendant_stopped();
}

#[test]
fn parallel_exchanges_keep_their_binary_streams_separate() {
    thread::scope(|scope| {
        let calls: Vec<_> = (0..4)
            .map(|index| {
                scope.spawn(move || {
                    let directory = Directory::new();
                    let input = vec![index; 32768];
                    let output = execute(
                        &directory.command("binary"),
                        &input,
                        limits(input.len(), input.len() + 6, input.len() + 1),
                    )
                    .unwrap();
                    assert!(output.status.success());
                    assert_eq!(output.stdout, [input.as_slice(), b"\0\xfftail"].concat());
                    assert_eq!(output.stderr, [input.as_slice(), b"\xfe"].concat());
                    assert_eq!(output.input_bytes_written, input.len());
                    directory.assert_reaped();
                })
            })
            .collect();
        for call in calls {
            call.join().unwrap();
        }
    });
}

#[test]
fn cancelling_one_parallel_call_does_not_kill_its_neighbor() {
    let cancelled_directory = Directory::new();
    let survivor_directory = Directory::new();
    let cancelled = AtomicBool::new(false);
    let (ready, cancelled_result, survivor_was_live, survivor_result) = thread::scope(|scope| {
        let cancelled_call = scope.spawn(|| {
            run(
                &cancelled_directory.command("wait"),
                b"",
                limits(0, 8, 0),
                Instant::now() + NORMAL_TIMEOUT,
                &cancelled,
            )
        });
        let survivor_call =
            scope.spawn(|| execute(&survivor_directory.command("wait"), b"", limits(0, 8, 0)));
        let readiness_deadline = Instant::now() + Duration::from_secs(3);
        let ready =
            wait_until_exists(
                &cancelled_directory.0.join("leader.pid"),
                readiness_deadline,
            ) && wait_until_exists(&survivor_directory.0.join("leader.pid"), readiness_deadline);
        cancelled.store(true, Ordering::Release);
        let cancelled_result = cancelled_call.join().unwrap();
        let survivor_was_live = ready && survivor_directory.process("leader").is_live();
        fs::write(survivor_directory.0.join("release"), b"go").unwrap();
        let survivor_result = survivor_call.join().unwrap();
        (ready, cancelled_result, survivor_was_live, survivor_result)
    });
    assert!(ready, "parallel fixtures never became ready");
    assert!(matches!(cancelled_result, Err(Error::Cancelled)));
    assert!(
        survivor_was_live,
        "cancellation stopped an unrelated executor group"
    );
    let output = survivor_result.unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"released");
    cancelled_directory.assert_reaped();
    survivor_directory.assert_reaped();
}
