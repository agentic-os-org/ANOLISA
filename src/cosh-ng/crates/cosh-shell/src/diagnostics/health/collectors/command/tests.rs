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

fn filter_fixture(
    content: &str,
    filter: HealthLineFilter,
    matches: impl Fn(&str) -> bool,
) -> HealthFilteredLines {
    filter_fixture_chunked(content, filter, matches, 64)
}

fn filter_fixture_chunked(
    content: &str,
    filter: HealthLineFilter,
    matches: impl Fn(&str) -> bool,
    chunk_bytes: usize,
) -> HealthFilteredLines {
    let mut state = HealthLineFilterState::new(filter);
    for chunk in content.as_bytes().chunks(chunk_bytes) {
        state.push(chunk, &matches);
    }
    state.mark_eof(&matches);
    state.finish()
}

#[test]
fn bounded_filter_keeps_matching_tail_when_noise_floods_the_stream() {
    let mut content = String::new();
    for index in 0..4000 {
        content.push_str(&format!(
            "[ {index}.000000] audit: unrelated noise line {index}\n"
        ));
    }
    content.push_str(
        "[ 5000.000000] oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/w,task=java,pid=71\n",
    );
    content.push_str("[ 5001.000000] Out of memory: Killed process 71 (java) total-vm:1024kB\n");
    let output = filter_fixture(
        &content,
        HealthLineFilter {
            max_records: 200,
            max_line_bytes: 4096,
            retained_bytes: 16 * 1024,
            raw_read_limit_bytes: 8 * 1024 * 1024,
            newest_first: false,
        },
        |line| line.contains("oom-kill") || line.contains("Killed process"),
    );
    assert!(output.complete);
    assert_eq!(output.lines.len(), 2);
    assert!(output.lines[0].contains("oom-kill:constraint="));
    assert!(output.lines[1].contains("Killed process 71"));
    assert_eq!(output.lines_scanned, 4002);
    assert!(output.raw_bytes_read >= content.len() as u64);
}

#[test]
fn bounded_filter_stops_at_raw_read_limit_and_reports_incomplete() {
    let mut content = String::new();
    for index in 0..2000 {
        content.push_str(&format!(
            "[ {index}.000000] oom-kill:constraint=CONSTRAINT_MEMCG,task=java,pid={index}\n"
        ));
    }
    let output = filter_fixture(
        &content,
        HealthLineFilter {
            max_records: 200,
            max_line_bytes: 4096,
            retained_bytes: 16 * 1024,
            raw_read_limit_bytes: 4096,
            newest_first: false,
        },
        |line| line.contains("oom-kill"),
    );
    assert!(!output.complete);
    assert!(output.raw_bytes_read >= 4096);
    assert!(!output.lines.is_empty());
}

#[test]
fn bounded_filter_truncates_overlong_lines_and_counts_them() {
    let content = format!(
        "[ 1.000000] oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/{}\n",
        "x".repeat(20_000)
    );
    let output = filter_fixture(
        &content,
        HealthLineFilter {
            max_records: 200,
            max_line_bytes: 512,
            retained_bytes: 16 * 1024,
            raw_read_limit_bytes: 8 * 1024 * 1024,
            newest_first: false,
        },
        |line| line.contains("oom-kill"),
    );
    assert_eq!(output.truncated_lines, 1);
    assert_eq!(output.lines.len(), 1);
    assert!(output.lines[0].len() <= 512);
}

#[test]
fn bounded_filter_retains_only_the_newest_matching_records() {
    let mut content = String::new();
    for index in 0..50 {
        content.push_str(&format!(
            "[ {index}.000000] oom-kill:constraint=CONSTRAINT_MEMCG,task=java,pid={index}\n"
        ));
    }
    let output = filter_fixture(
        &content,
        HealthLineFilter {
            max_records: 3,
            max_line_bytes: 4096,
            retained_bytes: 16 * 1024,
            raw_read_limit_bytes: 8 * 1024 * 1024,
            newest_first: false,
        },
        |line| line.contains("oom-kill"),
    );
    assert!(output.complete);
    assert_eq!(output.lines.len(), 3);
    assert!(output.lines[0].contains("pid=47"));
    assert!(output.lines[2].contains("pid=49"));
}

#[test]
fn bounded_filter_stress_noise_flood_stays_within_check_budget() {
    let mut content = String::with_capacity(4 * 1024 * 1024);
    while content.len() < 3 * 1024 * 1024 {
        content.push_str(
            "[ 1234.567890] audit: type=1400 audit(1700000000.000:1234): unrelated noise record\n",
        );
    }
    for index in 0..400 {
        content.push_str(&format!(
            "[ 5000.000000] oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/w,task=java,pid={index}\n"
        ));
    }
    let expected_lines = content.lines().count() as u64;
    let started = Instant::now();
    let output = filter_fixture(&content, HealthLineFilter::default(), |line| {
        line.contains("oom-kill")
    });
    let elapsed = started.elapsed();
    eprintln!(
        "stress noise_flood: bytes={} lines={} retained={} elapsed_ms={}",
        content.len(),
        output.lines_scanned,
        output.lines.len(),
        elapsed.as_millis()
    );
    assert!(output.complete);
    assert_eq!(output.lines_scanned, expected_lines);
    assert!(output.lines.len() <= 200);
    assert!(!output.lines.is_empty());
    assert!(output.lines.last().expect("tail").contains("pid=399"));
    assert!(elapsed < Duration::from_millis(120));
}

#[test]
fn bounded_filter_stress_oom_storm_bounds_retained_evidence() {
    let mut content = String::new();
    for index in 0..5000 {
        content.push_str(&format!(
            "[ 5000.000000] oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/storm,task=java,pid={index}\n\
             [ 5000.000000] Out of memory: Killed process {index} (java) total-vm:1024kB\n"
        ));
    }
    let started = Instant::now();
    let output = filter_fixture(&content, HealthLineFilter::default(), |line| {
        line.contains("oom-kill") || line.contains("Killed process")
    });
    let elapsed = started.elapsed();
    let retained_bytes: usize = output.lines.iter().map(String::len).sum();
    eprintln!(
        "stress oom_storm: bytes={} lines={} retained={} retained_bytes={} elapsed_ms={}",
        content.len(),
        output.lines_scanned,
        output.lines.len(),
        retained_bytes,
        elapsed.as_millis()
    );
    assert!(output.complete);
    assert_eq!(output.lines_scanned, 10_000);
    assert!(output.lines.len() <= 200);
    assert!(!output.lines.is_empty());
    assert!(retained_bytes <= 16 * 1024);
    assert!(output
        .lines
        .last()
        .expect("tail")
        .contains("Killed process 4999"));
    assert!(elapsed < Duration::from_millis(120));
}

#[test]
fn filtered_command_keeps_confirmed_evidence_when_the_deadline_expires() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("temporary directory");
    let fixture = directory.path().join("dmesg.log");
    let mut content =
        String::from("[ 1.000000] oom-kill:constraint=CONSTRAINT_MEMCG,task=java,pid=71\n");
    while content.len() < 16 * 1024 * 1024 {
        content.push_str("[ 1234.567890] audit: unrelated noise record for the ring buffer\n");
    }
    std::fs::write(&fixture, &content).expect("fixture");
    let started = Instant::now();
    let output = run_health_command_filtered(
        "/bin/sh",
        &["-c", "cat \"$1\"", "sh", fixture.to_str().expect("path")],
        HealthCommandConfig {
            timeout: Duration::from_millis(120),
            output_limit_bytes: 16 * 1024,
        },
        HealthLineFilter {
            raw_read_limit_bytes: 32 * 1024 * 1024,
            ..HealthLineFilter::default()
        },
        |line| line.contains("oom-kill"),
    )
    .expect("partial evidence instead of an error");
    let elapsed = started.elapsed();
    eprintln!(
        "filtered_command deadline: elapsed_ms={} lines={} complete={}",
        elapsed.as_millis(),
        output.filtered.lines.len(),
        output.filtered.complete
    );
    assert_eq!(output.filtered.lines.len(), 1);
    assert!(output.filtered.lines[0].contains("pid=71"));
    assert!(!output.filtered.complete);
    assert!(elapsed < Duration::from_millis(400));
}

#[test]
fn filtered_command_reads_a_large_log_without_failing_on_output_limit() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("temporary directory");
    let fixture = directory.path().join("dmesg.log");
    let mut content = String::new();
    while content.len() < 512 * 1024 {
        content.push_str("[ 1234.567890] audit: unrelated noise record for the ring buffer\n");
    }
    content.push_str("[ 5000.000000] oom-kill:constraint=CONSTRAINT_MEMCG,task=java,pid=71\n");
    content.push_str("[ 5001.000000] Out of memory: Killed process 71 (java) total-vm:1024kB\n");
    std::fs::write(&fixture, &content).expect("fixture");
    let started = Instant::now();
    let output = run_health_command_filtered(
        "/bin/sh",
        &[
            "-c",
            "cat \"$1\"; printf 'final diagnostic' >&2",
            "sh",
            fixture.to_str().expect("path"),
        ],
        HealthCommandConfig {
            timeout: Duration::from_millis(120),
            output_limit_bytes: 16 * 1024,
        },
        HealthLineFilter::default(),
        |line| line.contains("oom-kill") || line.contains("Killed process"),
    )
    .expect("large log keeps evidence");
    let elapsed = started.elapsed();
    eprintln!(
        "filtered_command large_log: bytes={} elapsed_ms={} lines={} complete={}",
        content.len(),
        elapsed.as_millis(),
        output.filtered.lines.len(),
        output.filtered.complete
    );
    assert_eq!(output.exit_code, Some(0));
    assert!(output.filtered.complete);
    assert_eq!(output.filtered.lines.len(), 2);
    assert!(output.filtered.lines[1].contains("Killed process 71"));
    assert!(output.stderr.contains("final diagnostic"));
    assert!(elapsed < Duration::from_millis(120));
}

#[test]
fn bounded_filter_accepts_empty_input() {
    let output = filter_fixture("", HealthLineFilter::default(), |_| true);
    assert!(output.complete);
    assert!(output.lines.is_empty());
    assert_eq!(output.lines_scanned, 0);
    assert_eq!(output.raw_bytes_read, 0);
}

#[test]
fn bounded_filter_offers_a_final_line_without_a_newline() {
    let output = filter_fixture(
        "[ 1.000000] oom-kill:constraint=CONSTRAINT_MEMCG,task=java,pid=71",
        HealthLineFilter::default(),
        |line| line.contains("oom-kill"),
    );
    assert!(output.complete);
    assert_eq!(output.lines.len(), 1);
    assert_eq!(output.lines_scanned, 1);
}

#[test]
fn bounded_filter_replaces_invalid_utf8_without_panicking() {
    let mut content = b"[ 1.000000] oom-kill:task=".to_vec();
    content.extend_from_slice(&[0xff, 0xfe, 0x80, 0xc0]);
    content.extend_from_slice(b",pid=71\n");
    let mut state = HealthLineFilterState::new(HealthLineFilter::default());
    state.push(&content, &|line| line.contains("oom-kill"));
    state.mark_eof(&|line| line.contains("oom-kill"));
    let output = state.finish();
    assert!(output.complete);
    assert_eq!(output.lines.len(), 1);
    assert!(output.lines[0].contains("pid=71"));
}

#[test]
fn bounded_filter_counts_one_overlong_line_across_single_byte_chunks() {
    let content = format!("[ 1.000000] oom-kill:oom_memcg=/{}\n", "x".repeat(20_000));
    let output = filter_fixture_chunked(
        &content,
        HealthLineFilter {
            max_line_bytes: 512,
            ..HealthLineFilter::default()
        },
        |line| line.contains("oom-kill"),
        1,
    );
    assert_eq!(output.truncated_lines, 1);
    assert_eq!(output.lines.len(), 1);
    assert!(output.lines[0].len() <= 512);
    assert_eq!(output.lines_scanned, 1);
}

#[test]
fn bounded_filter_keeps_the_newest_when_every_line_matches() {
    let mut content = String::new();
    for index in 0..4000 {
        content.push_str(&format!(
            "[ 5000.000000] oom-kill:constraint=CONSTRAINT_MEMCG,task=java,pid={index}\n"
        ));
    }
    let output = filter_fixture(&content, HealthLineFilter::default(), |line| {
        line.contains("oom-kill")
    });
    let retained_bytes: usize = output.lines.iter().map(String::len).sum();
    assert!(output.complete);
    assert!(retained_bytes <= 16 * 1024);
    assert!(output.lines.len() <= 200);
    assert!(output.lines.last().expect("tail").contains("pid=3999"));
}

#[test]
fn bounded_filter_drops_a_record_larger_than_the_retained_budget() {
    let content = format!("[ 1.000000] oom-kill:oom_memcg=/{}\n", "x".repeat(4096));
    let output = filter_fixture(
        &content,
        HealthLineFilter {
            retained_bytes: 64,
            ..HealthLineFilter::default()
        },
        |line| line.contains("oom-kill"),
    );
    assert!(output.complete);
    assert!(output.lines.is_empty());
    assert_eq!(output.lines_scanned, 1);
}

#[test]
fn bounded_filter_treats_the_exact_raw_limit_as_complete() {
    let line = "[ 1.000000] oom-kill:task=java,pid=71\n";
    let content = line.repeat(10);
    let output = filter_fixture(
        &content,
        HealthLineFilter {
            raw_read_limit_bytes: content.len() as u64,
            ..HealthLineFilter::default()
        },
        |line| line.contains("oom-kill"),
    );
    assert!(output.complete);
    assert_eq!(output.lines.len(), 10);
    assert_eq!(output.raw_bytes_read, content.len() as u64);
}

#[test]
fn bounded_filter_reports_incomplete_one_byte_past_the_raw_limit() {
    let line = "[ 1.000000] oom-kill:task=java,pid=71\n";
    let content = line.repeat(10);
    let output = filter_fixture(
        &content,
        HealthLineFilter {
            raw_read_limit_bytes: content.len() as u64 - 1,
            ..HealthLineFilter::default()
        },
        |line| line.contains("oom-kill"),
    );
    assert!(!output.complete);
    assert_eq!(output.raw_bytes_read, content.len() as u64 - 1);
    assert_eq!(output.lines.len(), 10);
    assert_eq!(output.lines_scanned, 10);
}

#[test]
fn bounded_filter_stress_full_raw_limit_stays_within_check_budget() {
    let mut content = String::with_capacity(5 * 1024 * 1024);
    while content.len() < 5 * 1024 * 1024 {
        content.push_str(
            "[ 1234.567890] audit: type=1400 audit(1700000000.000:1234): unrelated noise record\n",
        );
    }
    let started = Instant::now();
    let output = filter_fixture(&content, HealthLineFilter::default(), |line| {
        line.contains("oom-kill")
    });
    let elapsed = started.elapsed();
    eprintln!(
        "stress raw_limit: offered_bytes={} read_bytes={} lines={} retained={} elapsed_ms={}",
        content.len(),
        output.raw_bytes_read,
        output.lines_scanned,
        output.lines.len(),
        elapsed.as_millis()
    );
    assert!(!output.complete);
    assert_eq!(output.raw_bytes_read, 4 * 1024 * 1024);
    assert!(output.lines.is_empty());
    assert!(elapsed < Duration::from_millis(120));
}

#[test]
fn bounded_filter_stress_single_byte_chunks_stay_within_check_budget() {
    let mut content = String::new();
    while content.len() < 1024 * 1024 {
        content.push_str("[ 1234.567890] audit: unrelated noise record\n");
    }
    content.push_str("[ 5000.000000] oom-kill:constraint=CONSTRAINT_MEMCG,task=java,pid=71\n");
    let started = Instant::now();
    let output = filter_fixture_chunked(
        &content,
        HealthLineFilter::default(),
        |line| line.contains("oom-kill"),
        1,
    );
    let elapsed = started.elapsed();
    eprintln!(
        "stress single_byte_chunks: bytes={} lines={} retained={} elapsed_ms={}",
        content.len(),
        output.lines_scanned,
        output.lines.len(),
        elapsed.as_millis()
    );
    assert!(output.complete);
    assert_eq!(output.lines.len(), 1);
    // One-byte chunking is an artificial worst case; production reads 4096-byte chunks.
    let budget = if cfg!(debug_assertions) {
        Duration::from_millis(1000)
    } else {
        Duration::from_millis(120)
    };
    assert!(elapsed < budget);
}

#[test]
fn filtered_command_reports_a_missing_program() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let result = run_health_command_filtered(
        "/nonexistent/cosh-oom-probe-9978",
        &[],
        HealthCommandConfig::default(),
        HealthLineFilter::default(),
        |_| true,
    );
    assert_eq!(result, Err(HealthCommandError::CommandMissing));
}

#[test]
fn filtered_command_keeps_evidence_when_the_child_exits_nonzero() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let output = run_health_command_filtered(
        "/bin/sh",
        &[
            "-c",
            "printf '[ 1.000000] oom-kill:constraint=CONSTRAINT_MEMCG,task=java,pid=71\\n'; exit 3",
        ],
        HealthCommandConfig::default(),
        HealthLineFilter::default(),
        |line| line.contains("oom-kill"),
    )
    .expect("nonzero exit still yields confirmed evidence");
    assert_eq!(output.exit_code, Some(3));
    assert_eq!(output.filtered.lines.len(), 1);
    assert!(output.filtered.complete);
}

#[test]
fn filtered_command_bounds_a_stderr_flood_and_keeps_evidence() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("temporary directory");
    let fixture = directory.path().join("stderr.log");
    let mut noise = String::new();
    while noise.len() < 512 * 1024 {
        noise.push_str("dmesg: read kernel buffer failed: Operation not permitted\n");
    }
    std::fs::write(&fixture, &noise).expect("fixture");
    let output = run_health_command_filtered(
        "/bin/sh",
        &[
            "-c",
            "printf '[ 1.000000] oom-kill:constraint=CONSTRAINT_MEMCG,task=java,pid=71\\n'; cat \"$1\" >&2",
            "sh",
            fixture.to_str().expect("path"),
        ],
        HealthCommandConfig::default(),
        HealthLineFilter::default(),
        |line| line.contains("oom-kill"),
    )
    .expect("stderr flood does not discard evidence");
    assert_eq!(output.filtered.lines.len(), 1);
    assert!(output.stderr.len() <= 16 * 1024);
}

#[test]
fn filtered_command_repeated_deadline_cuts_reclaim_child_slots() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let directory = tempfile::tempdir().expect("temporary directory");
    let fixture = directory.path().join("dmesg.log");
    let mut content =
        String::from("[ 1.000000] oom-kill:constraint=CONSTRAINT_MEMCG,task=java,pid=71\n");
    while content.len() < 16 * 1024 * 1024 {
        content.push_str("[ 1234.567890] audit: unrelated noise record for the ring buffer\n");
    }
    std::fs::write(&fixture, &content).expect("fixture");
    let started = Instant::now();
    for round in 0..4 {
        let output = run_health_command_filtered(
            "/bin/sh",
            &["-c", "cat \"$1\"", "sh", fixture.to_str().expect("path")],
            HealthCommandConfig {
                timeout: Duration::from_millis(120),
                output_limit_bytes: 16 * 1024,
            },
            HealthLineFilter {
                raw_read_limit_bytes: 32 * 1024 * 1024,
                ..HealthLineFilter::default()
            },
            |line| line.contains("oom-kill"),
        )
        .unwrap_or_else(|error| panic!("round {round} lost the child slot: {error:?}"));
        assert_eq!(output.filtered.lines.len(), 1);
        assert!(!output.filtered.complete);
    }
    let elapsed = started.elapsed();
    eprintln!(
        "stress repeated_deadline_cuts: rounds=4 elapsed_ms={}",
        elapsed.as_millis()
    );
    assert!(elapsed < Duration::from_millis(2000));
}
