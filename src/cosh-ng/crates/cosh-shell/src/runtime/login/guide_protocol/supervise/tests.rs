//! Deterministic witnesses for the supervision guards.
use super::*;
use std::io::{Seek, SeekFrom};

const OPERATION: &str = "op-test";
const USER: &str = "alice";
const UID: u32 = 1000;
const TARGET: &str = "/usr/bin/cosh";
const TEST_BUDGET: Duration = Duration::from_millis(120);
const TEST_GRACE: Duration = Duration::from_millis(60);
const TEST_POLL: Duration = Duration::from_millis(2);

// Per-thread signal slot: the production slot is process-wide, so sharing it
// would let one test's supervision loop consume another test's signal.
thread_local! {
    static TEST_SIGNAL: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

fn take_test_signal() -> Option<i32> {
    TEST_SIGNAL.with(|slot| slot.take())
}

fn publish_test_signal(signal: i32) {
    TEST_SIGNAL.with(|slot| slot.set(Some(signal)));
}

fn terminal_present() -> bool {
    false
}

fn terminal_gone() -> bool {
    true
}

fn probes(terminal_lost: fn() -> bool) -> Probes {
    Probes {
        terminal_lost,
        take_signal: take_test_signal,
        publish_signal: publish_test_signal,
    }
}

fn ready_record() -> String {
    format!(
        "{{\"version\":{},\"kind\":\"ready\",\"operation_id\":\"{}\",\"worker_pid\":4242}}\n",
        VERSION, OPERATION
    )
}

fn final_record() -> String {
    format!(
            "{{\"version\":{},\"outcome\":\"ok\",\"username\":\"{}\",\"uid\":{},\"current_shell\":\"{}\",\"original_bash\":null,\"managed\":true,\"conflict\":false,\"error\":null,\"operation_id\":\"{}\"}}\n",
            VERSION, USER, UID, TARGET, OPERATION
        )
}

/// Scripted stand-in for the privileged worker. It speaks the same
/// Ready/grant/Final protocol over the same files, so the supervision
/// guards are exercised without spawning a process or opening a PTY.
struct FakeWorker {
    out: File,
    grant_read: File,
    ready_written: bool,
    ready_on_poll: bool,
    granted: Option<bool>,
    on_grant: &'static str,
    exit_after_final: bool,
    exited: bool,
    requested_stop: bool,
    forced: bool,
    withdrawn_polls: usize,
    saw_second_grant: bool,
}

impl FakeWorker {
    fn new(
        result: &tempfile::NamedTempFile,
        ready_on_poll: bool,
        on_grant: &'static str,
    ) -> (Self, File) {
        let (read_end, write_end) = nix::unistd::pipe().expect("grant pipe");
        let grant_read = File::from(read_end);
        let flags = unsafe { nix::libc::fcntl(grant_read.as_raw_fd(), nix::libc::F_GETFL) };
        unsafe {
            nix::libc::fcntl(
                grant_read.as_raw_fd(),
                nix::libc::F_SETFL,
                flags | nix::libc::O_NONBLOCK,
            );
        }
        let mut out = result.as_file().try_clone().expect("result clone");
        out.seek(SeekFrom::Start(0)).expect("seek");
        let mut worker = Self {
            out,
            grant_read,
            ready_written: false,
            ready_on_poll,
            granted: None,
            on_grant,
            exit_after_final: false,
            exited: false,
            requested_stop: false,
            forced: false,
            withdrawn_polls: 0,
            saw_second_grant: false,
        };
        if !ready_on_poll {
            worker.write_ready();
        }
        (worker, File::from(write_end))
    }

    fn write_ready(&mut self) {
        let _ = self.out.write_all(ready_record().as_bytes());
        let _ = self.out.flush();
        self.ready_written = true;
    }
}

impl Worker for FakeWorker {
    fn poll(&mut self) -> Poll {
        if self.exited {
            return Poll::Exited;
        }
        // sudo can refuse before the worker ever starts: no Ready, no grant.
        if self.on_grant == "exit-before-ready" {
            self.exited = true;
            return Poll::Exited;
        }
        if self.ready_on_poll && !self.ready_written {
            self.write_ready();
        }
        if self.granted.is_none() {
            let mut byte = [0u8; 1];
            if let Ok(1) = self.grant_read.read(&mut byte) {
                self.granted = Some(byte[0] == b'1');
                match (self.granted, self.on_grant) {
                    (Some(true), "final") => {
                        let _ = self.out.write_all(final_record().as_bytes());
                        let _ = self.out.flush();
                        self.exit_after_final = true;
                    }
                    (Some(true), "final-lingering") => {
                        let _ = self.out.write_all(final_record().as_bytes());
                        let _ = self.out.flush();
                    }
                    (Some(true), "flood") => {
                        let _ = self.out.write_all(&vec![b'x'; MAX_STREAM_BYTES + 1024]);
                        let _ = self.out.flush();
                    }
                    (Some(false), "ready-after-withdrawal") => {
                        // A worker that keeps publishing Ready after the
                        // grant was withdrawn must never be granted.
                        self.ready_written = false;
                        self.write_ready();
                        self.withdrawn_polls = 0;
                    }
                    // Real sudo is still waiting for a password here, so it never
                    // reads the withdrawn grant. Bound the stand-in so a regression
                    // fails the assertion instead of hanging the suite.
                    (Some(false), "stuck-after-withdrawal") => {}
                    (Some(false), _) => self.exited = true,
                    _ => {}
                }
            }
        }
        if self.granted == Some(false) && self.on_grant == "ready-after-withdrawal" {
            let mut byte = [0u8; 1];
            if let Ok(1) = self.grant_read.read(&mut byte) {
                if byte[0] == b'1' {
                    self.saw_second_grant = true;
                    let _ = self.out.write_all(final_record().as_bytes());
                    let _ = self.out.flush();
                }
            }
            self.withdrawn_polls += 1;
            if self.withdrawn_polls >= 4 {
                self.exited = true;
            }
        }
        if self.granted == Some(false) && self.on_grant == "stuck-after-withdrawal" {
            self.withdrawn_polls += 1;
            if self.withdrawn_polls >= 2000 {
                self.exited = true;
            }
        }
        if self.exit_after_final && !self.requested_stop {
            self.exited = true;
        }
        if self.exited {
            Poll::Exited
        } else {
            Poll::Running
        }
    }

    fn request_stop(&mut self) {
        self.requested_stop = true;
    }

    fn force_stop(&mut self, _worker_pid: Option<u32>) {
        self.forced = true;
        self.exited = true;
    }
}

fn run(
    ready_on_poll: bool,
    on_grant: &'static str,
    probe: fn() -> bool,
    signal: Option<i32>,
) -> (Outcome, String, bool, std::time::Duration) {
    let _ = take_test_signal();
    let mut result = tempfile::NamedTempFile::new().expect("result file");
    let (mut worker, grant) = FakeWorker::new(&result, ready_on_poll, on_grant);
    if let Some(signal) = signal {
        publish_test_signal(signal);
    }
    let started = Instant::now();
    let outcome = supervise_with_budget(
        &mut worker,
        &mut result,
        grant,
        OPERATION,
        USER,
        UID,
        TARGET,
        TEST_BUDGET,
        TEST_GRACE,
        TEST_POLL,
        &probes(probe),
    );
    let elapsed = started.elapsed();
    let raw = std::fs::read_to_string(result.path()).unwrap_or_default();
    let forced = worker.forced;
    let _ = take_test_signal();
    (outcome, raw, forced, elapsed)
}

#[test]
fn a_cancel_before_ready_never_grants_a_commit() {
    let (outcome, raw, _forced, _elapsed) =
        run(true, "final", terminal_present, Some(nix::libc::SIGINT));
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(
        !raw.contains("\"outcome\""),
        "a withdrawn grant must not produce a result: {raw}"
    );
}

#[test]
fn a_withdrawn_grant_stops_a_child_that_never_notices_it() {
    let (outcome, raw, forced, elapsed) = run(
        false,
        "stuck-after-withdrawal",
        terminal_present,
        Some(nix::libc::SIGTERM),
    );
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(
        forced,
        "a signal sent only to this process never reaches sudo; the child must be stopped"
    );
    assert!(!raw.contains("\"outcome\""), "{raw}");
    assert!(
        elapsed < TEST_GRACE * 8,
        "withdrawal did not converge within the stop grace: {elapsed:?}"
    );
}

#[test]
fn a_child_that_exits_before_any_grant_is_denied_not_unconfirmed() {
    let (outcome, raw, _forced, _elapsed) = run(true, "exit-before-ready", terminal_present, None);
    assert_eq!(outcome, Outcome::Denied);
    assert!(
        !raw.contains("\"outcome\""),
        "a refusal must not produce a worker result: {raw}"
    );
}

#[test]
fn a_lost_terminal_withdraws_authorization_before_any_commit() {
    let (outcome, raw, _forced, _elapsed) = run(false, "final", terminal_gone, None);
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(!raw.contains("\"outcome\""), "{raw}");
}

#[test]
fn a_commit_past_the_deadline_is_unconfirmed_and_forced_to_stop() {
    let (outcome, _raw, forced, elapsed) = run(false, "silent", terminal_present, None);
    assert_eq!(outcome, Outcome::Unconfirmed);
    assert!(forced, "the deadline must stop a worker that ignores TERM");
    assert!(
        elapsed < Duration::from_secs(5),
        "commit deadline did not bound the wait: {elapsed:?}"
    );
}

#[test]
fn an_oversized_record_stream_is_unconfirmed() {
    let (outcome, _raw, forced, elapsed) = run(false, "flood", terminal_present, None);
    assert_eq!(outcome, Outcome::Unconfirmed);
    assert!(forced, "truncation must still reap the worker");
    assert!(
        elapsed < Duration::from_secs(5),
        "stream truncation did not bound the wait: {elapsed:?}"
    );
}

#[test]
fn a_granted_worker_result_is_classified_after_it_exits() {
    let (outcome, raw, _forced, _elapsed) = run(false, "final", terminal_present, None);
    assert!(raw.contains("\"kind\":\"ready\""), "{raw}");
    match outcome {
        Outcome::Verified(status) => {
            assert_eq!(status.username, USER);
            assert_eq!(status.uid, UID);
            assert_eq!(status.current_shell, TARGET);
        }
        other => panic!("expected a verified result, got {other:?}"),
    }
}

#[test]
fn a_result_from_a_worker_that_will_not_exit_is_still_reaped() {
    let (outcome, _raw, forced, elapsed) = run(false, "final-lingering", terminal_present, None);
    assert!(matches!(outcome, Outcome::Verified(_)), "{outcome:?}");
    assert!(forced, "a lingering worker must be reaped before returning");
    assert!(
        elapsed < Duration::from_secs(5),
        "a lingering worker kept the wizard waiting: {elapsed:?}"
    );
}

#[test]
fn a_termination_signal_is_republished_for_the_wizard() {
    let _ = take_test_signal();
    let mut result = tempfile::NamedTempFile::new().expect("result file");
    let (mut worker, grant) = FakeWorker::new(&result, false, "silent");
    publish_test_signal(nix::libc::SIGTERM);
    let outcome = supervise_with_budget(
        &mut worker,
        &mut result,
        grant,
        OPERATION,
        USER,
        UID,
        TARGET,
        TEST_BUDGET,
        TEST_GRACE,
        TEST_POLL,
        &probes(terminal_present),
    );
    assert_eq!(outcome, Outcome::Cancelled);
    assert_eq!(
        take_test_signal(),
        Some(nix::libc::SIGTERM),
        "TERM must still end the wizard after the task converged"
    );
}

/// Witnesses the `cancelled` disjunct: a worker that publishes Ready only
/// after the grant was withdrawn must still be refused, not granted.
#[test]
fn a_ready_published_after_withdrawal_is_never_granted() {
    let _ = take_test_signal();
    let mut result = tempfile::NamedTempFile::new().expect("result file");
    let (mut worker, grant) = FakeWorker::new(&result, true, "ready-after-withdrawal");
    publish_test_signal(nix::libc::SIGINT);
    let outcome = supervise_with_budget(
        &mut worker,
        &mut result,
        grant,
        OPERATION,
        USER,
        UID,
        TARGET,
        TEST_BUDGET,
        TEST_GRACE,
        TEST_POLL,
        &probes(terminal_present),
    );
    let raw = std::fs::read_to_string(result.path()).unwrap_or_default();
    let _ = take_test_signal();
    assert_eq!(outcome, Outcome::Cancelled);
    assert!(
        !worker.saw_second_grant,
        "a withdrawn authorization must never be granted again"
    );
    assert!(
        !raw.contains("\"outcome\""),
        "a refused worker must not produce a result: {raw}"
    );
}
