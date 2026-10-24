use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::super::{PromptGhostCandidate, RawInputCapture};
use super::deadline::{deferred_waits_for_shell_prompt, resolve_held_delivery};
use super::*;

struct IdleReader {
    reads: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

#[test]
fn only_nonempty_shell_lines_add_pending_boundaries() {
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();

    assert!(!is_pending_shell_submission(b"", &state, &classifier));
    assert!(!is_pending_shell_submission(b"  \t", &state, &classifier));
    assert!(!is_pending_shell_submission(
        b"?? what happened",
        &state,
        &classifier
    ));
    assert!(is_pending_shell_submission(b"echo ok", &state, &classifier));

    state.native_line_state.observe_shell_bytes(b"echo ok");
    assert!(is_pending_shell_submission(b"", &state, &classifier));
}

impl Read for IdleReader {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if self.stop.load(Ordering::Relaxed) {
            Ok(0)
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
}

#[test]
fn idle_reader_backs_off_between_would_block_retries() {
    let reads = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::sync_channel(1);
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let reader = thread::spawn({
        let reads = reads.clone();
        let stop = stop.clone();
        move || read_input_chunks(IdleReader { reads, stop }, sender, input_mode, None)
    });

    thread::sleep(Duration::from_millis(120));
    stop.store(true, Ordering::Relaxed);
    assert!(matches!(
        receiver.recv_timeout(Duration::from_secs(1)),
        Ok(InputRead::Eof)
    ));
    reader.join().expect("idle reader");
    assert!(
        reads.load(Ordering::Relaxed) <= 20,
        "idle reader retried {} times",
        reads.load(Ordering::Relaxed)
    );
}

#[test]
fn fd_reader_waits_for_readiness_without_backoff() {
    let (read_fd, write_fd) = nix::unistd::pipe().expect("pipe");
    let flags = unsafe { nix::libc::fcntl(read_fd.as_raw_fd(), nix::libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe {
            nix::libc::fcntl(
                read_fd.as_raw_fd(),
                nix::libc::F_SETFL,
                flags | nix::libc::O_NONBLOCK,
            )
        },
        0
    );
    let input_fd = read_fd.as_raw_fd();
    let input = File::from(read_fd);
    let mut writer = File::from(write_fd);
    let (sender, receiver) = mpsc::sync_channel(1);
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let reader = thread::spawn(move || {
        read_input_chunks(input, sender, input_mode, Some(input_fd));
    });

    thread::sleep(Duration::from_millis(10));
    std::io::Write::write_all(&mut writer, b"x").expect("write input");
    let received = receiver
        .recv_timeout(Duration::from_millis(250))
        .expect("poll wakes for input");
    assert!(matches!(received, InputRead::Bytes { bytes, .. } if bytes == b"x"));

    drop(writer);
    assert!(matches!(
        receiver.recv_timeout(Duration::from_millis(250)),
        Ok(InputRead::Eof)
    ));
    reader.join().expect("fd reader");
}

#[test]
fn completed_delay_escape_is_discarded_before_a_later_capture() {
    let observed = RawInputMode::Delay { generation: 1 };
    let current = RawInputMode::Capture {
        capture: RawInputCapture::Consultation {
            id: "new-capture".to_string(),
        },
        generation: 2,
        installed_at: Instant::now(),
    };

    assert!(stale_delay_escape_reached_interactive_owner(
        &[0x1b],
        &observed,
        &current
    ));
}

#[test]
fn relay_uses_the_validated_mode_snapshot() {
    let path = std::env::temp_dir().join(format!(
        "cosh-shell-validated-mode-snapshot-{}",
        std::process::id()
    ));
    let mut master = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("test output file");
    let (input_tx, input_rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
        capture: RawInputCapture::Question {
            id: "later-capture".to_string(),
            option_count: 0,
            selected: 0,
            allow_free_text: true,
            multiple: false,
            secret: false,
        },
        generation: 2,
        installed_at: Instant::now(),
    }));
    let observed = RawInputMode::Passthrough;

    relay_input_bytes_with_read_ahead(
        b"stale\n",
        Instant::now(),
        &mut master,
        &input_tx,
        &InputClassifier::default(),
        &input_mode,
        &mut RawInputRelayState::default(),
        RelayReadContext {
            read_ahead: None,
            expected_capture_generation: None,
            observed_mode: Some(&observed),
            pending_shell_submits: 0,
        },
    )
    .expect("relay validated snapshot");

    assert!(!input_rx
        .try_iter()
        .any(|event| matches!(event, RawInputEvent::CardInput(_, _))));
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"stale\n");
    fs::remove_file(path).ok();
}

#[test]
fn prompt_ghost_timeout_refreshes_the_validated_snapshot() {
    let path = std::env::temp_dir().join(format!(
        "cosh-shell-prompt-ghost-timeout-snapshot-{}",
        std::process::id()
    ));
    let mut master = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("test output file");
    let (input_tx, input_rx) = mpsc::channel();
    let observed = RawInputMode::PromptGhost {
        text: "inspect this".to_string(),
        route: PromptGhostRoute::AgentSelection {
            candidates: vec![PromptGhostCandidate {
                text: "inspect this".to_string(),
                suggestion_id: "suggestion-1".to_string(),
            }],
            active: 0,
        },
    };
    let input_mode = Arc::new(Mutex::new(observed.clone()));
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();
    let received_at = Instant::now()
        .checked_sub(Duration::from_millis(100))
        .expect("recent timestamp");

    relay_input_bytes(
        b"\x1b",
        received_at,
        &mut master,
        &input_tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("buffer ghost escape");
    relay_input_bytes_with_read_ahead(
        b"\t",
        received_at + Duration::from_millis(51),
        &mut master,
        &input_tx,
        &classifier,
        &input_mode,
        &mut state,
        RelayReadContext {
            read_ahead: None,
            expected_capture_generation: None,
            observed_mode: Some(&observed),
            pending_shell_submits: 0,
        },
    )
    .expect("flush escape and relay tab");

    let events = input_rx.try_iter().collect::<Vec<_>>();
    assert_eq!(fs::read(&path).expect("read test output"), b"\x1b\t");
    assert!(!events.iter().any(|event| matches!(
        event,
        RawInputEvent::PromptGhostAccepted { .. } | RawInputEvent::PromptGhostIntercept { .. }
    )));
    assert!(matches!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::Passthrough
    ));
    fs::remove_file(path).ok();
}

#[test]
fn delayed_ghost_suffix_keeps_capture_generation_across_replacement() {
    let path = std::env::temp_dir().join(format!(
        "cosh-shell-delayed-ghost-suffix-{}",
        std::process::id()
    ));
    let mut master = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("test output file");
    let (input_tx, input_rx) = mpsc::channel();
    let previous = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let next = RawInputCapture::Question {
        id: "q-2".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let candidate = PromptGhostCandidate {
        text: "inspect memory".to_string(),
        suggestion_id: "health-1".to_string(),
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: candidate.text.clone(),
        route: PromptGhostRoute::AgentSelection {
            candidates: vec![candidate],
            active: 0,
        },
    }));
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();
    let received_at = Instant::now();

    relay_input_bytes_with_read_ahead(
        b"\x1b",
        received_at,
        &mut master,
        &input_tx,
        &classifier,
        &input_mode,
        &mut state,
        RelayReadContext::default(),
    )
    .expect("buffer ghost escape");
    *input_mode.lock().expect("input mode") = RawInputMode::Capture {
        capture: previous.clone(),
        generation: 7,
        installed_at: Instant::now(),
    };
    relay_input_bytes_with_read_ahead(
        b"[",
        received_at + Duration::from_millis(1),
        &mut master,
        &input_tx,
        &classifier,
        &input_mode,
        &mut state,
        RelayReadContext {
            read_ahead: None,
            expected_capture_generation: Some(7),
            observed_mode: None,
            pending_shell_submits: 0,
        },
    )
    .expect("buffer partial replaced ghost suffix");
    *input_mode.lock().expect("input mode") = RawInputMode::Draining {
        previous_capture: previous,
        generation: 7,
        next_capture: Some(next),
        invalidated: false,
        post_owner: PostCaptureOwner::MainPrompt,
    };
    let mode = current_raw_input_mode(&input_mode);
    flush_pending_replaced_prompt_ghost_suffix(
        true,
        received_at + Duration::from_millis(60),
        &mode,
        &mut master,
        &input_tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("flush partial suffix");
    finish_input_relay(&mut master, &input_tx, &classifier, &input_mode, &mut state)
        .expect("finish relay");

    let events = input_rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events.iter().any(|event| matches!(
            event,
            RawInputEvent::CardInput(target, _) if target == "q-2"
        )),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn ghost_suffix_does_not_consume_input_from_a_new_capture_generation() {
    let path = std::env::temp_dir().join(format!(
        "cosh-shell-ghost-suffix-generation-boundary-{}",
        std::process::id()
    ));
    let mut master = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("test output file");
    let (input_tx, input_rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
        capture: RawInputCapture::Question {
            id: "q-2".to_string(),
            option_count: 0,
            selected: 0,
            allow_free_text: true,
            multiple: false,
            secret: false,
        },
        generation: 8,
        installed_at: Instant::now(),
    }));
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState {
        pending_replaced_prompt_ghost_suffix: Some(PendingReplacedPromptGhostSuffix {
            bytes: b"[".to_vec(),
            deadline: Instant::now() + Duration::from_millis(50),
            expected_capture_generation: Some(7),
        }),
        ..RawInputRelayState::default()
    };

    relay_input_bytes_with_read_ahead(
        b"Z",
        Instant::now(),
        &mut master,
        &input_tx,
        &classifier,
        &input_mode,
        &mut state,
        RelayReadContext {
            read_ahead: None,
            expected_capture_generation: Some(8),
            observed_mode: None,
            pending_shell_submits: 0,
        },
    )
    .expect("relay new generation input");

    let events = input_rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.iter().any(
            |event| matches!(event, RawInputEvent::CardInput(target, input) if target == "q-2" && input == "Z")
        ),
        "{events:?}"
    );
    finish_input_relay(&mut master, &input_tx, &classifier, &input_mode, &mut state)
        .expect("finish relay");
    let eof_events = input_rx.try_iter().collect::<Vec<_>>();
    assert!(eof_events.contains(&RawInputEvent::CaptureDrained { generation: 8 }));
    assert!(!eof_events.contains(&RawInputEvent::EofShutdownRequested));
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn delay_escape_does_not_cancel_a_later_capture() {
    let path = std::env::temp_dir().join(format!(
        "cosh-shell-delay-escape-capture-boundary-{}",
        std::process::id()
    ));
    let mut master = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("test output file");
    let capture = RawInputCapture::Question {
        id: "new-question".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
        capture: capture.clone(),
        generation: 42,
        installed_at: Instant::now(),
    }));
    let (input_tx, input_rx) = mpsc::channel();
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState {
        pending_delay_escape: Some(PendingDelayEscape {
            bytes: vec![ESC],
            deadline: Instant::now(),
            generation: 7,
        }),
        ..RawInputRelayState::default()
    };

    flush_pending_delay_escape(
        true,
        Instant::now(),
        &mut master,
        &input_tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("flush stale delay escape");

    assert!(input_rx.try_iter().next().is_none());
    assert!(matches!(
        current_raw_input_mode(&input_mode),
        RawInputMode::Capture {
            capture: active,
            generation: 42,
            ..
        } if active == capture
    ));
    master.sync_all().expect("sync test output");
    assert!(fs::read(&path).expect("read test output").is_empty());
    fs::remove_file(path).ok();
}

fn deferred_read(bytes: &[u8], pending_shell_submits: u16) -> InputRead {
    InputRead::Bytes {
        bytes: bytes.to_vec(),
        received_at: Instant::now(),
        observed_mode: RawInputMode::Passthrough,
        ownership_changed_during_read: false,
        pending_shell_submits,
    }
}

fn deferred_pending(read: &InputRead) -> u16 {
    match read {
        InputRead::Bytes {
            pending_shell_submits,
            ..
        } => *pending_shell_submits,
        _ => panic!("bytes read expected"),
    }
}

#[test]
fn deferred_path_prompt_remainder_waits_for_prompt_gate() {
    let base = tempfile::tempdir().expect("temp dir");
    let cwd = base
        .path()
        .canonicalize()
        .expect("canonical temp dir")
        .to_string_lossy()
        .into_owned();
    let classifier = InputClassifier::default();
    classifier.prompt_cwd().set(Some(cwd));
    let state = RawInputRelayState::default();

    // Startup coalescing parks the path line behind a submitted `cd` while
    // the gate is down; it must wait instead of reaching the shell unrouted.
    let remainder = deferred_read("打开./nonexistent-cosh-2913/SKILL.md\n".as_bytes(), 1);
    assert!(deferred_waits_for_shell_prompt(
        &remainder,
        &state,
        &classifier
    ));

    // Once the gate rises the remainder delivers immediately.
    state.main_prompt_gate.set_at_prompt(true);
    assert!(!deferred_waits_for_shell_prompt(
        &remainder,
        &state,
        &classifier
    ));

    // Plain shell type-ahead, slash controls, resolved submissions, and
    // control-bearing batches keep today's immediate delivery.
    state.main_prompt_gate.set_at_prompt(false);
    assert!(!deferred_waits_for_shell_prompt(
        &deferred_read(b"ls -la\n", 1),
        &state,
        &classifier
    ));
    assert!(!deferred_waits_for_shell_prompt(
        &deferred_read(b"/mode routing shell-only\n", 1),
        &state,
        &classifier
    ));
    assert!(!deferred_waits_for_shell_prompt(
        &deferred_read("打开./nonexistent-cosh-2913/SKILL.md\n".as_bytes(), 0),
        &state,
        &classifier
    ));
    assert!(!deferred_waits_for_shell_prompt(
        &deferred_read(b"\x1b[A\n", 1),
        &state,
        &classifier
    ));

    // A slash-bearing leading submission marks a slash candidate batch: the
    // remainder keeps the shell-owned delivery even when it is a path prompt.
    let slash_batch = RawInputRelayState {
        deferred_split_slash_bearing: true,
        ..RawInputRelayState::default()
    };
    assert!(!deferred_waits_for_shell_prompt(
        &deferred_read("打开./nonexistent-cosh-2913/SKILL.md\n".as_bytes(), 1),
        &slash_batch,
        &classifier
    ));

    // Sensitive remainders never wait for a route either; the pending count
    // keeps refusing them into the shell-owned path.
    assert!(!deferred_waits_for_shell_prompt(
        &deferred_read(
            "打开./nonexistent-cosh-2913 API Key: sk-review-fixture\n".as_bytes(),
            1
        ),
        &state,
        &classifier
    ));
}

#[test]
fn held_remainder_resolves_pending_submits_once_gate_rises() {
    let mut state = RawInputRelayState::default();

    // Delivered while never held: the pending count is preserved.
    let delivered = resolve_held_delivery(deferred_read(b"echo ok\n", 1), &mut state);
    assert_eq!(deferred_pending(&delivered), 1);

    // Held behind the gate, then delivered after it rose: the completed
    // submissions resolve so the path-prompt route re-evaluates cleanly.
    state.deferred_prompt_hold_since = Some(Instant::now());
    state.main_prompt_gate.set_at_prompt(true);
    let delivered = resolve_held_delivery(
        deferred_read("打开./nonexistent-cosh-2913/SKILL.md\n".as_bytes(), 1),
        &mut state,
    );
    assert_eq!(deferred_pending(&delivered), 0);
    assert!(state.deferred_prompt_hold_since.is_none());
    assert!(state.deferred_prompt_poll.is_none());
    assert!(state.deferred_prompt_hold_candidate.is_none());
}

#[test]
fn main_prompt_gate_tracks_whether_it_ever_rose() {
    let gate = MainPromptGate::default();
    assert!(!gate.has_ever_been_ready());

    gate.set_at_prompt(true);
    assert!(gate.has_ever_been_ready());

    // Falling back while a submission runs must not forget that this session
    // raises its gate: held remainders stay releaseable.
    gate.set_at_prompt(false);
    assert!(gate.has_ever_been_ready());

    // The startup latch counts as a risen gate too.
    let seeded = MainPromptGate::default();
    seeded.seed_initial_prompt();
    assert!(seeded.has_ever_been_ready());
}

#[test]
fn gateless_session_delivers_deferred_path_prompt_immediately() {
    let base = tempfile::tempdir().expect("temp dir");
    let cwd = base
        .path()
        .canonicalize()
        .expect("canonical temp dir")
        .to_string_lossy()
        .into_owned();
    let classifier = InputClassifier::default();
    classifier.prompt_cwd().set(Some(cwd));

    // A session that never showed a prompt can never re-raise the gate, so
    // holding would only burn the wait cap: the remainder delivers at once
    // (pre-#7897 behavior) instead of parking for 2 s.
    let mut state = RawInputRelayState::default();
    assert!(!state.main_prompt_gate.has_ever_been_ready());
    state.deferred_input = Some(deferred_read(
        "打开./nonexistent-cosh-2913/SKILL.md\n".as_bytes(),
        1,
    ));
    let (sender, receiver) = mpsc::sync_channel(1);
    let input = receive_input(&receiver, &mut state, &classifier)
        .expect("gateless session delivers without parking");
    assert_eq!(deferred_pending(&input), 1);
    assert!(state.deferred_prompt_hold_since.is_none());
    assert!(state.deferred_prompt_poll.is_none());
    drop(sender);

    // A marker session (gate already rose once) parks the same remainder
    // while the gate is down, and the poll re-checks only the gate.
    let mut state = RawInputRelayState::default();
    state.main_prompt_gate.set_at_prompt(true);
    state.main_prompt_gate.set_at_prompt(false);
    assert!(state.main_prompt_gate.has_ever_been_ready());
    state.deferred_input = Some(deferred_read(
        "打开./nonexistent-cosh-2913/SKILL.md\n".as_bytes(),
        1,
    ));
    let (sender, receiver) = mpsc::sync_channel(1);
    let parked = receive_input(&receiver, &mut state, &classifier);
    assert!(
        matches!(parked, Err(RecvTimeoutError::Timeout)),
        "marker session parks the remainder for the gate"
    );
    assert!(state.deferred_input.is_some());
    assert!(state.deferred_prompt_hold_since.is_some());
    assert_eq!(state.deferred_prompt_hold_candidate, Some(true));
    drop(sender);
}
