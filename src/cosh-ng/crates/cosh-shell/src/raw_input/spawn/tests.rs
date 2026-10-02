use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::super::{PromptGhostCandidate, RawInputCapture};
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
