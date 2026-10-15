use super::*;
use crate::raw_input::UserPtyInputGeneration;

const TEST_MARKER_TOKEN: &str = "test-marker-token";

#[test]
fn relay_wake_bytes_are_coalesced_and_fully_drained() {
    let (master, _master_writer) = nix::unistd::pipe().expect("master pipe");
    let (mut wake_reader, mut wake_writer) = UnixStream::pair().expect("wake pair");
    let (mut resize_reader, _resize_writer) = UnixStream::pair().expect("resize pair");
    wake_reader.set_nonblocking(true).expect("nonblocking wake");
    resize_reader
        .set_nonblocking(true)
        .expect("nonblocking resize");
    wake_writer
        .set_nonblocking(true)
        .expect("nonblocking writer");
    wake_writer.write_all(&[1; 128]).expect("queue wakes");

    let activity = wait_for_relay_activity(
        master.as_raw_fd(),
        &mut wake_reader,
        &mut resize_reader,
        Duration::from_millis(50),
    )
    .expect("wait for wake");
    assert_eq!(
        activity,
        RelayActivity {
            pty: false,
            wake: true,
            resize: false,
        }
    );

    let drained = wait_for_relay_activity(
        master.as_raw_fd(),
        &mut wake_reader,
        &mut resize_reader,
        Duration::from_millis(2),
    )
    .expect("wait after drain");
    assert_eq!(drained, RelayActivity::default());
}

#[test]
fn relay_wait_returns_when_pty_becomes_readable() {
    let (master, master_writer) = nix::unistd::pipe().expect("master pipe");
    let (mut wake_reader, _wake_writer) = UnixStream::pair().expect("wake pair");
    let (mut resize_reader, _resize_writer) = UnixStream::pair().expect("resize pair");
    wake_reader.set_nonblocking(true).expect("nonblocking wake");
    resize_reader
        .set_nonblocking(true)
        .expect("nonblocking resize");
    nix::unistd::write(&master_writer, b"output").expect("write master output");

    let activity = wait_for_relay_activity(
        master.as_raw_fd(),
        &mut wake_reader,
        &mut resize_reader,
        Duration::from_secs(1),
    )
    .expect("wait for pty");
    assert!(activity.pty);
    assert!(!activity.wake);
    assert!(!activity.resize);
}

#[test]
fn relay_wait_distinguishes_resize_from_regular_wake() {
    let (master, _master_writer) = nix::unistd::pipe().expect("master pipe");
    let (mut wake_reader, _wake_writer) = UnixStream::pair().expect("wake pair");
    let (mut resize_reader, mut resize_writer) = UnixStream::pair().expect("resize pair");
    wake_reader.set_nonblocking(true).expect("nonblocking wake");
    resize_reader
        .set_nonblocking(true)
        .expect("nonblocking resize");
    resize_writer.write_all(&[1]).expect("queue resize");

    let activity = wait_for_relay_activity(
        master.as_raw_fd(),
        &mut wake_reader,
        &mut resize_reader,
        Duration::from_secs(1),
    )
    .expect("wait for resize");
    assert!(!activity.pty);
    assert!(!activity.wake);
    assert!(activity.resize);
}

fn parser_for_test(name: &str) -> OscParser {
    let dir = std::env::temp_dir().join(format!("cosh-raw-relay-{name}"));
    OscParser::new(name.to_string(), dir, TEST_MARKER_TOKEN.to_string())
}

fn tracker_for_test() -> (UserPtyInputGeneration, PromptReplayTracker) {
    let generation = UserPtyInputGeneration::default();
    let tracker = PromptReplayTracker::new(generation.clone());
    (generation, tracker)
}

fn feed_shell_ready(parser: &mut OscParser) {
    let mut marker = Vec::new();
    marker.extend_from_slice(b"\x1b]1337;COSH;");
    marker.extend_from_slice(
        br#"{"event":"precmd","token":"test-marker-token","status":0,"cwd":"/tmp"}"#,
    );
    marker.push(b'\x07');
    parser.feed(&marker).expect("feed precmd");
}

fn feed_enhanced_prompt_ready(parser: &mut OscParser) {
    parser
        .feed(
            b"\x1b]1337;COSH;{\"event\":\"precmd\",\"token\":\"test-marker-token\",\"status\":0,\"cwd\":\"/tmp\",\"prompt_ready\":true}\x07",
        )
        .expect("feed enhanced prompt ready");
}

#[test]
fn prompt_snapshot_publishes_only_after_pty_is_drained() {
    for (name, drained, expected) in [
        ("not-drained", false, b"".as_slice()),
        ("drained", true, b"prompt> ".as_slice()),
    ] {
        let mut parser = parser_for_test(name);
        let generation = UserPtyInputGeneration::default();
        parser.set_prompt_epoch_exchange(generation.prompt_epoch_exchange());
        feed_enhanced_prompt_ready(&mut parser);
        parser.feed(b"prompt> ").expect("feed prompt");

        publish_prompt_snapshot_if_drained(&parser, drained);
        generation.bump();
        parser
            .feed(b"\x1b]1337;COSH;{\"e\":\"slash_guard\",\"t\":\"test-marker-token\"}\x07")
            .expect("arm slash guard");

        assert_eq!(
            parser
                .pending_slash_guard_prompt_for_test()
                .expect("pending slash guard"),
            expected
        );
    }
}

#[test]
fn buffered_pty_output_stays_before_queued_candidate_redraw() {
    let mut parser = parser_for_test("buffered-output-before-redraw");
    let (_generation, mut prompt_replay) = tracker_for_test();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut output = Vec::new();
    let mut display_start = 0usize;
    let mut echoed = 0usize;
    let mut prompt_presentation = PromptPresentation::new(false);

    parser
        .feed(b"BACKGROUND\r\n")
        .expect("feed buffered PTY output");
    sender
        .send(crate::raw_input::RawInputEvent::CandidateRedraw {
            input: b"/cancel".to_vec(),
            hint: None,
        })
        .expect("queue candidate redraw");
    write_pending_display(
        &parser,
        &mut output,
        &mut display_start,
        &mut prompt_replay,
        &mut prompt_presentation,
    )
    .expect("write older PTY output");
    drain_raw_input_events(
        &receiver,
        &mut parser,
        &mut output,
        "prompt> ",
        &mut echoed,
        &mut prompt_replay,
        &prompt_presentation,
        None,
        None,
    )
    .expect("draw queued candidate");

    assert_eq!(output, b"BACKGROUND\r\n\r\x1b[2Kprompt> /cancel");
}

#[test]
fn capture_submission_and_representation_reset_retire_old_generations() {
    let mut parser = parser_for_test("capture-ack-lifecycle");
    parser.push_capture_event(
        crate::types::ShellCaptureLifecycle::Submitted,
        7,
        Some("question"),
        Some("question-1"),
    );
    assert_eq!(
        latest_capture_submission_generation(&parser.events),
        Some(7)
    );

    parser.push_capture_event(crate::types::ShellCaptureLifecycle::Drained, 7, None, None);
    assert_eq!(latest_capture_submission_generation(&parser.events), None);
    for (observer_fails, already_submitted, focused_deny) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (true, true, false),
        (false, true, true),
        (false, false, true),
    ] {
        assert_representation_reset_rejects_inflight_read(
            observer_fails,
            already_submitted,
            focused_deny,
        );
    }
    assert_split_capture_batch_is_drained_before_observer_invalidation();
}

fn assert_split_capture_batch_is_drained_before_observer_invalidation() {
    use crate::raw_input::RawInputCapture;
    use std::sync::mpsc;

    let capture = RawInputCapture::Approval {
        id: "split-approval".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    let mode = Arc::new(Mutex::new(RawInputMode::Hold));
    update_input_mode(
        &mode,
        &RawObserverAction::CaptureInput(capture.clone()),
        None,
    );
    let old_generation = match &*mode.lock().expect("initial split capture") {
        RawInputMode::Capture { generation, .. } => *generation,
        mode => panic!("expected initial capture, got {mode:?}"),
    };
    let (sender, receiver) = mpsc::channel();
    let (submitted_tx, submitted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let producer = thread::spawn({
        let mode = mode.clone();
        let capture = capture.clone();
        move || {
            let mut locked = mode.lock().expect("producer mode lock");
            *locked = RawInputMode::Submitted {
                capture,
                generation: old_generation,
            };
            sender
                .send(RawInputEvent::CaptureSubmitted {
                    kind: "approval",
                    target_id: "split-approval".to_string(),
                    generation: old_generation,
                })
                .expect("first half of capture batch");
            submitted_tx.send(()).expect("submission visible");
            release_rx.recv().expect("release second half");
            sender
                .send(RawInputEvent::CardApprove("split-approval".to_string()))
                .expect("late capture action");
            drop(locked);
        }
    });
    submitted_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("split submission");
    let mut parser = parser_for_test("split-capture-batch");
    let mut output = Vec::new();
    let (_, mut prompt_replay) = tracker_for_test();
    let presentation = PromptPresentation::new(false);
    let mut echoed = 0;
    drain_raw_input_events(
        &receiver,
        &mut parser,
        &mut output,
        "prompt> ",
        &mut echoed,
        &mut prompt_replay,
        &presentation,
        None,
        None,
    )
    .expect("first drain");
    assert!(!parser
        .events
        .iter()
        .any(|event| event.message.as_deref() == Some("approve")));
    let reset = CaptureRepresentationReset::default();
    let (observer_started_tx, observer_started_rx) = mpsc::channel();
    let observer = thread::spawn({
        let mode = mode.clone();
        move || {
            observer_started_tx
                .send(())
                .expect("observer starts waiting for mode");
            observe_with_input_mode_lock(
                &mut |view: ShellEventView<'_>, _: &mut Vec<u8>| {
                    let old_action = view
                        .events()
                        .iter()
                        .position(|event| event.message.as_deref() == Some("approve"))
                        .expect("late old action must enter the same invalidation snapshot");
                    let exclusive_fence = view.base() + view.events().len();
                    assert!(view.base() + old_action < exclusive_fence);
                    reset.signal();
                    Ok(RawObserverAction::CaptureInput(capture.clone()))
                },
                &mut parser,
                &mut output,
                &mode,
                &reset,
                &mut |parser, output| {
                    assert!(
                        mode.try_lock().is_err(),
                        "second drain must hold the mode lock"
                    );
                    drain_raw_input_events(
                        &receiver,
                        parser,
                        output,
                        "prompt> ",
                        &mut echoed,
                        &mut prompt_replay,
                        &presentation,
                        None,
                        None,
                    )?;
                    Ok(())
                },
            )
            .expect("observer invalidation after full old batch drain");
        }
    });
    observer_started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("observer queued");
    release_tx
        .send(())
        .expect("complete producer batch before unlocking");
    producer.join().expect("join capture producer");
    observer.join().expect("join draining observer");
    assert!(
        matches!(*mode.lock().expect("replacement capture"), RawInputMode::Capture { generation, .. } if generation != old_generation)
    );
}

struct ControlledCaptureReader {
    started: std::sync::mpsc::Sender<()>,
    batches: std::sync::mpsc::Receiver<Vec<u8>>,
}

impl Read for ControlledCaptureReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.started.send(()).is_err() {
            return Ok(0);
        }
        let bytes = self.batches.recv().unwrap_or_default();
        buffer[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}

fn assert_representation_reset_rejects_inflight_read(
    observer_fails: bool,
    already_submitted: bool,
    focused_deny: bool,
) {
    use crate::raw_input::{spawn_raw_input_relay, MainPromptGate, RawInputCapture};
    use std::sync::mpsc;

    let capture = RawInputCapture::Approval {
        id: "same-approval".to_string(),
        action_set: crate::ui::ApprovalActionSet::StandardHighRisk,
    };
    let mode = Arc::new(Mutex::new(RawInputMode::Hold));
    update_input_mode(
        &mode,
        &RawObserverAction::CaptureInput(capture.clone()),
        None,
    );
    let old_generation = match &*mode.lock().expect("mode") {
        RawInputMode::Capture { generation, .. } => *generation,
        mode => panic!("expected capture, got {mode:?}"),
    };
    let (transport, receiver) = UnixStream::pair().expect("capture transport");
    let mut slave = File::from(std::os::fd::OwnedFd::from(receiver));
    let flags = unsafe { nix::libc::fcntl(slave.as_raw_fd(), nix::libc::F_GETFL) };
    assert_eq!(
        unsafe {
            nix::libc::fcntl(
                slave.as_raw_fd(),
                nix::libc::F_SETFL,
                flags | nix::libc::O_NONBLOCK,
            )
        },
        0
    );
    let (started_tx, started_rx) = mpsc::channel();
    let (batch_tx, batch_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let driver = spawn_raw_input_relay(
        ControlledCaptureReader {
            started: started_tx,
            batches: batch_rx,
        },
        File::from(std::os::fd::OwnedFd::from(transport)),
        event_tx,
        crate::input::InputClassifier::default(),
        mode.clone(),
        UserPtyInputGeneration::default(),
        MainPromptGate::default(),
        false,
    );
    started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("old read started");
    if focused_deny {
        batch_tx
            .send(b"\x1b[C".to_vec())
            .expect("select Deny in the real reader");
        assert_eq!(
            event_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("queued Deny focus"),
            RawInputEvent::CardFocus("same-approval".to_string(), 1)
        );
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("read after queued Deny focus");
    }
    if already_submitted {
        batch_tx
            .send(b"\necho forbidden-old-batch\n".to_vec())
            .expect("queued old approval and same-batch suffix");
        loop {
            let event = event_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("queued old approval");
            match event {
                RawInputEvent::CardApprove(id) => {
                    assert!(!focused_deny);
                    assert_eq!(id, "same-approval");
                    break;
                }
                RawInputEvent::CardDeny(id) => {
                    assert!(focused_deny);
                    assert_eq!(id, "same-approval");
                    break;
                }
                _ => {}
            }
        }
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("submitted-generation read started");
        assert!(
            matches!(*mode.lock().expect("submitted mode"), RawInputMode::Submitted { generation, .. } if generation == old_generation)
        );
    }
    let reset = CaptureRepresentationReset::default();
    let mut parser = parser_for_test(if observer_fails {
        "reset-error"
    } else {
        "reset-success"
    });
    let mut output = Vec::new();
    let result = observe_with_input_mode_lock(
        &mut |_, output: &mut Vec<u8>| {
            reset.signal();
            if observer_fails {
                return Err(io::Error::other("replacement write failed"));
            }
            output.write_all(b"complete replacement card")?;
            output.flush()?;
            Ok(RawObserverAction::CaptureInput(capture.clone()))
        },
        &mut parser,
        &mut output,
        &mode,
        &reset,
        &mut |_, _| Ok(()),
    );
    assert_eq!(result.is_err(), observer_fails);
    if observer_fails {
        assert!(matches!(
            *mode.lock().expect("failed presentation mode"),
            RawInputMode::Hold
        ));
    }
    if !already_submitted {
        batch_tx
            .send(b"\necho forbidden-old-batch\n".to_vec())
            .expect("release old read");
    }
    let rejected = event_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("old batch rejection");
    let expected_rejected_len =
        b"\necho forbidden-old-batch\n".len() - usize::from(already_submitted);
    assert!(
        matches!(rejected, RawInputEvent::CaptureInputRejected { generation, byte_len }
        if generation == old_generation && byte_len == expected_rejected_len),
        "{rejected:?}"
    );
    if already_submitted {
        batch_tx
            .send(b"\necho forbidden-old-batch\n".to_vec())
            .expect("release submitted-generation read after suffix rejection");
    }
    started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("next read started");
    if observer_fails {
        observe_with_input_mode_lock(
            &mut |_, output: &mut Vec<u8>| {
                output.write_all(b"complete replacement card")?;
                output.flush()?;
                Ok(RawObserverAction::CaptureInput(capture.clone()))
            },
            &mut parser,
            &mut output,
            &mode,
            &reset,
            &mut |_, _| Ok(()),
        )
        .expect("successful replacement");
        // The read already started in Hold, so it cannot approve the newly published card.
        batch_tx.send(b"\n".to_vec()).expect("finish held read");
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("new capture read started");
    }
    let new_generation = match &*mode.lock().expect("replacement mode") {
        RawInputMode::Capture { generation, .. } => *generation,
        mode => panic!("expected replacement capture, got {mode:?}"),
    };
    assert_ne!(old_generation, new_generation);
    batch_tx.send(b"\n".to_vec()).expect("new approval input");
    let mut decisions = 0;
    loop {
        let event = event_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("new capture event");
        match event {
            RawInputEvent::CardApprove(id) => {
                assert!(
                    !focused_deny,
                    "replacement still highlights Deny; fresh Enter must not approve"
                );
                assert_eq!(id, "same-approval");
                decisions += 1;
                break;
            }
            RawInputEvent::CardDeny(id) => {
                assert!(focused_deny);
                assert_eq!(id, "same-approval");
                decisions += 1;
                break;
            }
            RawInputEvent::CaptureSubmitted { generation, .. } => {
                assert_eq!(generation, new_generation);
            }
            _ => {}
        }
    }
    assert_eq!(decisions, 1);
    let mut bytes = [0; 256];
    assert!(
        matches!(slave.read(&mut bytes), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    batch_tx.send(Vec::new()).expect("stop reader");
    driver
        .join()
        .expect("join capture worker")
        .expect("capture worker");
    assert!(!event_rx.try_iter().any(|event| matches!(
        event,
        RawInputEvent::CardApprove(_) | RawInputEvent::CardDeny(_)
    )));
}

#[test]
fn handoff_prompt_restore_strips_duplicate_prompt_echo() {
    assert_committed_handoff_preserves_transport_checks();
    let mut parser = parser_for_test("handoff-prompt-restore");
    feed_shell_ready(&mut parser);
    parser.feed(b"bash-4.4$ ").expect("feed prompt");
    let mut display_start = parser.display.len();
    let (_generation, mut prompt_replay) = tracker_for_test();
    let mut prompt_presentation = PromptPresentation::new(false);
    let mut output = Vec::new();

    restore_prompt_display_before_handoff(
        &parser,
        &mut output,
        &mut display_start,
        &mut prompt_replay,
        &mut prompt_presentation,
    )
    .expect("restore prompt");

    assert_eq!(String::from_utf8_lossy(&output), "bash-4.4$ ");
    assert!(prompt_replay.is_armed());

    parser
        .feed(b"bash-4.4$ echo ok\r\n")
        .expect("feed echoed handoff");
    write_pending_display(
        &parser,
        &mut output,
        &mut display_start,
        &mut prompt_replay,
        &mut prompt_presentation,
    )
    .expect("write echoed handoff");

    assert_eq!(String::from_utf8_lossy(&output), "bash-4.4$ echo ok\r\n");
    assert!(!prompt_replay.is_armed());
}

#[test]
fn user_pty_input_expires_armed_prompt_replay_before_echo_is_parsed() {
    let mut parser = parser_for_test("user-input-expires-replay");
    feed_shell_ready(&mut parser);
    parser.feed(b"prompt> \x1b[?2004h").expect("feed prompt");
    let mut display_start = parser.display.len();
    let (generation, mut prompt_replay) = tracker_for_test();
    let mut prompt_presentation = PromptPresentation::new(false);
    let mut output = Vec::new();

    restore_prompt_display_before_handoff(
        &parser,
        &mut output,
        &mut display_start,
        &mut prompt_replay,
        &mut prompt_presentation,
    )
    .expect("restore prompt");
    assert!(prompt_replay.is_armed());

    // A real empty Enter: the relay bumps the generation before writing to
    // the PTY, so its echo (bracketed-paste toggle + CRLF + fresh prompt)
    // must not be deduplicated as the replayed prompt.
    generation.bump();
    parser
        .feed(b"\x1b[?2004l\r\r\n")
        .expect("feed empty enter accept");
    write_pending_display(
        &parser,
        &mut output,
        &mut display_start,
        &mut prompt_replay,
        &mut prompt_presentation,
    )
    .expect("write empty enter accept");
    parser
        .feed(b"prompt> \x1b[?2004h")
        .expect("feed fresh prompt");
    write_pending_display(
        &parser,
        &mut output,
        &mut display_start,
        &mut prompt_replay,
        &mut prompt_presentation,
    )
    .expect("write fresh prompt");

    assert_eq!(
        String::from_utf8_lossy(&output),
        "prompt> \x1b[?2004h\x1b[?2004l\r\r\nprompt> \x1b[?2004h"
    );
    assert!(!prompt_replay.is_armed());
}

#[test]
fn relay_write_event_expires_armed_prompt_replay() {
    let (generation, mut prompt_replay) = tracker_for_test();

    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(prompt_replay.is_armed());

    // The write event for a user keystroke travels through the event
    // channel ahead of the echo bytes it triggers.
    prompt_replay.observe_user_write(generation.bump(), 1);

    assert!(!prompt_replay.is_armed());
}

#[test]
fn pty_user_writes_emit_independent_prompt_evidence_barriers() {
    // Production wiring must invalidate both cwd evidence and prompt occupancy
    // even when submit detection cannot see a custom `accept-line` binding.
    // Cwd evidence is restored only by `ShellReady`, while every precmd restores
    // prompt occupancy and independently re-arms that barrier.
    let mut parser = parser_for_test("pty-write-cwd-barrier");
    let (generation, mut prompt_replay) = tracker_for_test();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut output = Vec::new();
    let mut echoed = 0usize;
    let prompt_presentation = PromptPresentation::new(false);

    let barrier_count = |parser: &OscParser, component| {
        parser
            .events
            .iter()
            .filter(|event| {
                event.component.as_deref() == Some(component)
                    && event.message.as_deref() == Some("write")
            })
            .count()
    };

    for _ in 0..2 {
        sender
            .send(crate::raw_input::RawInputEvent::PtyUserWrite {
                generation: generation.bump(),
                line_submits: 0,
            })
            .expect("queue pty write");
    }
    drain_raw_input_events(
        &receiver,
        &mut parser,
        &mut output,
        "prompt> ",
        &mut echoed,
        &mut prompt_replay,
        &prompt_presentation,
        None,
        None,
    )
    .expect("drain pty writes");
    assert_eq!(
        barrier_count(&parser, "shell_pty_input"),
        1,
        "consecutive writes must collapse into one cwd barrier event"
    );

    // A fresh command-less prompt report re-arms the barrier, so the
    // next write emits a new invalidation event.
    feed_shell_ready(&mut parser);
    sender
        .send(crate::raw_input::RawInputEvent::PtyUserWrite {
            generation: generation.bump(),
            line_submits: 0,
        })
        .expect("queue post-prompt write");
    drain_raw_input_events(
        &receiver,
        &mut parser,
        &mut output,
        "prompt> ",
        &mut echoed,
        &mut prompt_replay,
        &prompt_presentation,
        None,
        None,
    )
    .expect("drain post-prompt write");
    assert_eq!(
        barrier_count(&parser, "shell_pty_input"),
        2,
        "a command-less prompt report must re-arm the cwd barrier"
    );

    parser
        .feed(
            b"\x1b]1337;COSH;{\"event\":\"preexec\",\"token\":\"test-marker-token\",\"command\":\"true\",\"cwd\":\"/tmp\"}\x07",
        )
        .expect("feed preexec");
    feed_shell_ready(&mut parser);
    sender
        .send(crate::raw_input::RawInputEvent::PtyUserWrite {
            generation: generation.bump(),
            line_submits: 0,
        })
        .expect("queue post-command write");
    drain_raw_input_events(
        &receiver,
        &mut parser,
        &mut output,
        "prompt> ",
        &mut echoed,
        &mut prompt_replay,
        &prompt_presentation,
        None,
        None,
    )
    .expect("drain post-command write");

    assert_eq!(
        barrier_count(&parser, "shell_pty_input"),
        2,
        "command completion must not restore invalidated cwd evidence"
    );
    assert_eq!(
        barrier_count(&parser, "shell_prompt_input"),
        3,
        "command completion must re-arm the prompt occupancy barrier"
    );
}

#[test]
fn composer_slash_retains_workspace_before_the_next_shell_ready() {
    let mut parser = parser_for_test("composer-workspace");
    let (generation, mut prompt_replay) = tracker_for_test();
    let (sender, receiver) = std::sync::mpsc::channel();
    sender
        .send(RawInputEvent::PtyUserWrite {
            generation: generation.bump(),
            line_submits: 0,
        })
        .unwrap();
    sender
        .send(RawInputEvent::PromptDraftSubmit {
            id: "composer-1".into(),
            text: "/health".into(),
            slash: true,
            workspace_cwd: Some("/workspace/project".into()),
        })
        .unwrap();
    drain_raw_input_events(
        &receiver,
        &mut parser,
        &mut Vec::new(),
        "prompt$ ",
        &mut 0,
        &mut prompt_replay,
        &PromptPresentation::new(false),
        None,
        None,
    )
    .unwrap();
    assert!(!parser
        .events
        .iter()
        .any(|event| event.kind == ShellEventKind::ShellReady));
    let intercepts = parser
        .events
        .iter()
        .filter(|event| event.component.as_deref() == Some("slash"))
        .collect::<Vec<_>>();
    assert_eq!(intercepts.len(), 1);
    assert_eq!(intercepts[0].input.as_deref(), Some("/health"));
    assert_eq!(intercepts[0].cwd.as_deref(), Some("/workspace/project"));
}

#[test]
fn candidate_hint_uses_terminfo_cursor_save_restore() {
    let mut parser = parser_for_test("candidate-cursor-restore");
    let (_generation, mut prompt_replay) = tracker_for_test();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut output = Vec::new();
    let mut echoed = 0usize;
    let prompt_presentation = PromptPresentation::new(false);

    sender
        .send(crate::raw_input::RawInputEvent::CandidateRedraw {
            input: b"/m".to_vec(),
            hint: Some("/mode approval [recommend|auto|trust]".to_string()),
        })
        .expect("queue candidate redraw");
    drain_raw_input_events(
        &receiver,
        &mut parser,
        &mut output,
        "",
        &mut echoed,
        &mut prompt_replay,
        &prompt_presentation,
        None,
        None,
    )
    .expect("draw candidate hint");

    assert_eq!(
        output,
        b"\x1b[K/m\x1b7\x1b[?7l\x1b[2m /mode approval [recommend|auto|trust]\x1b[0m\x1b[?7h\x1b8"
    );
    assert_eq!(echoed, 2);
    assert!(!output.windows(3).any(|window| window == b"\x1b[s"));
    assert!(!output.windows(3).any(|window| window == b"\x1b[u"));
}

#[test]
fn isolated_candidate_repaints_do_not_add_status_lines() {
    let mut parser = parser_for_test("candidate-prompt-owner");
    let (_generation, mut prompt_replay) = tracker_for_test();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut output = Vec::new();
    let mut echoed = 0usize;
    // Status symbols enabled: candidate redraws must still not publish.
    let prompt_presentation = PromptPresentation::new(true);

    sender
        .send(crate::raw_input::RawInputEvent::CandidateRedraw {
            input: "你好".as_bytes().to_vec(),
            hint: None,
        })
        .expect("queue candidate redraw");
    sender
        .send(crate::raw_input::RawInputEvent::CandidateCommit(
            "你好".as_bytes().to_vec(),
        ))
        .expect("queue candidate commit");
    sender
        .send(crate::raw_input::RawInputEvent::CandidateClearLine)
        .expect("queue candidate clear");

    drain_raw_input_events(
        &receiver,
        &mut parser,
        &mut output,
        "prompt> ",
        &mut echoed,
        &mut prompt_replay,
        &prompt_presentation,
        None,
        None,
    )
    .expect("draw isolated candidate states");

    assert_eq!(
        String::from_utf8(output).expect("utf8 output"),
        "\r\x1b[2Kprompt> 你好\r\x1b[2Kprompt> 你好\n\r\x1b[2Kprompt> "
    );
}

// A wrapped hint tail would land below the erase-to-EOL reach of the next
// redraw/commit, so the hint write must keep auto-wrap disabled in both
// the native and the prompt-owned branches.
#[test]
fn candidate_hint_disables_autowrap_in_both_branches() {
    for prompt in ["", "prompt> "] {
        let mut parser = parser_for_test("candidate-hint-autowrap");
        let (_generation, mut prompt_replay) = tracker_for_test();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut output = Vec::new();
        let mut echoed = 0usize;
        let prompt_presentation = PromptPresentation::new(false);

        sender
            .send(crate::raw_input::RawInputEvent::CandidateRedraw {
                input: b"/s".to_vec(),
                hint: Some("/status · /stats · /session · /skills".to_string()),
            })
            .expect("queue candidate redraw");
        drain_raw_input_events(
            &receiver,
            &mut parser,
            &mut output,
            prompt,
            &mut echoed,
            &mut prompt_replay,
            &prompt_presentation,
            None,
            None,
        )
        .expect("draw candidate hint");

        let rendered = String::from_utf8(output).expect("utf8 output");
        let disable = rendered.find("\x1b[?7l").expect("auto-wrap disabled");
        let hint = rendered.find("/status · /stats").expect("hint rendered");
        let enable = rendered.find("\x1b[?7h").expect("auto-wrap restored");
        assert!(
            disable < hint && hint < enable,
            "hint must render inside the no-wrap window: {rendered:?}"
        );
    }
}

// A prompt ghost can be longer than the remaining terminal row. Its clear
// restores the saved prompt cursor and erases that physical line, so a wrapped
// tail would survive below the next draft as stale screen content.
#[test]
fn prompt_ghost_disables_autowrap_before_restoring_the_cursor() {
    let mut output = Vec::new();

    write_prompt_ghost(
        &mut output,
        "Analyze and handle the previous input that did not run successfully",
        false,
    )
    .expect("render prompt ghost");

    let rendered = String::from_utf8(output).expect("utf8 output");
    let disable = rendered.find("\x1b[?7l").expect("auto-wrap disabled");
    let ghost = rendered.find("Analyze and handle").expect("ghost rendered");
    let enable = rendered.find("\x1b[?7h").expect("auto-wrap restored");
    let restore = rendered.find(RESTORE_CURSOR).expect("cursor restored");
    assert!(
        disable < ghost && ghost < enable && enable < restore,
        "ghost must render inside the no-wrap window: {rendered:?}"
    );
}

struct OneShotFailingWriter {
    output: Vec<u8>,
    fail_after: usize,
    failed: bool,
}

impl Write for OneShotFailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.failed && self.output.len() >= self.fail_after {
            self.failed = true;
            return Err(io::Error::other("injected prompt ghost failure"));
        }
        if !self.failed && self.output.len() + bytes.len() > self.fail_after {
            let accepted = self.fail_after - self.output.len();
            self.output.extend_from_slice(&bytes[..accepted]);
            return Ok(accepted);
        }
        self.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn prompt_ghost_restores_terminal_after_a_recoverable_partial_write() {
    let fail_after = b"\x1b7\x1b[?7l\x1b[2mAnalyze".len();
    let mut output = OneShotFailingWriter {
        output: Vec::new(),
        fail_after,
        failed: false,
    };

    let error = write_prompt_ghost(
        &mut output,
        "Analyze and handle the previous input that did not run successfully",
        false,
    )
    .expect_err("body write must preserve the injected error");

    assert_eq!(error.to_string(), "injected prompt ghost failure");
    assert!(output.failed, "writer must exercise its one-shot failure");
    assert!(
        output.output.ends_with(b"\x1b[0m\x1b[?7h\x1b8"),
        "recoverable error must restore SGR, autowrap, and cursor: {:?}",
        String::from_utf8_lossy(&output.output)
    );
}

#[test]
fn overlay_does_not_restore_an_unpaired_cursor_after_save_failure() {
    let mut output = OneShotFailingWriter {
        output: Vec::new(),
        fail_after: 0,
        failed: false,
    };

    let error = write_no_wrap_overlay(&mut output, |output| output.write_all(b"body"))
        .expect_err("cursor save must preserve the injected error");

    assert_eq!(error.to_string(), "injected prompt ghost failure");
    assert_eq!(output.output, b"\x1b[0m\x1b[?7h");
    assert!(
        !output.output.windows(2).any(|bytes| bytes == b"\x1b8"),
        "a failed DECSC must not consume an older cursor save"
    );
}

#[test]
fn overlay_returns_first_cleanup_error_after_restoring_later_state() {
    let body = b"\x1b[2m cleanup";
    let fail_after = SAVE_CURSOR.len() + b"\x1b[?7l".len() + body.len();
    let mut output = OneShotFailingWriter {
        output: Vec::new(),
        fail_after,
        failed: false,
    };

    let error = write_no_wrap_overlay(&mut output, |output| output.write_all(body))
        .expect_err("first cleanup write must preserve the injected error");

    assert_eq!(error.to_string(), "injected prompt ghost failure");
    assert!(output.failed, "writer must exercise its cleanup failure");
    assert!(
        output.output.ends_with(b"\x1b[?7h\x1b8"),
        "later cleanup must restore autowrap and cursor: {:?}",
        String::from_utf8_lossy(&output.output)
    );
}

#[test]
fn prompt_restore_refuses_to_arm_while_user_input_response_is_unparsed() {
    let (generation, mut prompt_replay) = tracker_for_test();

    // An empty Enter reached the PTY inside the Delay window; no prompt
    // boundary has confirmed its response was parsed yet.
    generation.bump();

    prompt_replay.arm_for_replay(b"prompt> ");

    assert!(!prompt_replay.is_armed());
}

#[test]
fn handoff_refuses_to_arm_over_pending_user_input() {
    let (generation, mut prompt_replay) = tracker_for_test();

    // A user write raced in after the output loop's last event drain and
    // before the handoff prompt restore runs: bump -> arm -> user echo.
    generation.bump();
    prompt_replay.arm_for_replay(b"prompt> \x1b[?2004h");
    assert!(!prompt_replay.is_armed());

    // The user's accept-line echo must pass through untouched.
    assert_eq!(
        prompt_replay.strip(b"\x1b[?2004l\r\r\n"),
        b"\x1b[?2004l\r\r\n"
    );
    assert_eq!(
        prompt_replay.strip(b"prompt> \x1b[?2004h"),
        b"prompt> \x1b[?2004h"
    );
}

#[test]
fn prompt_restore_arms_again_after_the_response_reaches_a_prompt_boundary() {
    let (generation, mut prompt_replay) = tracker_for_test();

    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(!prompt_replay.is_armed());

    // precmd boundary: the Enter response is fully parsed.
    prompt_replay.observe_prompt_boundary();
    prompt_replay.arm_for_replay(b"prompt> ");

    assert!(prompt_replay.is_armed());
}

#[test]
fn typeahead_submission_blocks_arm_until_its_own_prompt_boundary() {
    let (generation, mut prompt_replay) = tracker_for_test();

    // A command line, then an empty Enter typed ahead while it runs; both
    // write events are drained before the command's precmd arrives.
    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.observe_user_write(generation.bump(), 1);

    // The command's precmd only accounts for the command's own submission,
    // so a command insight or handoff must not arm over the queued Enter.
    prompt_replay.observe_prompt_boundary();
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(!prompt_replay.is_armed());

    // The empty Enter's own boundary settles the ledger.
    prompt_replay.observe_prompt_boundary();
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(prompt_replay.is_armed());
}

#[test]
fn uncounted_input_holds_occupancy_through_a_quiet_painted_prompt() {
    let (generation, mut prompt_replay) = tracker_for_test();

    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.observe_user_write(generation.bump(), 0);
    assert!(prompt_replay.observe_prompt_boundary());

    // A prefix key bound to accept-line fires only after the line editor's
    // key timeout, so even a long quiet painted prompt proves nothing about
    // the unclassified input; the counted submit alone is written off.
    prompt_replay.set_idle_reconcile_window(Duration::ZERO);
    assert!(!prompt_replay.reconcile_idle_at_prompt(false, true, None));
    assert!(!prompt_replay.has_unconsumed_submit());
    assert!(prompt_replay.observe_prompt_boundary());
}

#[test]
fn uncounted_write_holds_prompt_occupancy_in_every_shell() {
    // zsh `bindkey` widgets are as invisible to byte counting as Readline
    // `bind` ones, so no shell may treat a zero-count write as pure editing.
    let (generation, mut prompt_replay) = tracker_for_test();
    prompt_replay.set_idle_reconcile_window(Duration::ZERO);

    prompt_replay.observe_user_write(generation.bump(), 0);
    assert!(prompt_replay.observe_prompt_boundary());
    // Nothing can release the hold, so the relay must not schedule idle
    // wakeups for it either.
    assert_eq!(
        prompt_replay.idle_reconcile_remaining(false, true, None),
        None
    );
    assert!(!prompt_replay.reconcile_idle_at_prompt(false, true, None));
    assert!(prompt_replay.observe_prompt_boundary());
}

#[test]
fn consecutive_uncounted_submissions_hold_through_every_command_boundary() {
    let (generation, mut prompt_replay) = tracker_for_test();

    prompt_replay.observe_user_write(generation.bump(), 1);
    // One coalesced PTY write may contain any number of custom accept-line
    // keys, so it deliberately records no guessed submission count.
    prompt_replay.observe_user_write(generation.bump(), 0);

    // The foreground command and both queued commands each produce a
    // boundary, but none is proof that the line editor exhausted the queue.
    for _ in 0..3 {
        assert!(prompt_replay.observe_prompt_boundary());
        assert!(!prompt_replay.reconcile_idle_at_prompt(true, true, None));
    }

    // Idling at the last prompt is no proof either.
    prompt_replay.set_idle_reconcile_window(Duration::ZERO);
    assert!(!prompt_replay.reconcile_idle_at_prompt(false, true, None));
    assert!(prompt_replay.observe_prompt_boundary());
}

#[test]
fn idle_write_off_restores_replay_dedup_but_keeps_held_occupancy() {
    let (generation, mut prompt_replay) = tracker_for_test();
    prompt_replay.set_idle_reconcile_window(Duration::ZERO);

    // `read value` plus a typed reply consumed by `read`: the reply's counted
    // submit never gets a boundary, and the typed keys are unclassified.
    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.observe_user_write(generation.bump(), 0);
    prompt_replay.observe_user_write(generation.bump(), 1);
    assert!(prompt_replay.observe_prompt_boundary());
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(!prompt_replay.is_armed());

    // The write-off recovers dedup without claiming the prompt is free.
    assert!(!prompt_replay.reconcile_idle_at_prompt(false, true, None));
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(prompt_replay.is_armed());
    assert!(prompt_replay.observe_prompt_boundary());
}

#[test]
fn held_occupancy_still_arms_replay_and_passes_accept_line_responses() {
    let (generation, mut prompt_replay) = tracker_for_test();

    prompt_replay.observe_user_write(generation.bump(), 0);
    assert!(prompt_replay.observe_prompt_boundary());

    // Plain editing keys look exactly like custom accept-line keys; arming
    // keeps a panel restore from repainting the prompt twice.
    prompt_replay.arm_for_replay(b"prompt> \x1b[?2004h");
    assert!(prompt_replay.is_armed());

    // A hidden accept-line response opens with the paste disable, so the
    // fail-open strip leaves it and the following prompt paint intact.
    assert_eq!(
        prompt_replay.strip(b"\x1b[?2004l\r\r\n"),
        b"\x1b[?2004l\r\r\n"
    );
    assert_eq!(
        prompt_replay.strip(b"prompt> \x1b[?2004h"),
        b"prompt> \x1b[?2004h"
    );
}

#[test]
fn several_enters_in_one_write_block_arm_until_all_boundaries_arrive() {
    let (generation, mut prompt_replay) = tracker_for_test();

    // One relay write carrying two accept-line keys (e.g. "\r\r").
    prompt_replay.observe_user_write(generation.bump(), 2);

    prompt_replay.observe_prompt_boundary();
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(!prompt_replay.is_armed());

    prompt_replay.observe_prompt_boundary();
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(prompt_replay.is_armed());
}

#[test]
fn boundary_without_a_matching_submission_is_ignored() {
    let (generation, mut prompt_replay) = tracker_for_test();

    // A Ctrl-C style prompt repaint yields a boundary with no submission.
    prompt_replay.observe_prompt_boundary();

    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(!prompt_replay.is_armed());

    prompt_replay.observe_prompt_boundary();
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(prompt_replay.is_armed());
}

#[test]
fn intercepted_submission_does_not_block_arming_over_its_repaint() {
    let (generation, mut prompt_replay) = tracker_for_test();

    // A recalled slash line is submitted and killed by the DEBUG trap; its
    // precmd has not arrived when the panel's RestorePrompt fires, but its
    // only remaining response is the repaint the arm exists to strip.
    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.observe_intercept_cut();

    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(prompt_replay.is_armed());
}

#[test]
fn typeahead_enter_still_blocks_arming_alongside_an_intercept() {
    let (generation, mut prompt_replay) = tracker_for_test();

    // Recalled slash + empty Enter in one write: only the slash line was
    // intercepted; the Enter's response is still pending.
    prompt_replay.observe_user_write(generation.bump(), 2);
    prompt_replay.observe_intercept_cut();

    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(!prompt_replay.is_armed());
}

#[test]
fn uncounted_custom_accept_line_intercept_never_swallows_typeahead_enter() {
    let (generation, mut prompt_replay) = tracker_for_test();

    // `.inputrc` binds C-x to accept-line: the intercepted slash line's
    // submission is invisible to the byte counter, so the typeahead Enter in
    // the same write is the only counted submit and the intercept cut lets
    // the panel restore arm over it.
    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.observe_intercept_cut();
    prompt_replay.arm_for_replay(b"prompt> \x1b[?2004h");
    assert!(prompt_replay.is_armed());

    // Bash's post-intercept repaint opens with the prompt and is deduped.
    assert_eq!(prompt_replay.strip(b"prompt> \x1b[?2004h"), b"");
    // The queued Enter's response never opens with the prompt bytes, so the
    // mis-attributed arm cannot swallow it: toggle, CRLF, and the fresh
    // prompt all reach the terminal verbatim.
    assert_eq!(
        prompt_replay.strip(b"\x1b[?2004l\r\r\n"),
        b"\x1b[?2004l\r\r\n"
    );
    assert_eq!(
        prompt_replay.strip(b"prompt> \x1b[?2004h"),
        b"prompt> \x1b[?2004h"
    );
}

#[test]
fn foreground_consumed_submission_is_written_off_at_an_idle_prompt() {
    let (generation, mut prompt_replay) = tracker_for_test();
    prompt_replay.set_idle_reconcile_window(Duration::from_millis(0));

    // `read value` (one submit) plus "hello\r" consumed by `read` itself:
    // only the command's precmd arrives, leaving a permanent +1 balance.
    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.observe_prompt_boundary();

    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(!prompt_replay.is_armed());

    // Shell idles at the painted prompt: the orphaned submission is
    // reconciled and replay dedup recovers for the rest of the session.
    prompt_replay.reconcile_idle_at_prompt(false, true, None);
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(prompt_replay.is_armed());
}

#[test]
fn idle_reconcile_is_refused_while_a_command_is_running() {
    let (generation, mut prompt_replay) = tracker_for_test();
    prompt_replay.set_idle_reconcile_window(Duration::from_millis(0));

    // Command submitted, then a typeahead Enter while it runs.
    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.observe_prompt_boundary();

    // A long-running command keeps the PTY silent, but the typeahead Enter
    // is still queued: it must not be written off.
    prompt_replay.reconcile_idle_at_prompt(true, true, None);
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(!prompt_replay.is_armed());
}

#[test]
fn idle_reconcile_is_refused_until_the_prompt_is_painted() {
    let (generation, mut prompt_replay) = tracker_for_test();
    prompt_replay.set_idle_reconcile_window(Duration::from_millis(0));

    // Command plus a typeahead Enter; the command's precmd marker arrived
    // but the user's slow PROMPT_COMMAND still runs, so no prompt bytes are
    // painted and readline has not consumed the queued Enter yet.
    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.observe_user_write(generation.bump(), 1);
    prompt_replay.observe_prompt_boundary();

    // However long the silence lasts, the ledger must survive until the
    // shell actually paints a prompt: a precmd marker alone is not consent.
    prompt_replay.reconcile_idle_at_prompt(false, false, None);
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(!prompt_replay.is_armed());

    // Once the prompt is painted, readline echoes the queued Enter and its
    // own boundary settles the ledger through the normal path.
    prompt_replay.observe_prompt_boundary();
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(prompt_replay.is_armed());
}

#[test]
fn idle_reconcile_waits_for_pty_and_relay_silence() {
    let (generation, mut prompt_replay) = tracker_for_test();

    prompt_replay.observe_user_write(generation.bump(), 1);

    // A write just drained: the reconcile window has not elapsed, so the
    // pending submission survives (its echo may still be in flight).
    prompt_replay.reconcile_idle_at_prompt(false, true, Some(Instant::now()));
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(!prompt_replay.is_armed());

    prompt_replay.set_idle_reconcile_window(Duration::from_millis(0));
    prompt_replay.reconcile_idle_at_prompt(false, true, None);
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(prompt_replay.is_armed());
}

#[test]
fn idle_reconcile_is_refused_while_a_write_is_undrained() {
    let (generation, mut prompt_replay) = tracker_for_test();
    prompt_replay.set_idle_reconcile_window(Duration::from_millis(0));

    prompt_replay.observe_user_write(generation.bump(), 1);
    // A second write bumped the shared generation but its event has not
    // been drained yet: the ledger is not settled.
    generation.bump();

    prompt_replay.reconcile_idle_at_prompt(false, true, None);
    prompt_replay.arm_for_replay(b"prompt> ");
    assert!(!prompt_replay.is_armed());
}

#[test]
fn prompt_restore_arms_when_no_user_input_followed_the_intercept() {
    let (_generation, mut prompt_replay) = tracker_for_test();

    prompt_replay.arm_for_replay(b"prompt> ");

    assert!(prompt_replay.is_armed());
}

#[test]
fn prompt_restore_waits_through_passive_observer_cycles() {
    let restore = RawObserverAction::RestorePrompt {
        ghost_text: Some("analyze failure".to_string()),
        ghost_route: Default::default(),
    };
    let mut pending = None;
    remember_pending_prompt_restore(&restore, &mut pending);

    assert_eq!(
        merge_pending_prompt_restore(RawObserverAction::Continue, &mut pending),
        restore
    );
    assert!(pending.is_none());
}

#[test]
fn active_observer_action_supersedes_waiting_prompt_restore() {
    let restore = RawObserverAction::RestorePrompt {
        ghost_text: Some("analyze failure".to_string()),
        ghost_route: Default::default(),
    };
    let mut pending = Some(restore);

    let observed = RawObserverAction::HoldShellOutput;
    assert_eq!(
        merge_pending_prompt_restore(observed.clone(), &mut pending),
        observed
    );
    assert!(pending.is_none());
}

#[test]
fn foreground_passthrough_cancels_waiting_prompt_restore() {
    let restore = RawObserverAction::RestorePrompt {
        ghost_text: Some("analyze failure".to_string()),
        ghost_route: Default::default(),
    };
    let mut pending = Some(restore);

    assert_eq!(
        merge_pending_prompt_restore(RawObserverAction::RawPassthrough, &mut pending),
        RawObserverAction::RawPassthrough
    );
    assert!(pending.is_none());
}

#[test]
fn prompt_fragment_after_restore_keeps_ghost_last_on_screen() {
    let mut parser = parser_for_test("fragmented-prompt-ghost");
    feed_shell_ready(&mut parser);
    parser
        .feed(b"\x1b]0;root@host\x07")
        .expect("feed title fragment");
    let mut output = Vec::new();
    let mut display_start = 0;
    let (_generation, mut prompt_replay) = tracker_for_test();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut pending_terminal_restore = PendingTerminalRecovery::default();
    let mut prompt_presentation = PromptPresentation::new(false);
    let mut null = File::open("/dev/null").expect("open null");

    let action = resolve_pty_emit(
        &mut null,
        1,
        -1,
        &mut parser,
        &mut output,
        &input_mode,
        RawObserverAction::RestorePrompt {
            ghost_text: Some("objdump".to_string()),
            ghost_route: Default::default(),
        },
        &mut display_start,
        &mut prompt_replay,
        &mut prompt_presentation,
        &mut pending_terminal_restore,
        Path::new("/tmp/cosh-test-recovery"),
        Path::new("/tmp/cosh-test-handoff"),
        false,
        false,
        None,
    )
    .expect("restore prompt");
    assert_eq!(action, RawObserverAction::Continue);

    parser
        .feed(b"\x1b[?2004h[root@host]# ")
        .expect("feed prompt fragment");
    write_pending_display_preserving_prompt_ghost(
        &parser,
        &mut output,
        &mut display_start,
        &mut prompt_replay,
        &mut prompt_presentation,
        &input_mode,
    )
    .expect("write prompt fragment");

    assert!(
        output.ends_with(b"\x1b7\x1b[?7l\x1b[2m objdump\x1b[0m\x1b[?7h\x1b8"),
        "{}",
        String::from_utf8_lossy(&output)
    );
}

struct FailedHandoffDisplay {
    fail_flush: bool,
}

impl Write for FailedHandoffDisplay {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("handoff prompt write failed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            Err(io::Error::other("handoff display flush failed"))
        } else {
            Ok(())
        }
    }
}

fn assert_committed_handoff_preserves_transport_checks() {
    for (restore_prompt, fail_flush) in [(false, true), (true, true), (true, false)] {
        for case in 0..5 {
            let directory = tempfile::tempdir().expect("handoff stage directory");
            let stage = directory.path().join("handoff");
            if case == 3 {
                std::fs::create_dir(&stage).expect("block mandatory staging");
            }
            let (transport, receiver) = UnixStream::pair().expect("handoff transport");
            let mut master = File::from(std::os::fd::OwnedFd::from(transport));
            let mut slave = File::from(std::os::fd::OwnedFd::from(receiver));
            let flags = unsafe { nix::libc::fcntl(slave.as_raw_fd(), nix::libc::F_GETFL) };
            assert_eq!(
                unsafe {
                    nix::libc::fcntl(
                        slave.as_raw_fd(),
                        nix::libc::F_SETFL,
                        flags | nix::libc::O_NONBLOCK,
                    )
                },
                0
            );
            let mut readonly_master = File::open("/dev/null").expect("nonwritable transport");
            let mut parser = parser_for_test("committed-handoff-display-failure");
            feed_shell_ready(&mut parser);
            parser.feed(b"prompt> ").expect("painted prompt");
            let mut display_start = parser.display.len();
            let (_generation, mut replay) = tracker_for_test();
            let mut presentation = PromptPresentation::new(false);
            let mut recovery = PendingTerminalRecovery::default();
            let mode = Arc::new(Mutex::new(RawInputMode::Hold));
            let mut request = crate::types::ShellHandoffRequest::new(
                "printf committed-effect",
                "$ printf committed-effect",
                "approved_provider_shell_tool",
                "user",
                "req-1",
                "run-1",
                1,
            )
            .expect("valid approved request");
            if case == 2 {
                request.command.push('\n');
            }
            let action = if restore_prompt {
                RawObserverAction::EmitToPtyWithPromptRestore(request)
            } else {
                RawObserverAction::EmitToPty(request)
            };
            let result = resolve_pty_emit(
                if case == 4 {
                    &mut readonly_master
                } else {
                    &mut master
                },
                1,
                slave.as_raw_fd(),
                &mut parser,
                &mut FailedHandoffDisplay { fail_flush },
                &mode,
                action,
                &mut display_start,
                &mut replay,
                &mut presentation,
                &mut recovery,
                &directory.path().join("recovery"),
                &stage,
                false,
                case != 1,
                None,
            );
            let mut bytes = [0; 256];
            if case == 0 {
                assert_eq!(
                    result.expect("a committed effect must survive display failure"),
                    RawObserverAction::RawPassthrough
                );
                let count = slave
                    .read(&mut bytes)
                    .expect("approved bytes reached the transport");
                assert_eq!(&bytes[..count], b"printf committed-effect\n");
                assert!(
                    matches!(slave.read(&mut bytes), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
                );
                assert_eq!(
                    std::fs::read_to_string(&stage).expect("mandatory stage"),
                    "printf committed-effect"
                );
            } else {
                assert!(
                    result.is_err(),
                    "transport/security error cannot be hidden: case={case}"
                );
                assert!(
                    matches!(slave.read(&mut bytes), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
                );
                if case == 4 {
                    assert!(!stage.exists(), "failed PTY write clears staged authority");
                }
            }
        }
    }
}
