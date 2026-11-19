use super::*;

// issue #10318: a chunked bracketed paste must not redraw the whole
// accumulated candidate line per chunk (O(n²/chunk) terminal output).
#[test]
fn large_paste_candidate_echo_stays_bounded() {
    let (path, mut master) = output_file("paste-echo-budget");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    let payload_len = 102_400;
    let mut payload = vec![b'P'];
    payload.resize(payload_len, b'x');

    relay_passthrough_input(b"\x1b[200~", &mut relay).expect("paste opener");
    for chunk in payload.chunks(2048) {
        relay_passthrough_input(chunk, &mut relay).expect("paste payload");
    }
    relay_passthrough_input(b"\x1b[201~", &mut relay).expect("paste closer");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    let redraw_bytes: usize = events
        .iter()
        .map(|event| match event {
            RawInputEvent::CandidateRedraw { input, .. } => input.len(),
            _ => 0,
        })
        .sum();
    assert!(
        redraw_bytes <= 2 * payload_len + 64,
        "candidate echo must stay bounded, got {redraw_bytes} bytes for {payload_len} payload bytes"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            RawInputEvent::UserIntercept(..) | RawInputEvent::PromptDraftOpen { .. }
        )),
        "plain paste stays Shell-owned: {events:?}"
    );

    let mut expected = b"\x1b[200~".to_vec();
    expected.extend_from_slice(&payload);
    expected.extend_from_slice(b"\x1b[201~");
    assert_eq!(
        fs::read(&path).expect("read test output"),
        expected,
        "paste wrapper and payload must reach the PTY byte-identical"
    );
    fs::remove_file(path).ok();
}

// A pasted Backspace edits the draft while echo is coalesced: the single
// redraw after the closer must carry the edited display.
#[test]
fn paste_payload_backspace_final_display_is_correct() {
    let (path, mut master) = output_file("paste-backspace-fallback");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"\x1b[200~", &mut relay).expect("paste opener");
    relay_passthrough_input(b"abc", &mut relay).expect("paste head");
    relay_passthrough_input(b"\x7f", &mut relay).expect("pasted backspace");
    relay_passthrough_input(b"def", &mut relay).expect("paste tail");
    relay_passthrough_input(b"\x1b[201~", &mut relay).expect("paste closer");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::CandidateRedraw { input, .. } if input == b"abdef"
        )),
        "close redraw must carry the edited display: {events:?}"
    );
    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"\x1b[200~abdef\x1b[201~",
        "edited payload must reach the PTY byte-identical"
    );
    fs::remove_file(path).ok();
}

// issue #10318 fix anchor: while a bracketed paste is streaming, candidate
// echo is coalesced; exactly one full redraw fires after the closer.
#[test]
fn streaming_paste_coalesces_echo_into_one_close_redraw() {
    let (path, mut master) = output_file("paste-coalesce");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    let payload = b"printf coalesced".to_vec();
    relay_passthrough_input(b"\x1b[200~", &mut relay).expect("paste opener");
    for chunk in payload.chunks(4) {
        relay_passthrough_input(chunk, &mut relay).expect("paste payload");
    }
    relay_passthrough_input(b"\x1b[201~", &mut relay).expect("paste closer");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    let redraws: Vec<&Vec<u8>> = events
        .iter()
        .filter_map(|event| match event {
            RawInputEvent::CandidateRedraw { input, .. } => Some(input),
            _ => None,
        })
        .collect();
    assert_eq!(
        redraws.len(),
        1,
        "streaming paste must coalesce echo into one close redraw: {redraws:?}"
    );
    assert_eq!(redraws[0], &payload);
    let mut expected = b"\x1b[200~".to_vec();
    expected.extend_from_slice(&payload);
    expected.extend_from_slice(b"\x1b[201~");
    assert_eq!(fs::read(&path).expect("read test output"), expected);
    fs::remove_file(path).ok();
}

// Ghost-accepted drafts keep the forced-intercept path; a paste into that
// path must coalesce echo too (secondary amplification site, relay.rs force
// branch).
#[test]
fn forced_intercept_paste_coalesces_echo() {
    let (path, mut master) = output_file("paste-forced-coalesce");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    // Mirror the ghost-accept state (relay.rs AgentIntercept route): a typed
    // lone `??` would submit as an empty draft, so inject the forced draft
    // directly instead of routing keystrokes.
    relay.line_buffer.bytes.extend_from_slice(b"??");
    relay.line_buffer.soft_newline_enabled = true;
    relay.line_buffer.force_agent_intercept = true;
    relay_passthrough_input(b"\x1b[200~", &mut relay).expect("paste opener");
    for chunk in b"hello forced paste".chunks(4) {
        relay_passthrough_input(chunk, &mut relay).expect("paste payload");
    }
    relay_passthrough_input(b"\x1b[201~", &mut relay).expect("paste closer");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    let paste_redraws: Vec<&Vec<u8>> = events
        .iter()
        .filter_map(|event| match event {
            RawInputEvent::CandidateRedraw { input, .. } if input.starts_with(b"??hello") => {
                Some(input)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        paste_redraws.len(),
        1,
        "forced-intercept paste must coalesce echo: {paste_redraws:?}"
    );
    assert_eq!(paste_redraws[0], b"??hello forced paste");
    assert!(
        !events.iter().any(|event| matches!(
            event,
            RawInputEvent::UserIntercept(..) | RawInputEvent::PromptGhostIntercept { .. }
        )),
        "forced draft without a submit newline must stay held: {events:?}"
    );
    assert_eq!(fs::read(&path).expect("read test output"), b"");
    fs::remove_file(path).ok();
}

// A typed slash prefix keeps the candidate active before the paste; the
// coalesced close redraw must carry prefix + payload, and the pasted slash
// line still waits for an explicit Enter (no PTY leak).
#[test]
fn typed_slash_prefix_paste_coalesces_echo() {
    let (path, mut master) = output_file("paste-slash-prefix");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"/mode ", &mut relay).expect("slash prefix");
    relay_passthrough_input(b"\x1b[200~", &mut relay).expect("paste opener");
    for chunk in b"approval recommend".chunks(4) {
        relay_passthrough_input(chunk, &mut relay).expect("paste payload");
    }
    relay_passthrough_input(b"\x1b[201~", &mut relay).expect("paste closer");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    let paste_redraws: Vec<&Vec<u8>> = events
        .iter()
        .filter_map(|event| match event {
            RawInputEvent::CandidateRedraw { input, .. } if input.starts_with(b"/mode a") => {
                Some(input)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        paste_redraws.len(),
        1,
        "typed-prefix paste must coalesce echo: {paste_redraws:?}"
    );
    assert_eq!(paste_redraws[0], b"/mode approval recommend");
    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"",
        "pasted slash line must wait for explicit Enter"
    );
    fs::remove_file(path).ok();
}

// CJK payload split mid-codepoint across chunks: the coalesced close redraw
// must carry the complete bytes and the PTY replay stays byte-identical.
#[test]
fn cjk_paste_split_multibyte_coalesces_echo() {
    let (path, mut master) = output_file("paste-cjk-split");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    let payload = "中文分析负载".as_bytes();
    relay_passthrough_input(b"\x1b[200~", &mut relay).expect("paste opener");
    let mut offset = 0;
    for size in [1, 2, 3, 5, 7] {
        relay_passthrough_input(&payload[offset..offset + size], &mut relay).expect("cjk chunk");
        offset += size;
    }
    relay_passthrough_input(b"\x1b[201~", &mut relay).expect("paste closer");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    let redraws: Vec<&Vec<u8>> = events
        .iter()
        .filter_map(|event| match event {
            RawInputEvent::CandidateRedraw { input, .. } => Some(input),
            _ => None,
        })
        .collect();
    assert_eq!(
        redraws.len(),
        1,
        "CJK paste must coalesce echo: {redraws:?}"
    );
    assert_eq!(redraws[0], payload);
    let mut expected = b"\x1b[200~".to_vec();
    expected.extend_from_slice(payload);
    expected.extend_from_slice(b"\x1b[201~");
    assert_eq!(fs::read(&path).expect("read test output"), expected);
    fs::remove_file(path).ok();
}

// Delimiters split across reads (pending_partial) into an active ?? draft
// must not break coalescing: one close redraw, no PTY leak (card route).
#[test]
fn split_delimiters_paste_coalesces_echo() {
    let (path, mut master) = output_file("paste-split-delims");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"?", &mut relay).expect("first ?");
    relay_passthrough_input(b"?", &mut relay).expect("second ?");
    relay_passthrough_input(b"\x1b[2", &mut relay).expect("opener head");
    relay_passthrough_input(b"00~", &mut relay).expect("opener tail");
    for chunk in b"printf split".chunks(4) {
        relay_passthrough_input(chunk, &mut relay).expect("paste payload");
    }
    relay_passthrough_input(b"\x1b[20", &mut relay).expect("closer head");
    relay_passthrough_input(b"1~", &mut relay).expect("closer tail");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    let paste_redraws: Vec<&Vec<u8>> = events
        .iter()
        .filter_map(|event| match event {
            RawInputEvent::CandidateRedraw { input, .. } if input.starts_with(b"??printf") => {
                Some(input)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        paste_redraws.len(),
        1,
        "split-delimiter paste must coalesce echo: {paste_redraws:?}"
    );
    assert_eq!(paste_redraws[0], b"??printf split");
    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"",
        "?? draft paste must stay Cosh-owned"
    );
    fs::remove_file(path).ok();
}
