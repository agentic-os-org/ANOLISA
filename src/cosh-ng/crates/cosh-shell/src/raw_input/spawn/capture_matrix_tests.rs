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
            .expect("drain abandoned capture");
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
