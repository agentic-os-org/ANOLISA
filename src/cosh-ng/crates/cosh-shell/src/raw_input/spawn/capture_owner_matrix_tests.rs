//! Post-capture owner matrix tests (#1913): the drain must land the
//! quarantined replay on the observer-acknowledged owner, re-verify live
//! ownership before delivery, and reject visibly whenever no safe owner
//! can consume the bytes.

use super::*;

/// The ack's post-capture owner must survive the drain: under a delay
/// owner a buffered Ctrl-C cancels the agent (CtrlC event) instead of
/// leaking into bash through the terminal passthrough path.
#[test]
fn delay_owner_replays_buffered_ctrl_c_as_agent_cancel() {
    let capture = config_capture();
    let mut harness = MatrixHarness::new(
        "owner-delay-ctrl-c",
        RawInputMode::Capture {
            capture: capture.clone(),
            generation: 31,
            installed_at: Instant::now(),
        },
    );
    let ack = spawn_submission_ack(
        harness.input_mode.clone(),
        RawObserverAction::DelayShellOutput,
    );

    harness.relay_chunk(
        b"\r\x03",
        RawInputMode::Capture {
            capture,
            generation: 31,
            installed_at: Instant::now(),
        },
    );
    ack.join().expect("ack thread");

    let events = harness.events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CtrlC)),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CaptureInputRejected { .. })),
        "{events:?}"
    );
    assert!(harness.pty_bytes().is_empty());
    assert!(matches!(
        current_raw_input_mode(&harness.input_mode),
        RawInputMode::Delay { .. }
    ));
}

/// The ack's post-capture owner must survive the drain: under a raw
/// passthrough owner the buffered bytes reach the PTY verbatim instead
/// of being reinterpreted by the prompt classifier.
#[test]
fn raw_passthrough_owner_replays_buffered_bytes_verbatim() {
    let capture = config_capture();
    let mut harness = MatrixHarness::new(
        "owner-raw-passthrough",
        RawInputMode::Capture {
            capture: capture.clone(),
            generation: 33,
            installed_at: Instant::now(),
        },
    );
    let ack = spawn_submission_ack(
        harness.input_mode.clone(),
        RawObserverAction::RawPassthrough,
    );

    harness.relay_chunk(
        b"\rvi-keys",
        RawInputMode::Capture {
            capture,
            generation: 33,
            installed_at: Instant::now(),
        },
    );
    ack.join().expect("ack thread");

    let events = harness.events();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CaptureInputRejected { .. })),
        "{events:?}"
    );
    assert_eq!(harness.pty_bytes(), b"vi-keys");
    assert!(matches!(
        current_raw_input_mode(&harness.input_mode),
        RawInputMode::RawPassthrough
    ));
}

/// The replay verdict must re-check live ownership: if an observer armed
/// a new capture after the drain terminal was installed, the stale
/// snapshot never reaches the PTY and the bytes reject visibly.
#[test]
fn replay_rejects_when_live_owner_superseded_the_drain_terminal() {
    let next = question_capture("q-superseded");
    let mut harness = MatrixHarness::new(
        "owner-superseded",
        RawInputMode::Capture {
            capture: next.clone(),
            generation: 36,
            installed_at: Instant::now(),
        },
    );
    assert!(!harness.quarantine.observe(b"must not leak\n"));
    let installed = Some((
        RawInputMode::Terminal {
            previous_capture: config_capture(),
            generation: 35,
        },
        PostCaptureOwner::MainPrompt,
    ));

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
        35,
        &mut relay,
    )
    .expect("replay verdict");

    let events = harness.events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::CaptureInputRejected { generation: 35, byte_len } if *byte_len == 14
        )),
        "{events:?}"
    );
    assert!(harness.pty_bytes().is_empty());
    assert!(matches!(
        current_raw_input_mode(&harness.input_mode),
        RawInputMode::Capture { capture, .. } if capture == next
    ));
}

/// An ownership cutover during the submit wait (e.g. a prompt ghost
/// replacing the draining mode) surfaces the quarantined bytes as a
/// rejection instead of returning with a silent buffer.
#[test]
fn ghost_cutover_during_submit_wait_rejects_quarantine() {
    let capture = config_capture();
    let mut harness = MatrixHarness::new(
        "owner-ghost-cutover",
        RawInputMode::Capture {
            capture: capture.clone(),
            generation: 37,
            installed_at: Instant::now(),
        },
    );
    let mode_for_cutover = harness.input_mode.clone();
    let cutover = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if matches!(
                current_raw_input_mode(&mode_for_cutover),
                RawInputMode::Submitted { .. }
            ) {
                *mode_for_cutover.lock().expect("input mode") = RawInputMode::PromptGhost {
                    text: "ghost".to_string(),
                    route: PromptGhostRoute::NativeShell,
                };
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("capture never reached the submitted state");
    });

    harness.relay_chunk(
        b"\rmust be surfaced\n",
        RawInputMode::Capture {
            capture,
            generation: 37,
            installed_at: Instant::now(),
        },
    );
    cutover.join().expect("cutover thread");

    let events = harness.events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::CaptureInputRejected { generation: 37, byte_len } if *byte_len == 17
        )),
        "{events:?}"
    );
    assert!(harness.quarantine.take_bytes().is_empty());
    assert!(harness.pty_bytes().is_empty());
}

/// A hold owner cannot deliver ordinary text (held input only recognizes
/// cancel controls), so the buffered batch surfaces as a visible
/// rejection instead of vanishing after take_bytes().
#[test]
fn hold_owner_rejects_buffered_text_visibly() {
    let capture = config_capture();
    let mut harness = MatrixHarness::new(
        "owner-hold-text",
        RawInputMode::Capture {
            capture: capture.clone(),
            generation: 39,
            installed_at: Instant::now(),
        },
    );
    let ack = spawn_submission_ack(
        harness.input_mode.clone(),
        RawObserverAction::HoldShellOutput,
    );

    harness.relay_chunk(
        b"\rmust be delivered or rejected\n",
        RawInputMode::Capture {
            capture,
            generation: 39,
            installed_at: Instant::now(),
        },
    );
    ack.join().expect("ack thread");

    let events = harness.events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::CaptureInputRejected { generation: 39, byte_len } if *byte_len == 30
        )),
        "{events:?}"
    );
    assert!(harness.pty_bytes().is_empty());
    assert!(matches!(
        current_raw_input_mode(&harness.input_mode),
        RawInputMode::Hold
    ));
}

/// The hold owner still honors the cancel semantics of held input: a
/// buffered Ctrl-C emits the cancel event alongside the rejection.
#[test]
fn hold_owner_keeps_cancel_semantics_for_buffered_ctrl_c() {
    let capture = config_capture();
    let mut harness = MatrixHarness::new(
        "owner-hold-ctrl-c",
        RawInputMode::Capture {
            capture: capture.clone(),
            generation: 41,
            installed_at: Instant::now(),
        },
    );
    let ack = spawn_submission_ack(
        harness.input_mode.clone(),
        RawObserverAction::HoldShellOutput,
    );

    harness.relay_chunk(
        b"\r\x03",
        RawInputMode::Capture {
            capture,
            generation: 41,
            installed_at: Instant::now(),
        },
    );
    ack.join().expect("ack thread");

    let events = harness.events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CtrlC)),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::CaptureInputRejected { generation: 41, .. }
        )),
        "{events:?}"
    );
    assert!(harness.pty_bytes().is_empty());
}

/// A superseded hold owner must not disturb its replacement: when a new
/// capture armed after the drain, the dead chain's buffered Ctrl-C is
/// rejected without emitting any held-control event.
#[test]
fn superseded_hold_owner_rejects_ctrl_c_without_control_events() {
    let next = question_capture("q-hold-cutover");
    let mut harness = MatrixHarness::new(
        "owner-hold-cutover",
        RawInputMode::Capture {
            capture: next,
            generation: 44,
            installed_at: Instant::now(),
        },
    );
    assert!(!harness.quarantine.observe(b"\x03"));
    let installed = Some((RawInputMode::Hold, PostCaptureOwner::Hold));

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
        43,
        &mut relay,
    )
    .expect("replay verdict");

    let events = harness.events();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CtrlC)),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::CaptureInputRejected {
                generation: 43,
                byte_len: 1
            }
        )),
        "{events:?}"
    );
    assert!(harness.pty_bytes().is_empty());
}
