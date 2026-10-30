//! Chain terminal-state matrix tests for the #1913 submit-window input
//! buffering (design T1/T2/T3/T6/T8): a cleanly drained capture chain
//! replays quarantined bytes to the main-prompt owner, every unsafe
//! terminal state rejects them visibly, and nothing leaks into a
//! follow-up capture or the PTY while a capture owns input.

use std::fs::{self, OpenOptions};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::super::*;
use super::{
    drain_abandoned_capture, relay_input_chunk, relay_late_capture_bytes,
    replay_or_reject_after_drain, CaptureOwnedInput,
};
use crate::input::InputClassifier;
use crate::raw_input::{update_input_mode, PromptGhostRoute, RawInputCapture, RawObserverAction};

fn output_file(tag: &str) -> (std::path::PathBuf, std::fs::File) {
    let path = std::env::temp_dir().join(format!(
        "cosh-shell-capture-matrix-{tag}-{}",
        std::process::id()
    ));
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("test output file");
    use std::os::fd::AsRawFd;
    let flags = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GETFL) };
    assert_eq!(
        unsafe {
            nix::libc::fcntl(
                file.as_raw_fd(),
                nix::libc::F_SETFL,
                flags | nix::libc::O_NONBLOCK,
            )
        },
        0
    );
    (path, file)
}

fn config_capture() -> RawInputCapture {
    RawInputCapture::Config {
        id: "config-1".to_string(),
        option_count: 2,
        selected: 0,
    }
}

fn question_capture(id: &str) -> RawInputCapture {
    RawInputCapture::Question {
        id: id.to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    }
}

struct MatrixHarness {
    path: std::path::PathBuf,
    master: std::fs::File,
    input_mode: Arc<Mutex<RawInputMode>>,
    event_tx: mpsc::Sender<RawInputEvent>,
    event_rx: mpsc::Receiver<RawInputEvent>,
    classifier: InputClassifier,
    card_state: CardInputState,
    quarantine: CaptureOwnedInput,
    line_buffer: CandidateLineBuffer,
    native_line_state: NativeLineState,
    exit_tracker: ExplicitExitTracker,
    input_generation: UserPtyInputGeneration,
    line_submits: LineSubmitCounter,
    main_prompt_gate: MainPromptGate,
}

impl MatrixHarness {
    fn new(tag: &str, mode: RawInputMode) -> Self {
        let (path, master) = output_file(tag);
        let (event_tx, event_rx) = mpsc::channel();
        Self {
            path,
            master,
            input_mode: Arc::new(Mutex::new(mode)),
            event_tx,
            event_rx,
            classifier: InputClassifier::default(),
            card_state: CardInputState::default(),
            quarantine: CaptureOwnedInput::default(),
            line_buffer: CandidateLineBuffer::default(),
            native_line_state: NativeLineState::default(),
            exit_tracker: ExplicitExitTracker::default(),
            input_generation: UserPtyInputGeneration::default(),
            line_submits: LineSubmitCounter::default(),
            main_prompt_gate: MainPromptGate::default(),
        }
    }

    fn relay_chunk(&mut self, bytes: &[u8], mode: RawInputMode) {
        let mut deferred_input = None;
        let mut relay = InputRelayContext {
            master: &mut self.master,
            input_classifier: &self.classifier,
            input_events: &self.event_tx,
            input_mode: &self.input_mode,
            input_generation: &self.input_generation,
            line_submits: &mut self.line_submits,
            line_buffer: &mut self.line_buffer,
            native_line_state: &mut self.native_line_state,
            exit_tracker: &mut self.exit_tracker,
            main_prompt_gate: &self.main_prompt_gate,
            slash_route_enabled: false,
            zsh_path_prompt_buffering: None,
        };
        relay_input_chunk(
            bytes,
            mode,
            &mut self.card_state,
            &mut self.quarantine,
            &mut deferred_input,
            RelayReadContext::default(),
            &mut relay,
        )
        .expect("relay chunk");
    }

    fn drain_abandoned(&mut self) {
        self.try_drain_abandoned().expect("drain abandoned capture");
    }

    fn try_drain_abandoned(&mut self) -> std::io::Result<()> {
        let mut relay = InputRelayContext {
            master: &mut self.master,
            input_classifier: &self.classifier,
            input_events: &self.event_tx,
            input_mode: &self.input_mode,
            input_generation: &self.input_generation,
            line_submits: &mut self.line_submits,
            line_buffer: &mut self.line_buffer,
            native_line_state: &mut self.native_line_state,
            exit_tracker: &mut self.exit_tracker,
            main_prompt_gate: &self.main_prompt_gate,
            slash_route_enabled: false,
            zsh_path_prompt_buffering: None,
        };
        drain_abandoned_capture(&mut self.card_state, &mut self.quarantine, &mut relay)
    }

    fn events(&self) -> Vec<RawInputEvent> {
        self.event_rx.try_iter().collect()
    }

    fn pty_bytes(&mut self) -> Vec<u8> {
        self.master.sync_all().expect("sync test output");
        fs::read(&self.path).expect("read test output")
    }
}

impl Drop for MatrixHarness {
    fn drop(&mut self) {
        fs::remove_file(&self.path).ok();
    }
}

/// Acknowledge the submitted capture like the host observer would, using
/// the given action once the relay parks the mode in `Submitted`.
fn spawn_submission_ack(
    input_mode: Arc<Mutex<RawInputMode>>,
    action: RawObserverAction,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if let RawInputMode::Submitted { generation, .. } = current_raw_input_mode(&input_mode)
            {
                update_input_mode(&input_mode, &action, Some(generation));
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("capture never reached the submitted state");
    })
}

/// T1: a burst chunk carrying the submit Enter plus trailing type-ahead
/// replays the type-ahead to the shell once the chain drains cleanly.
#[test]
fn clean_chain_end_replays_submit_window_bytes_to_the_shell() {
    let capture = config_capture();
    let mut harness = MatrixHarness::new(
        "t1-replay",
        RawInputMode::Capture {
            capture: capture.clone(),
            generation: 7,
            installed_at: Instant::now(),
        },
    );
    let ack = spawn_submission_ack(harness.input_mode.clone(), RawObserverAction::Continue);

    harness.relay_chunk(
        b"\recho hi\n",
        RawInputMode::Capture {
            capture,
            generation: 7,
            installed_at: Instant::now(),
        },
    );
    ack.join().expect("ack thread");

    let events = harness.events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RawInputEvent::ConfigSave(id) if id == "config-1")),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CaptureDrained { .. })),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CaptureInputRejected { .. })),
        "{events:?}"
    );
    assert_eq!(harness.pty_bytes(), b"echo hi\n");
    assert!(matches!(
        current_raw_input_mode(&harness.input_mode),
        RawInputMode::Passthrough | RawInputMode::Terminal { .. }
    ));
}

/// T2: when the chain arms a follow-up capture, the quarantined bytes are
/// rejected visibly and never reach the new card or the PTY.
#[test]
fn chain_continuation_rejects_submit_window_bytes() {
    let capture = config_capture();
    let mut harness = MatrixHarness::new(
        "t2-reject",
        RawInputMode::Capture {
            capture: capture.clone(),
            generation: 9,
            installed_at: Instant::now(),
        },
    );
    let next = question_capture("q-follow");
    let ack = spawn_submission_ack(
        harness.input_mode.clone(),
        RawObserverAction::CaptureInput(next.clone()),
    );

    harness.relay_chunk(
        b"\rleaked text\n",
        RawInputMode::Capture {
            capture,
            generation: 9,
            installed_at: Instant::now(),
        },
    );
    ack.join().expect("ack thread");

    let events = harness.events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::CaptureInputRejected { byte_len, .. } if *byte_len == 12
        )),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CardInput(id, _) if id == "q-follow")),
        "{events:?}"
    );
    assert!(harness.pty_bytes().is_empty());
    assert!(matches!(
        current_raw_input_mode(&harness.input_mode),
        RawInputMode::Capture { capture: active, .. } if active == next
    ));
}

/// T3: an unacknowledged submission expires and rejects instead of
/// replaying stale bytes seconds later.
#[test]
fn expired_chain_rejects_submit_window_bytes() {
    let capture = config_capture();
    let mut harness = MatrixHarness::new(
        "t3-expired",
        RawInputMode::Capture {
            capture: capture.clone(),
            generation: 11,
            installed_at: Instant::now(),
        },
    );

    harness.relay_chunk(
        b"\rstale line\n",
        RawInputMode::Capture {
            capture,
            generation: 11,
            installed_at: Instant::now(),
        },
    );

    let events = harness.events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CaptureExpired { .. })),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CaptureInputRejected { .. })),
        "{events:?}"
    );
    assert!(harness.pty_bytes().is_empty());
}

/// T8: buffered submit-window bytes replay before the live bytes that
/// triggered the drain, preserving arrival order.
#[test]
fn abandoned_clean_drain_replays_buffered_bytes_before_live_input() {
    let mut harness = MatrixHarness::new(
        "t8-order",
        RawInputMode::Draining {
            previous_capture: config_capture(),
            generation: 13,
            next_capture: None,
            invalidated: false,
            post_owner: PostCaptureOwner::MainPrompt,
        },
    );
    assert!(!harness.quarantine.observe(b"early\n"));

    harness.relay_chunk(
        b"late\n",
        RawInputMode::Draining {
            previous_capture: config_capture(),
            generation: 13,
            next_capture: None,
            invalidated: false,
            post_owner: PostCaptureOwner::MainPrompt,
        },
    );

    assert_eq!(harness.pty_bytes(), b"early\nlate\n");
    let events = harness.events();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CaptureInputRejected { .. })),
        "{events:?}"
    );
}

/// T6: an invalidated (abandoned) chain never replays; the buffered bytes
/// are rejected visibly while live input still flows.
#[test]
fn invalidated_drain_rejects_buffered_bytes() {
    let mut harness = MatrixHarness::new(
        "t6-invalidated",
        RawInputMode::Draining {
            previous_capture: config_capture(),
            generation: 17,
            next_capture: None,
            invalidated: true,
            post_owner: PostCaptureOwner::MainPrompt,
        },
    );
    assert!(!harness.quarantine.observe(b"never delivered"));

    harness.drain_abandoned();

    let events = harness.events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::CaptureInputRejected { generation: 17, byte_len } if *byte_len == 15
        )),
        "{events:?}"
    );
    assert!(harness.pty_bytes().is_empty());
    assert!(matches!(
        current_raw_input_mode(&harness.input_mode),
        RawInputMode::Terminal { generation: 17, .. }
    ));
}

/// T7 refinement: a stale-generation batch must not wipe the live chain's
/// buffered type-ahead; the live chain keeps its own terminal verdict.
#[test]
fn stale_generation_batch_keeps_live_chain_buffer() {
    let mut harness = MatrixHarness::new(
        "t7-live-buffer",
        RawInputMode::Capture {
            capture: config_capture(),
            generation: 21,
            installed_at: Instant::now(),
        },
    );
    assert!(!harness.quarantine.observe(b"live chain bytes"));

    let mut relay = InputRelayContext {
        master: &mut harness.master,
        input_classifier: &harness.classifier,
        input_events: &harness.event_tx,
        input_mode: &harness.input_mode,
        input_generation: &harness.input_generation,
        line_submits: &mut harness.line_submits,
        line_buffer: &mut harness.line_buffer,
        native_line_state: &mut harness.native_line_state,
        exit_tracker: &mut harness.exit_tracker,
        main_prompt_gate: &harness.main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };
    relay_late_capture_bytes(
        b"stale batch",
        20,
        &mut harness.card_state,
        &mut harness.quarantine,
        &mut relay,
    )
    .expect("relay stale batch");

    let events = harness.events();
    // Only the stale batch is rejected; the live buffer stays pending.
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::CaptureInputRejected { generation: 20, byte_len } if *byte_len == 11
        )),
        "{events:?}"
    );
    assert_eq!(harness.quarantine.take_bytes(), b"live chain bytes");
    assert!(harness.pty_bytes().is_empty());
}

/// T7 refinement: leftovers with no live chain surface as a rejection,
/// never a silent wipe; leftovers and the late batch merge into ONE
/// rejection so a single chain never stacks duplicate notices.
#[test]
fn stale_generation_batch_rejects_orphan_buffer_visibly() {
    let mut harness = MatrixHarness::new("t7-orphan-buffer", RawInputMode::Passthrough);
    assert!(!harness.quarantine.observe(b"orphaned"));

    let mut relay = InputRelayContext {
        master: &mut harness.master,
        input_classifier: &harness.classifier,
        input_events: &harness.event_tx,
        input_mode: &harness.input_mode,
        input_generation: &harness.input_generation,
        line_submits: &mut harness.line_submits,
        line_buffer: &mut harness.line_buffer,
        native_line_state: &mut harness.native_line_state,
        exit_tracker: &mut harness.exit_tracker,
        main_prompt_gate: &harness.main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };
    relay_late_capture_bytes(
        b"stale batch",
        20,
        &mut harness.card_state,
        &mut harness.quarantine,
        &mut relay,
    )
    .expect("relay stale batch");

    let events = harness.events();
    let rejections: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            RawInputEvent::CaptureInputRejected {
                generation: 20,
                byte_len,
            } => Some(*byte_len),
            _ => None,
        })
        .collect();
    // "orphaned" (8) + "stale batch" (11) merge into one rejection.
    assert_eq!(rejections, vec![19], "{events:?}");
    assert!(harness.quarantine.take_bytes().is_empty());
    assert!(harness.pty_bytes().is_empty());
}

#[cfg(test)]
#[path = "capture_owner_matrix_tests.rs"]
mod owner;

#[test]
fn terminal_cancel_rejects_unconsumed_approval_submit_remainder() {
    let capture = RawInputCapture::Approval {
        id: "pending-eof".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    let mut harness = MatrixHarness::new(
        "terminal-eof",
        RawInputMode::Submitted {
            capture,
            generation: 51,
        },
    );
    let stale = b"printf unapproved >> effect\n";
    assert!(!harness.quarantine.observe(stale));
    update_input_mode(
        &harness.input_mode,
        &RawObserverAction::InvalidateApprovalCapture("pending-eof".to_string()),
        None,
    );
    harness.drain_abandoned();
    assert!(harness.pty_bytes().is_empty());
    assert!(harness.events().iter().any(|event| matches!(event,
        RawInputEvent::CaptureInputRejected { generation: 51, byte_len } if *byte_len == stale.len())));
    update_input_mode(&harness.input_mode, &RawObserverAction::Continue, Some(51));
    harness.relay_chunk(b"fresh\n", RawInputMode::Passthrough);
    assert_eq!(harness.pty_bytes(), b"fresh\n");
}

#[test]
fn r9_cancel_next_approval_never_installs_dead_card_and_keeps_previous_owner() {
    let pending = RawInputCapture::Approval {
        id: "pending-B".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    let mut harness = MatrixHarness::new(
        "r9-next-owner",
        RawInputMode::Draining {
            previous_capture: config_capture(),
            generation: 61,
            next_capture: Some(pending),
            invalidated: false,
            post_owner: PostCaptureOwner::MainPrompt,
        },
    );
    assert!(!harness.quarantine.observe(b"approved-A\n"));
    update_input_mode(
        &harness.input_mode,
        &RawObserverAction::InvalidateApprovalCapture("pending-B".to_string()),
        None,
    );
    assert!(matches!(
        current_raw_input_mode(&harness.input_mode),
        RawInputMode::Draining {
            generation: 61,
            next_capture: None,
            invalidated: false,
            ..
        }
    ));
    harness.drain_abandoned();
    assert!(matches!(
        current_raw_input_mode(&harness.input_mode),
        RawInputMode::Terminal { generation: 61, .. }
    ));
    assert_eq!(harness.pty_bytes(), b"approved-A\n");
}

#[test]
fn r9_revocation_between_snapshot_and_completion_rejects_old_tail() {
    assert_r9_revocation_boundary(false);
}

#[test]
fn r9_revocation_after_completion_before_replay_rejects_old_tail() {
    assert_r9_revocation_boundary(true);
}

fn assert_r9_revocation_boundary(after_completion: bool) {
    use std::sync::Barrier;
    let capture = RawInputCapture::Approval {
        id: "pending-A".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    let mut harness = MatrixHarness::new(
        if after_completion {
            "r9-after-completion"
        } else {
            "r9-before-completion"
        },
        RawInputMode::Draining {
            previous_capture: capture,
            generation: 62,
            next_capture: None,
            invalidated: false,
            post_owner: PostCaptureOwner::MainPrompt,
        },
    );
    let stale = b"printf unapproved >> effect\n";
    assert!(!harness.quarantine.observe(stale));
    let snapshot = current_raw_input_mode(&harness.input_mode);
    assert!(matches!(
        snapshot,
        RawInputMode::Draining {
            invalidated: false,
            ..
        }
    ));
    let installed_before = after_completion
        .then(|| complete_capture_replay(&harness.input_mode, 62))
        .flatten();
    let begin = Arc::new(Barrier::new(2));
    let done = Arc::new(Barrier::new(2));
    let mode = harness.input_mode.clone();
    let worker_begin = begin.clone();
    let worker_done = done.clone();
    let owner = thread::spawn(move || {
        worker_begin.wait();
        update_input_mode(
            &mode,
            &RawObserverAction::InvalidateApprovalCapture("pending-A".to_string()),
            None,
        );
        worker_done.wait();
    });
    begin.wait();
    done.wait();
    owner.join().unwrap();
    let installed = if after_completion {
        installed_before
    } else {
        complete_capture_replay(&harness.input_mode, 62)
    };
    let mut relay = InputRelayContext {
        master: &mut harness.master,
        input_classifier: &harness.classifier,
        input_events: &harness.event_tx,
        input_mode: &harness.input_mode,
        input_generation: &harness.input_generation,
        line_submits: &mut harness.line_submits,
        line_buffer: &mut harness.line_buffer,
        native_line_state: &mut harness.native_line_state,
        exit_tracker: &mut harness.exit_tracker,
        main_prompt_gate: &harness.main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };
    replay_or_reject_after_drain(
        installed,
        &mut harness.card_state,
        &mut harness.quarantine,
        62,
        &mut relay,
    )
    .unwrap();
    assert!(harness.pty_bytes().is_empty(), "revoked tail reached Bash");
    assert!(harness.events().iter().any(|event| matches!(event,
        RawInputEvent::CaptureInputRejected { generation: 62, byte_len } if *byte_len == stale.len())));
}

#[test]
fn r9_cancel_previous_approval_preserves_live_next_card_and_rejects_old_tail() {
    let capture = RawInputCapture::Approval {
        id: "pending-A".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    let next = question_capture("live-B");
    let mut harness = MatrixHarness::new(
        "r9-live-next",
        RawInputMode::Draining {
            previous_capture: capture,
            generation: 63,
            next_capture: Some(next.clone()),
            invalidated: false,
            post_owner: PostCaptureOwner::MainPrompt,
        },
    );
    assert!(!harness.quarantine.observe(b"never replay\n"));
    update_input_mode(
        &harness.input_mode,
        &RawObserverAction::InvalidateApprovalCapture("pending-A".to_string()),
        None,
    );
    harness.drain_abandoned();
    assert!(matches!(current_raw_input_mode(&harness.input_mode),
        RawInputMode::Capture { capture, .. } if capture == next));
    assert!(harness.pty_bytes().is_empty());
    assert!(harness.events().iter().any(|event| matches!(
        event,
        RawInputEvent::CaptureInputRejected { generation: 63, .. }
    )));
}

#[test]
fn r10_owner_check_then_terminal_revocation_never_replays_tail() {
    assert_r10_commit_revocation(PostCaptureOwner::MainPrompt, "r10-main");
}

#[test]
fn r10_owner_check_then_raw_owner_revocation_never_replays_tail() {
    assert_r10_commit_revocation(PostCaptureOwner::RawPassthrough, "r10-raw");
}

#[test]
fn r10_owner_check_then_delay_owner_revocation_never_replays_tail() {
    assert_r10_commit_revocation(PostCaptureOwner::Delay, "r10-delay");
}

fn assert_r10_commit_revocation(post_owner: PostCaptureOwner, tag: &str) {
    use std::sync::Barrier;
    let capture = RawInputCapture::Approval {
        id: "pending-commit".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    let mut harness = MatrixHarness::new(
        tag,
        RawInputMode::Draining {
            previous_capture: capture,
            generation: 71,
            next_capture: None,
            invalidated: false,
            post_owner,
        },
    );
    let tail = b"printf stale >> effect\n";
    assert!(!harness.quarantine.observe(tail));
    let begin = Arc::new(Barrier::new(2));
    let done = Arc::new(Barrier::new(2));
    let mode = harness.input_mode.clone();
    let worker_begin = begin.clone();
    let worker_done = done.clone();
    let owner = thread::spawn(move || {
        worker_begin.wait();
        update_input_mode(
            &mode,
            &RawObserverAction::InvalidateApprovalCapture("pending-commit".to_string()),
            None,
        );
        worker_done.wait();
    });
    super::BEFORE_REPLAY_COMMIT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            begin.wait();
            done.wait();
        }));
    });
    harness.drain_abandoned();
    owner.join().unwrap();
    assert!(
        harness.pty_bytes().is_empty(),
        "stale checked owner reached PTY"
    );
    assert!(harness.events().iter().any(|event| matches!(event,
        RawInputEvent::CaptureInputRejected { generation: 71, byte_len } if *byte_len == tail.len())));
}

#[test]
fn replay_backpressure_retains_suffix_and_releases_owner_lock() {
    use std::io::Write;
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    let (stream, _peer) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    let mut master = unsafe { std::fs::File::from_raw_fd(stream.into_raw_fd()) };
    let payload = [b'x'; 8192];
    loop {
        match master.write(&payload) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("fill nonblocking writer: {error}"),
        }
    }
    let mut harness = MatrixHarness::new(
        "r10-backpressure",
        RawInputMode::Draining {
            previous_capture: config_capture(),
            generation: 72,
            next_capture: None,
            invalidated: false,
            post_owner: PostCaptureOwner::RawPassthrough,
        },
    );
    harness.master = master;
    assert!(!harness.quarantine.observe(b"fresh\n"));
    harness
        .try_drain_abandoned()
        .expect("full PTY defers replay");
    assert!(harness.quarantine.has_pending_replay());
    assert!(harness.input_mode.try_lock().is_ok());
    assert!(!super::NonblockingReplayWrite::is_active());
    assert!(matches!(
        current_raw_input_mode(&harness.input_mode),
        RawInputMode::RawPassthrough
    ));
    assert!(!harness
        .events()
        .iter()
        .any(|event| matches!(event, RawInputEvent::CaptureDrained { generation: 72 })));
}

#[test]
fn r10_owner_lock_covers_pty_write_and_releases_before_drained_event() {
    use std::cell::Cell;
    struct CheckedSink<'a> {
        mode: &'a Arc<Mutex<RawInputMode>>,
        sender: &'a mpsc::Sender<RawInputEvent>,
        writes: Cell<usize>,
    }
    impl RawInputEventSink for CheckedSink<'_> {
        fn send(&self, event: RawInputEvent) -> Result<(), mpsc::SendError<RawInputEvent>> {
            if matches!(event, RawInputEvent::PtyUserWrite { .. }) {
                assert!(self.mode.try_lock().is_err(), "write escaped owner lock");
                self.writes.set(self.writes.get() + 1);
            }
            if matches!(event, RawInputEvent::CaptureDrained { .. }) {
                assert!(
                    self.mode.try_lock().is_ok(),
                    "drained published before commit"
                );
            }
            self.sender.send(event)
        }
    }
    let mut harness = MatrixHarness::new(
        "r10-locked-write",
        RawInputMode::Draining {
            previous_capture: config_capture(),
            generation: 73,
            next_capture: None,
            invalidated: false,
            post_owner: PostCaptureOwner::RawPassthrough,
        },
    );
    assert!(!harness.quarantine.observe(b"fresh\n"));
    let sink = CheckedSink {
        mode: &harness.input_mode,
        sender: &harness.event_tx,
        writes: Cell::new(0),
    };
    let mut relay = InputRelayContext {
        master: &mut harness.master,
        input_classifier: &harness.classifier,
        input_events: &sink,
        input_mode: &harness.input_mode,
        input_generation: &harness.input_generation,
        line_submits: &mut harness.line_submits,
        line_buffer: &mut harness.line_buffer,
        native_line_state: &mut harness.native_line_state,
        exit_tracker: &mut harness.exit_tracker,
        main_prompt_gate: &harness.main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };
    drain_abandoned_capture(&mut harness.card_state, &mut harness.quarantine, &mut relay).unwrap();
    assert_eq!(sink.writes.get(), 1);
    assert_eq!(harness.pty_bytes(), b"fresh\n");
}

#[test]
fn capture_replay_backpressure_preserves_tail_for_retry_without_driver_failure() {
    use std::io::{Read, Write};
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    let (stream, mut peer) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut master = unsafe { std::fs::File::from_raw_fd(stream.into_raw_fd()) };
    let payload = [b'x'; 8192];
    let mut filled = 0;
    loop {
        match master.write(&payload) {
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("fill nonblocking writer: {error}"),
        }
    }
    let mut harness = MatrixHarness::new(
        "backpressure-retry",
        RawInputMode::Draining {
            previous_capture: config_capture(),
            generation: 74,
            next_capture: None,
            invalidated: false,
            post_owner: PostCaptureOwner::RawPassthrough,
        },
    );
    harness.master = master;
    let tail = b"printf fresh-once\n";
    assert!(!harness.quarantine.observe(tail));
    let first = harness.try_drain_abandoned();
    assert!(
        first.is_ok(),
        "temporary backpressure must not fail the input driver: {first:?}"
    );
    assert!(harness.input_mode.try_lock().is_ok());
    let mut previous = vec![0; filled];
    peer.read_exact(&mut previous).unwrap();
    assert!(previous.iter().all(|byte| *byte == b'x'));
    harness.try_drain_abandoned().expect("retry after writable");
    let mut delivered = vec![0; tail.len()];
    peer.read_exact(&mut delivered)
        .expect("retained replay tail");
    assert_eq!(delivered, tail);
    assert_eq!(
        harness
            .events()
            .iter()
            .filter(|event| matches!(event, RawInputEvent::CaptureDrained { generation: 74 }))
            .count(),
        1
    );
}

#[test]
fn capture_replay_partial_write_retries_only_the_unwritten_suffix() {
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    let (stream, mut peer) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    peer.set_nonblocking(true).unwrap();
    let capacity: nix::libc::c_int = 4096;
    assert_eq!(
        unsafe {
            nix::libc::setsockopt(
                stream.as_raw_fd(),
                nix::libc::SOL_SOCKET,
                nix::libc::SO_SNDBUF,
                (&capacity as *const nix::libc::c_int).cast(),
                std::mem::size_of_val(&capacity) as _,
            )
        },
        0
    );
    let master = unsafe { std::fs::File::from_raw_fd(stream.into_raw_fd()) };
    let mut harness = MatrixHarness::new(
        "partial-write",
        RawInputMode::Draining {
            previous_capture: config_capture(),
            generation: 75,
            next_capture: None,
            invalidated: false,
            post_owner: PostCaptureOwner::RawPassthrough,
        },
    );
    harness.master = master;
    let tail = (0..60 * 1024)
        .map(|index| b'a' + (index % 26) as u8)
        .collect::<Vec<_>>();
    assert!(!harness.quarantine.observe(&tail));
    harness.try_drain_abandoned().unwrap();
    let pending = harness
        .quarantine
        .pending_replay
        .as_ref()
        .expect("partial write remains pending");
    assert!(
        pending.bytes.len() < tail.len(),
        "the first prefix must already have been written"
    );
    let mut delivered = Vec::new();
    let mut buffer = [0; 8192];
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        loop {
            match peer.read(&mut buffer) {
                Ok(0) => panic!("replay peer closed"),
                Ok(count) => delivered.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("read replay: {error}"),
            }
        }
        if !harness.quarantine.has_pending_replay() {
            break;
        }
        assert!(Instant::now() < deadline, "partial replay did not advance");
        harness.try_drain_abandoned().unwrap();
    }
    assert_eq!(delivered, tail);
    assert_eq!(
        harness
            .events()
            .iter()
            .filter(|event| matches!(event, RawInputEvent::CaptureDrained { generation: 75 }))
            .count(),
        1
    );
}

#[test]
fn pending_capture_replay_is_rejected_when_a_new_capture_owns_input() {
    use std::io::{Read, Write};
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    let (stream, mut peer) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    peer.set_nonblocking(true).unwrap();
    let mut master = unsafe { std::fs::File::from_raw_fd(stream.into_raw_fd()) };
    let payload = [b'x'; 8192];
    let mut filled = 0;
    loop {
        match master.write(&payload) {
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("fill writer: {error}"),
        }
    }
    let mut harness = MatrixHarness::new(
        "pending-owner-change",
        RawInputMode::Draining {
            previous_capture: config_capture(),
            generation: 76,
            next_capture: None,
            invalidated: false,
            post_owner: PostCaptureOwner::RawPassthrough,
        },
    );
    harness.master = master;
    let tail = b"printf stale\n";
    assert!(!harness.quarantine.observe(tail));
    harness.try_drain_abandoned().unwrap();
    assert!(harness.quarantine.has_pending_replay());
    let replacement = question_capture("fresh-question");
    update_input_mode(
        &harness.input_mode,
        &RawObserverAction::CaptureInput(replacement.clone()),
        None,
    );
    let fresh_owner = current_raw_input_mode(&harness.input_mode);
    harness.try_drain_abandoned().unwrap();
    assert!(!harness.quarantine.has_pending_replay());
    assert_eq!(current_raw_input_mode(&harness.input_mode), fresh_owner);
    let events = harness.events();
    assert!(events.iter().any(|event| matches!(event,
        RawInputEvent::CaptureInputRejected { generation: 76, byte_len } if *byte_len == tail.len())));
    let mut previous = vec![0; filled];
    peer.read_exact(&mut previous).unwrap();
    assert!(previous.iter().all(|byte| *byte == b'x'));
    assert_eq!(
        peer.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let ack = spawn_submission_ack(harness.input_mode.clone(), RawObserverAction::Continue);
    harness.relay_chunk(b"fresh answer\r", fresh_owner);
    ack.join().unwrap();
    assert!(harness.events().iter().any(|event| matches!(event,
        RawInputEvent::CardInput(id, answer) if id == "fresh-question" && answer == "fresh answer")));
}

#[test]
fn pending_capture_replay_yields_to_an_already_deferred_eof() {
    use std::io::Write;
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    let (stream, _peer) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    let mut master = unsafe { std::fs::File::from_raw_fd(stream.into_raw_fd()) };
    loop {
        match master.write(&[b'x'; 8192]) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("fill writer: {error}"),
        }
    }
    let input_mode = Arc::new(Mutex::new(RawInputMode::RawPassthrough));
    let worker_mode = input_mode.clone();
    let (done, completed) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut state = RawInputRelayState::default();
        state.capture_owned_input.pending_replay = Some(super::PendingCaptureReplay {
            bytes: b"fresh\n".to_vec(),
            offset: 0,
            generation: 77,
            owner: RawInputMode::RawPassthrough,
            native_line_state: NativeLineState::default(),
            exit_tracker_after_replay: ExplicitExitTracker::default(),
        });
        state.deferred_input = Some(InputRead::Eof);
        let (events, _receiver) = mpsc::channel();
        super::wait_for_pending_capture_replay(
            &mut master,
            &InputClassifier::default(),
            &events,
            &worker_mode,
            &mut state,
            None,
            None,
        )
        .unwrap();
        done.send((
            matches!(state.deferred_input, Some(InputRead::Eof)),
            state.capture_owned_input.has_pending_replay(),
        ))
        .unwrap();
    });
    let outcome = completed.recv_timeout(Duration::from_secs(1));
    update_input_mode(&input_mode, &RawObserverAction::HoldShellOutput, None);
    worker.join().unwrap();
    assert_eq!(
        outcome.expect("EOF must wake pending replay without a writable peer"),
        (true, false)
    );
}

#[test]
fn input_driver_observes_ctrl_c_while_capture_replay_is_backpressured() {
    assert_input_driver_control_while_backpressured("ctrl-c", false);
}

#[test]
fn input_driver_observes_ctrl_c_after_queued_ordinary_reads() {
    assert_input_driver_control_while_backpressured("ctrl-c", true);
}

#[test]
fn input_driver_observes_ctrl_c_after_terminal_becomes_passthrough() {
    assert_input_driver_control_while_backpressured("ctrl-c-terminal", true);
}

#[test]
fn input_driver_observes_eof_after_terminal_becomes_passthrough() {
    assert_input_driver_control_while_backpressured("eof-terminal", true);
}

#[test]
fn input_driver_observes_eof_after_queued_ordinary_reads() {
    assert_input_driver_control_while_backpressured("eof", true);
}

#[test]
fn input_driver_preserves_read_ahead_at_the_byte_limit() {
    assert_input_driver_control_while_backpressured("drain-limit", true);
}

#[test]
fn input_driver_rejects_read_ahead_overflow_without_stopping() {
    assert_input_driver_control_while_backpressured("overflow", true);
}

#[test]
fn pending_unwritten_exit_does_not_swallow_eof_shutdown() {
    assert_input_driver_control_while_backpressured("eof-exit", false);
}

#[test]
fn input_driver_keeps_later_ordinary_reads_behind_pending_suffix() {
    assert_input_driver_control_while_backpressured("drain", true);
}

fn assert_input_driver_control_while_backpressured(control: &str, queue_ordinary: bool) {
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    struct ObservedInput {
        stream: UnixStream,
        read: mpsc::Sender<()>,
    }
    impl Read for ObservedInput {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let result = self.stream.read(bytes);
            if matches!(result, Ok(count) if count > 0) {
                self.read.send(()).unwrap();
            }
            result
        }
    }
    let (stream, mut peer) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    peer.set_nonblocking(true).unwrap();
    let mut master = unsafe { std::fs::File::from_raw_fd(stream.into_raw_fd()) };
    let mut filled = 0;
    loop {
        match master.write(&[b'x'; 8192]) {
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("fill writer: {error}"),
        }
    }
    let (input, writer) = UnixStream::pair().unwrap();
    input.set_nonblocking(true).unwrap();
    let input_fd = input.as_raw_fd();
    let (read, reads) = mpsc::channel();
    let input = ObservedInput {
        stream: input,
        read,
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
        capture: config_capture(),
        generation: 78,
        installed_at: Instant::now(),
    }));
    let (events, receiver) = mpsc::channel();
    let driver = spawn_raw_input_relay_with_wake(
        input,
        master,
        events,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        RawInputShellRoute::new(MainPromptGate::default(), false, None),
        Some(input_fd),
        None,
    );
    let mut writer = Some(writer);
    let initial = if control == "eof-exit" {
        b"\rexit\n".as_slice()
    } else {
        b"\rfresh\n".as_slice()
    };
    writer.as_mut().unwrap().write_all(initial).unwrap();
    loop {
        match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
            RawInputEvent::CaptureSubmitted { generation, .. } => {
                update_input_mode(
                    &input_mode,
                    if control.ends_with("terminal") {
                        &RawObserverAction::Continue
                    } else {
                        &RawObserverAction::RawPassthrough
                    },
                    Some(generation),
                );
            }
            RawInputEvent::PtyUserWrite { .. } => break,
            _ => {}
        }
    }
    reads.recv_timeout(Duration::from_secs(1)).unwrap();
    if control.ends_with("terminal") {
        assert!(matches!(
            current_raw_input_mode(&input_mode),
            RawInputMode::Terminal { .. }
        ));
        update_input_mode(&input_mode, &RawObserverAction::Continue, None);
        assert_eq!(
            current_raw_input_mode(&input_mode),
            RawInputMode::Passthrough
        );
    }
    let mut ordinary = Vec::new();
    if queue_ordinary {
        let count = if control == "overflow" {
            9
        } else if control == "drain-limit" {
            8
        } else {
            INPUT_READ_AHEAD_CAPACITY + 2
        };
        for index in 0..count {
            let bytes = if control == "overflow" || control == "drain-limit" {
                "x".repeat(8192)
            } else {
                format!("ordinary-{index}")
            };
            writer
                .as_mut()
                .unwrap()
                .write_all(bytes.as_bytes())
                .unwrap();
            if reads.recv_timeout(Duration::from_secs(1)).is_err() {
                break;
            }
            ordinary.extend_from_slice(bytes.as_bytes());
        }
    }
    if control.starts_with("drain") {
        let mut delivered = Vec::new();
        let expected = [vec![b'x'; filled], b"fresh\n".to_vec(), ordinary].concat();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut buffer = [0; 8192];
        while delivered.len() < expected.len() && Instant::now() < deadline {
            match peer.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => delivered.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => thread::yield_now(),
                Err(error) => panic!("read queued bytes: {error}"),
            }
        }
        assert_eq!(delivered, expected);
        drop(writer.take());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !driver.is_finished() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(driver.is_finished(), "driver cleanup timed out");
        driver.join().unwrap().unwrap();
        return;
    }
    let mut overflow_rejected = false;
    if control == "overflow" {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut overflow = false;
        let mut rejected = 0;
        while Instant::now() < deadline && (!overflow || rejected < 9 * 8192) {
            match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(RawInputEvent::CaptureOverflow { .. }) => overflow = true,
                Ok(RawInputEvent::CaptureInputRejected { byte_len, .. }) => rejected += byte_len,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        overflow_rejected = overflow && rejected == 9 * 8192;
    }
    if control.starts_with("eof") {
        drop(writer.take());
    } else {
        writer.as_mut().unwrap().write_all(b"\x03").unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut observed = false;
    while Instant::now() < deadline {
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(RawInputEvent::CtrlC) if control.starts_with("ctrl-c") || control == "overflow" => {
                observed = control != "overflow" || overflow_rejected;
                break;
            }
            Ok(RawInputEvent::EofShutdownRequested) if control.starts_with("eof") => {
                observed = true;
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    update_input_mode(&input_mode, &RawObserverAction::HoldShellOutput, None);
    let mut buffer = [0; 8192];
    loop {
        match peer.read(&mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    drop(writer.take());
    let deadline = Instant::now() + Duration::from_secs(2);
    while !driver.is_finished() && Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(driver.is_finished(), "driver cleanup timed out");
    driver.join().unwrap().unwrap();
    assert!(
        observed,
        "{control} must reach the input driver while the peer remains backpressured"
    );
}

#[test]
fn pending_replay_does_not_claim_ctrl_c_owned_by_a_new_prompt_ghost() {
    let mut harness = MatrixHarness::new("pending-ghost-owner", RawInputMode::RawPassthrough);
    let mut state = RawInputRelayState::default();
    state.capture_owned_input.pending_replay = Some(super::PendingCaptureReplay {
        bytes: b"stale\n".to_vec(),
        offset: 0,
        generation: 79,
        owner: RawInputMode::RawPassthrough,
        native_line_state: NativeLineState::default(),
        exit_tracker_after_replay: ExplicitExitTracker::default(),
    });
    let ghost = RawInputMode::PromptGhost {
        text: "suggested".to_string(),
        route: PromptGhostRoute::NativeShell,
    };
    state.deferred_input = Some(InputRead::Bytes {
        bytes: vec![3],
        received_at: Instant::now(),
        observed_mode: ghost.clone(),
        ownership_changed_during_read: false,
        pending_shell_submits: 0,
    });
    assert!(
        !super::consume_pending_replay_control(
            &harness.master,
            &harness.event_tx,
            &harness.input_mode,
            &mut state,
            None
        )
        .unwrap(),
        "the old pending owner must not claim the new ghost's Ctrl-C"
    );
    *harness.input_mode.lock().unwrap() = ghost.clone();
    harness.quarantine = std::mem::take(&mut state.capture_owned_input);
    harness.drain_abandoned();
    assert_eq!(current_raw_input_mode(&harness.input_mode), ghost);
    harness.relay_chunk(&[3], ghost);
    assert!(harness
        .events()
        .contains(&RawInputEvent::PromptGhostDismissed));
    assert_eq!(
        current_raw_input_mode(&harness.input_mode),
        RawInputMode::Passthrough
    );
    assert_eq!(harness.pty_bytes(), b"\x03");
}

#[test]
fn cancelling_a_pending_suffix_preserves_a_physically_submitted_exit_42() {
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    let (stream, mut peer) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    peer.set_nonblocking(true).unwrap();
    let capacity: nix::libc::c_int = 4096;
    assert_eq!(
        unsafe {
            nix::libc::setsockopt(
                stream.as_raw_fd(),
                nix::libc::SOL_SOCKET,
                nix::libc::SO_SNDBUF,
                (&capacity as *const nix::libc::c_int).cast(),
                std::mem::size_of_val(&capacity) as _,
            )
        },
        0
    );
    let master = unsafe { std::fs::File::from_raw_fd(stream.into_raw_fd()) };
    let mut harness = MatrixHarness::new(
        "committed-exit-42",
        RawInputMode::Draining {
            previous_capture: config_capture(),
            generation: 80,
            next_capture: None,
            invalidated: false,
            post_owner: PostCaptureOwner::RawPassthrough,
        },
    );
    harness.master = master;
    let tail = [b"exit 42\n".as_slice(), &vec![b'x'; 60 * 1024]].concat();
    assert!(!harness.quarantine.observe(&tail));
    harness.try_drain_abandoned().unwrap();
    let mut prefix = [0; 8192];
    let count = peer.read(&mut prefix).unwrap();
    assert!(prefix[..count].starts_with(b"exit 42\n"));
    let mut state = RawInputRelayState {
        capture_owned_input: std::mem::take(&mut harness.quarantine),
        exit_tracker: std::mem::take(&mut harness.exit_tracker),
        native_line_state: std::mem::take(&mut harness.native_line_state),
        deferred_input: Some(InputRead::Eof),
        ..Default::default()
    };
    assert!(super::consume_pending_replay_control(
        &harness.master,
        &harness.event_tx,
        &harness.input_mode,
        &mut state,
        None
    )
    .unwrap());
    assert!(state.exit_tracker.saw_explicit_exit());
    finish_input_relay(
        &mut harness.master,
        &harness.event_tx,
        &harness.classifier,
        &harness.input_mode,
        &mut state,
    )
    .unwrap();
    assert!(!harness
        .events()
        .contains(&RawInputEvent::EofShutdownRequested));
}

#[test]
fn pending_replay_cancellation_preserves_input_observed_by_the_new_owner() {
    let harness = MatrixHarness::new("queued-new-owner", RawInputMode::RawPassthrough);
    let mut state = RawInputRelayState::default();
    state.capture_owned_input.pending_replay = Some(super::PendingCaptureReplay {
        bytes: b"stale\n".to_vec(),
        offset: 0,
        generation: 81,
        owner: RawInputMode::RawPassthrough,
        native_line_state: NativeLineState::default(),
        exit_tracker_after_replay: ExplicitExitTracker::default(),
    });
    let fresh = RawInputMode::PromptGhost {
        text: "fresh".to_string(),
        route: PromptGhostRoute::NativeShell,
    };
    for (bytes, observed_mode) in [
        (b"old".to_vec(), RawInputMode::RawPassthrough),
        (b"new".to_vec(), fresh.clone()),
    ] {
        state.pending_replay_input_bytes += bytes.len();
        state.pending_replay_input.push_back(InputRead::Bytes {
            bytes,
            observed_mode,
            received_at: Instant::now(),
            ownership_changed_during_read: false,
            pending_shell_submits: 0,
        });
    }
    state.pending_replay_input_bytes += 1;
    state.pending_replay_input.push_back(InputRead::Bytes {
        bytes: vec![3],
        observed_mode: RawInputMode::RawPassthrough,
        received_at: Instant::now(),
        ownership_changed_during_read: false,
        pending_shell_submits: 0,
    });
    assert!(super::consume_pending_replay_control(
        &harness.master,
        &harness.event_tx,
        &harness.input_mode,
        &mut state,
        None
    )
    .unwrap());
    assert_eq!(state.pending_replay_input_bytes, 4);
    assert!(
        matches!(state.pending_replay_input.front(), Some(InputRead::Bytes { bytes, observed_mode, .. }) if bytes == b"new" && observed_mode == &fresh)
    );
}

#[test]
fn control_promotion_preserves_new_owner_before_ctrl_c_suffix() {
    assert_control_promotion_preserves_new_owner(false, "reader");
}

#[test]
fn control_promotion_preserves_new_owner_before_eof() {
    assert_control_promotion_preserves_new_owner(true, "reader");
}

#[test]
fn queued_new_owner_precedes_ctrl_c_suffix_and_rejects_old_deferred_input() {
    assert_control_promotion_preserves_new_owner(false, "queued");
}

#[test]
fn queued_new_owner_precedes_eof_and_rejects_old_deferred_input() {
    assert_control_promotion_preserves_new_owner(true, "queued");
}

#[test]
fn deferred_new_owner_survives_old_replay_cancellation() {
    assert_control_promotion_preserves_new_owner(false, "deferred");
}

#[test]
fn pending_deferred_terminal_preserves_new_owner_prefix() {
    assert_control_promotion_preserves_new_owner(true, "deferred-eof");
}

fn assert_control_promotion_preserves_new_owner(eof: bool, source: &str) {
    use std::sync::atomic::Ordering;
    let harness = MatrixHarness::new("promotion-new-owner", RawInputMode::RawPassthrough);
    let mut state = RawInputRelayState::default();
    state.capture_owned_input.pending_replay = Some(super::PendingCaptureReplay {
        bytes: b"stale\n".to_vec(),
        offset: 0,
        generation: 82,
        owner: RawInputMode::RawPassthrough,
        native_line_state: NativeLineState::default(),
        exit_tracker_after_replay: ExplicitExitTracker::default(),
    });
    let read = |bytes: &[u8], observed_mode| InputRead::Bytes {
        bytes: bytes.to_vec(),
        observed_mode,
        received_at: Instant::now(),
        ownership_changed_during_read: false,
        pending_shell_submits: 0,
    };
    state.deferred_input = Some(read(b"old", RawInputMode::RawPassthrough));
    let fresh = RawInputMode::PromptGhost {
        text: "fresh".to_string(),
        route: PromptGhostRoute::NativeShell,
    };
    let (sender, receiver) = mpsc::sync_channel(3);
    match source {
        "deferred-eof" => {
            state
                .pending_replay_input
                .push_back(state.deferred_input.take().unwrap());
            state
                .pending_replay_input
                .push_back(read(b"new", fresh.clone()));
            state.pending_replay_input_bytes = 6;
            state.deferred_input = Some(InputRead::Eof);
        }
        "queued" => {
            state.pending_replay_input_bytes += 3;
            state
                .pending_replay_input
                .push_back(read(b"new", fresh.clone()));
        }
        "deferred" => {
            state.pending_replay_input_bytes += 3;
            state
                .pending_replay_input
                .push_back(state.deferred_input.take().unwrap());
            state.deferred_input = Some(read(b"new", fresh.clone()));
        }
        _ => sender.send(read(b"new", fresh.clone())).unwrap(),
    }
    sender
        .send(if eof {
            InputRead::Eof
        } else {
            read(b"\x03suffix", RawInputMode::RawPassthrough)
        })
        .unwrap();
    state.capture_owned_input.replay_control.store(
        if eof {
            super::REPLAY_EOF
        } else {
            super::REPLAY_CTRL_C
        },
        Ordering::Release,
    );
    assert!(super::consume_pending_replay_control(
        &harness.master,
        &harness.event_tx,
        &harness.input_mode,
        &mut state,
        Some(&receiver)
    )
    .unwrap());
    assert!(!state.capture_owned_input.has_pending_replay());
    let first = super::super::deadline::receive_input(&receiver, &mut state).unwrap();
    assert!(
        matches!(first, InputRead::Bytes { bytes, observed_mode, .. } if bytes == b"new" && observed_mode == fresh)
    );
    let second = super::super::deadline::receive_input(&receiver, &mut state).unwrap();
    if eof {
        assert!(matches!(second, InputRead::Eof));
    } else {
        assert!(matches!(second, InputRead::Bytes { bytes, .. } if bytes == b"\x03suffix"));
    }
    assert_eq!(state.pending_replay_input_bytes, 0);
}

#[test]
fn pending_control_flag_waits_for_the_real_read_before_writing_replay() {
    use std::io::Read;
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::Ordering;
    let (stream, mut peer) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    peer.set_nonblocking(true).unwrap();
    let mut master = unsafe { std::fs::File::from_raw_fd(stream.into_raw_fd()) };
    let input_mode = Arc::new(Mutex::new(RawInputMode::RawPassthrough));
    let mode = input_mode.clone();
    let (sender, receiver) = mpsc::sync_channel(1);
    let (done, completed) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut state = RawInputRelayState::default();
        state.capture_owned_input.pending_replay = Some(super::PendingCaptureReplay {
            bytes: b"must-not-write\n".to_vec(),
            offset: 0,
            generation: 83,
            owner: RawInputMode::RawPassthrough,
            native_line_state: NativeLineState::default(),
            exit_tracker_after_replay: ExplicitExitTracker::default(),
        });
        state
            .capture_owned_input
            .replay_control
            .store(super::REPLAY_CTRL_C, Ordering::Release);
        let (events, _events_rx) = mpsc::channel();
        let result = super::wait_for_pending_capture_replay(
            &mut master,
            &InputClassifier::default(),
            &events,
            &mode,
            &mut state,
            None,
            Some(&receiver),
        );
        done.send(result).unwrap();
    });
    let before_read = completed.recv_timeout(Duration::from_millis(100));
    sender
        .send(InputRead::Bytes {
            bytes: vec![3],
            observed_mode: RawInputMode::RawPassthrough,
            received_at: Instant::now(),
            ownership_changed_during_read: false,
            pending_shell_submits: 0,
        })
        .unwrap();
    let result = completed.recv_timeout(Duration::from_secs(1));
    update_input_mode(&input_mode, &RawObserverAction::HoldShellOutput, None);
    drop(sender);
    let deadline = Instant::now() + Duration::from_secs(1);
    while !worker.is_finished() && Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(worker.is_finished(), "pending-control cleanup timed out");
    worker.join().unwrap();
    assert!(matches!(before_read, Err(mpsc::RecvTimeoutError::Timeout)));
    result.unwrap().unwrap();
    match peer.read(&mut [0; 32]) {
        Ok(0) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        result => panic!("replay bytes escaped before the real control read: {result:?}"),
    }
}

#[test]
fn deferred_terminal_eof_follows_already_buffered_input() {
    assert_deferred_terminal_follows_buffered_input(false);
}

#[test]
fn deferred_terminal_error_follows_already_buffered_input() {
    assert_deferred_terminal_follows_buffered_input(true);
}

fn assert_deferred_terminal_follows_buffered_input(error: bool) {
    let mut state = RawInputRelayState::default();
    state.pending_replay_input.push_back(InputRead::Bytes {
        bytes: b"new".to_vec(),
        observed_mode: RawInputMode::RawPassthrough,
        received_at: Instant::now(),
        ownership_changed_during_read: false,
        pending_shell_submits: 0,
    });
    state.pending_replay_input_bytes = 3;
    state.deferred_input = Some(if error {
        InputRead::Error(std::io::Error::other("reader failed"))
    } else {
        InputRead::Eof
    });
    let (_sender, receiver) = mpsc::sync_channel(1);
    assert!(
        matches!(super::super::deadline::receive_input(&receiver, &mut state).unwrap(), InputRead::Bytes { bytes, .. } if bytes == b"new")
    );
    let terminal = super::super::deadline::receive_input(&receiver, &mut state).unwrap();
    assert!(matches!(
        (&terminal, error),
        (InputRead::Eof, false) | (InputRead::Error(_), true)
    ));
    assert_eq!(state.pending_replay_input_bytes, 0);
}

#[test]
fn pending_replay_treats_disconnected_reader_as_eof() {
    use std::sync::atomic::Ordering;
    let harness = MatrixHarness::new("disconnected-reader", RawInputMode::RawPassthrough);
    let mut state = RawInputRelayState::default();
    state.capture_owned_input.pending_replay = Some(super::PendingCaptureReplay {
        bytes: b"stale\n".to_vec(),
        offset: 0,
        generation: 84,
        owner: RawInputMode::RawPassthrough,
        native_line_state: NativeLineState::default(),
        exit_tracker_after_replay: ExplicitExitTracker::default(),
    });
    state
        .capture_owned_input
        .replay_control
        .store(super::REPLAY_CTRL_C, Ordering::Release);
    let (sender, receiver) = mpsc::sync_channel(1);
    drop(sender);
    assert!(super::consume_pending_replay_control(
        &harness.master,
        &harness.event_tx,
        &harness.input_mode,
        &mut state,
        Some(&receiver)
    )
    .unwrap());
    assert!(matches!(state.deferred_input, Some(InputRead::Eof)));
    assert!(!state.capture_owned_input.has_pending_replay());
}

#[test]
fn deferred_ordinary_preserves_ctrl_c_at_full_fifo_budget() {
    assert_deferred_ordinary_keeps_queued_control(7 * 8192, "ctrl-c", false, "old");
}

#[test]
fn deferred_ordinary_overflow_preserves_queued_ctrl_c() {
    assert_deferred_ordinary_keeps_queued_control(8 * 8192, "ctrl-c", true, "old");
}

#[test]
fn deferred_ordinary_overflow_preserves_queued_eof() {
    assert_deferred_ordinary_keeps_queued_control(8 * 8192, "eof", true, "old");
}

#[test]
fn deferred_ordinary_overflow_preserves_queued_error() {
    assert_deferred_ordinary_keeps_queued_control(8 * 8192, "error", true, "old");
}

#[test]
fn deferred_ordinary_overflow_keeps_new_owner_prefix_before_ctrl_c() {
    assert_deferred_ordinary_keeps_queued_control(8 * 8192, "ctrl-c", true, "new");
}

#[test]
fn deferred_ordinary_overflow_keeps_changed_owner_prefix_before_ctrl_c() {
    assert_deferred_ordinary_keeps_queued_control(8 * 8192, "ctrl-c", true, "changed");
}

#[test]
fn deferred_ordinary_overflow_keeps_new_owner_prefix_before_eof() {
    assert_deferred_ordinary_keeps_queued_control(8 * 8192, "eof", true, "new");
}

fn assert_deferred_ordinary_keeps_queued_control(
    ordinary_bytes: usize,
    control: &str,
    overflow: bool,
    prefix_owner: &str,
) {
    let harness = MatrixHarness::new("control-budget", RawInputMode::RawPassthrough);
    let mut state = RawInputRelayState::default();
    state.capture_owned_input.pending_replay = Some(super::PendingCaptureReplay {
        bytes: b"stale\n".to_vec(),
        offset: 0,
        generation: 85,
        owner: RawInputMode::RawPassthrough,
        native_line_state: NativeLineState::default(),
        exit_tracker_after_replay: ExplicitExitTracker::default(),
    });
    let read = |bytes: Vec<u8>| InputRead::Bytes {
        bytes,
        observed_mode: RawInputMode::RawPassthrough,
        received_at: Instant::now(),
        ownership_changed_during_read: false,
        pending_shell_submits: 0,
    };
    let mut ordinary = read(vec![b'x'; ordinary_bytes]);
    if let InputRead::Bytes {
        observed_mode,
        ownership_changed_during_read,
        ..
    } = &mut ordinary
    {
        if prefix_owner == "new" {
            *observed_mode = RawInputMode::PromptGhost {
                text: "new".to_string(),
                route: PromptGhostRoute::NativeShell,
            };
        } else if prefix_owner == "changed" {
            *ownership_changed_during_read = true;
        }
    }
    state.pending_replay_input.push_back(ordinary);
    state.pending_replay_input_bytes = ordinary_bytes;
    let mut control_bytes = vec![b's'; 8192];
    control_bytes[0] = 3;
    let queued_control = match control {
        "eof" => InputRead::Eof,
        "error" => InputRead::Error(std::io::Error::other("reader failed")),
        _ => {
            state.pending_replay_input_bytes += control_bytes.len();
            read(control_bytes.clone())
        }
    };
    state.pending_replay_input.push_back(queued_control);
    state.deferred_input = Some(read(vec![b'a']));
    assert!(super::consume_pending_replay_control(
        &harness.master,
        &harness.event_tx,
        &harness.input_mode,
        &mut state,
        None
    )
    .unwrap());
    assert!(!state.capture_owned_input.has_pending_replay());
    let (_sender, receiver) = mpsc::sync_channel(1);
    if prefix_owner != "old" {
        let first = super::super::deadline::receive_input(&receiver, &mut state).unwrap();
        assert!(
            matches!(first, InputRead::Bytes { bytes, observed_mode, ownership_changed_during_read, .. }
            if bytes == vec![b'x'; ordinary_bytes]
                && (matches!(observed_mode, RawInputMode::PromptGhost { .. }) || ownership_changed_during_read))
        );
    }
    let delivered = super::super::deadline::receive_input(&receiver, &mut state).unwrap();
    match control {
        "eof" => assert!(matches!(delivered, InputRead::Eof)),
        "error" => assert!(matches!(delivered, InputRead::Error(_))),
        _ => assert!(matches!(delivered, InputRead::Bytes { bytes, .. } if bytes == control_bytes)),
    }
    assert_eq!(state.pending_replay_input_bytes, 0);
    assert_eq!(
        harness
            .events()
            .iter()
            .any(|event| matches!(event, RawInputEvent::CaptureOverflow { generation: 85 })),
        overflow
    );
}

#[test]
fn native_literal_policy_keeps_delay_and_eof_controls_actionable() {
    let input = |observed_mode| InputRead::Bytes {
        bytes: b"prefix\x03suffix".to_vec(),
        received_at: Instant::now(),
        observed_mode,
        ownership_changed_during_read: false,
        pending_shell_submits: 0,
    };
    assert!(!super::is_replay_control(
        &input(RawInputMode::RawPassthrough),
        super::NativeCtrlC::Literal
    ));
    assert!(super::is_replay_control(
        &input(RawInputMode::Delay { generation: 2 }),
        super::NativeCtrlC::Literal
    ));
    assert!(super::is_replay_control(
        &InputRead::Eof,
        super::NativeCtrlC::Literal
    ));
    assert!(super::is_replay_control(
        &InputRead::Error(io::Error::other("reader failed")),
        super::NativeCtrlC::Literal
    ));
}

#[test]
fn native_literal_hint_does_not_block_fifo_but_eof_and_delay_do() {
    let literal = super::NativeCtrlC::Literal;
    assert!(!super::replay_hint_blocks_writing(
        super::REPLAY_CTRL_C,
        &RawInputMode::RawPassthrough,
        literal
    ));
    assert!(super::replay_hint_blocks_writing(
        super::REPLAY_EOF,
        &RawInputMode::RawPassthrough,
        literal
    ));
    assert!(super::replay_hint_blocks_writing(
        super::REPLAY_CTRL_C,
        &RawInputMode::Delay { generation: 2 },
        literal
    ));
    assert!(super::replay_hint_blocks_writing(
        super::REPLAY_CTRL_C,
        &RawInputMode::RawPassthrough,
        super::NativeCtrlC::NonTerminal
    ));
}
