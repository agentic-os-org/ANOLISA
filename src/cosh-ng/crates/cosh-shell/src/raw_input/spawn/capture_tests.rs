//! Generation-cutoff regression tests for the capture quarantine
//! (split out of capture.rs to keep it under the 700-line layout gate).

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom};

use super::*;
use crate::raw_input::RawInputCapture;

#[test]
fn generation_cutoff_does_not_retry_input_into_the_replacement_capture() {
    let path = std::env::temp_dir().join(format!(
        "cosh-shell-capture-cutoff-retry-{}",
        std::process::id()
    ));
    let mut master = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("test output file");
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
    let stale_mode = RawInputMode::Capture {
        capture: previous.clone(),
        generation: 41,
        installed_at: Instant::now(),
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::Draining {
        previous_capture: previous.clone(),
        generation: 41,
        next_capture: Some(next.clone()),
        invalidated: false,
        post_owner: PostCaptureOwner::MainPrompt,
    }));
    let (input_tx, input_rx) = mpsc::channel();
    let classifier = InputClassifier::default();
    let mut card_state = CardInputState::default();
    let mut quarantine = CaptureOwnedInput::default();
    let mut deferred_input = None;
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::super::MainPromptGate::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &input_tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };

    relay_input_chunk(
        b"stale",
        stale_mode,
        &mut card_state,
        &mut quarantine,
        &mut deferred_input,
        RelayReadContext {
            expected_capture_generation: Some(41),
            ..RelayReadContext::default()
        },
        &mut relay,
    )
    .expect("relay stale input");

    assert!(!input_rx
        .try_iter()
        .any(|event| matches!(event, RawInputEvent::CardInput(target, _) if target == "q-2")));
    master.sync_all().expect("sync test output");
    assert!(fs::read(&path).expect("read test output").is_empty());
    assert!(matches!(
        current_raw_input_mode(&input_mode),
        RawInputMode::Capture {
            capture: RawInputCapture::Question { id, .. },
            ..
        } if id == "q-2"
    ));

    master.set_len(0).expect("truncate test output");
    master.seek(SeekFrom::Start(0)).expect("rewind test output");
    *input_mode.lock().expect("input mode") = RawInputMode::Draining {
        previous_capture: previous,
        generation: 41,
        next_capture: Some(next),
        invalidated: false,
        post_owner: PostCaptureOwner::MainPrompt,
    };
    let draining_snapshot = current_raw_input_mode(&input_mode);
    let mut card_state = CardInputState::default();
    let mut quarantine = CaptureOwnedInput::default();
    let mut deferred_input = None;
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::super::MainPromptGate::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &input_tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };
    relay_input_chunk(
        b"later",
        draining_snapshot,
        &mut card_state,
        &mut quarantine,
        &mut deferred_input,
        RelayReadContext {
            expected_capture_generation: Some(41),
            ..RelayReadContext::default()
        },
        &mut relay,
    )
    .expect("relay input across draining snapshot");
    abandon_active_capture(&input_mode);
    drain_abandoned_capture(&mut card_state, &mut quarantine, &mut relay)
        .expect("drain replacement capture");

    assert!(!input_rx
        .try_iter()
        .any(|event| matches!(event, RawInputEvent::CardInput(target, _) if target == "q-2")));
    master.sync_all().expect("sync test output");
    assert!(fs::read(&path).expect("read test output").is_empty());
    fs::remove_file(path).ok();
}
