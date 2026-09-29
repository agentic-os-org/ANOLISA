use super::*;

fn capture(text: &str, agent_composer: bool) -> RawInputCapture {
    RawInputCapture::PromptDraft {
        id: "composer-1".into(),
        initial_text: text.into(),
        completion: None,
        agent_composer,
        workspace_cwd: None,
    }
}

fn submitted(events: &[RawInputEvent]) -> (&str, bool) {
    events
        .iter()
        .find_map(|event| match event {
            RawInputEvent::PromptDraftSubmit { text, slash, .. } => Some((text.as_str(), *slash)),
            _ => None,
        })
        .expect("submitted draft")
}

#[test]
fn composer_completes_and_submits_from_live_input_in_one_chunk() {
    let capture = capture("", true);
    for (input, expected) in [
        (b"/ho\t\r".as_slice(), "/hooks "),
        (b"/ho\r".as_slice(), "/hooks "),
        (b"/\r".as_slice(), "/help "),
        (b"/sta\x1b[B\r".as_slice(), "/stats "),
        (b"/sta\x1b[B\x7f\x7f\x7fho\r".as_slice(), "/hooks "),
        (b"/sta\x1b[B\t\r".as_slice(), "/stats "),
        (b"/sta\x1b[B\x1b[A\t\r".as_slice(), "/status "),
        (b"/sta\x1b[B\x7f\x7f\x7fho\t\r".as_slice(), "/hooks "),
    ] {
        let mut state = CardInputState::default();
        state.apply_capture(&capture);
        let (events, remainder) = state.consume_split(&capture, input);
        assert!(remainder.is_empty());
        assert_eq!(submitted(&events), (expected, true));
    }
}

#[test]
fn composer_can_select_every_public_command_across_chunks() {
    let capture = capture("/", true);
    let mut state = CardInputState::default();
    state.apply_capture(&capture);
    let commands = slash_completions("/", 0, 1);
    for index in 1..commands.len() {
        let events = state.consume(&capture, b"\x1b[B");
        assert!(
            matches!(events.last(), Some(RawInputEvent::PromptDraftChanged {
            selected_completion, ..
        }) if *selected_completion == index)
        );
        // A delayed repaint must not reset the live editor's selection.
        state.apply_capture(&capture);
    }
    let events = state.consume(&capture, b"\r");
    assert_eq!(
        submitted(&events),
        (format!("{} ", commands.last().unwrap()).as_str(), true)
    );
}

#[test]
fn composer_enter_preserves_arguments_multiline_drafts_and_other_routes() {
    for (text, composer, slash) in [
        ("/ho details", true, true),
        ("\n/ho", true, true),
        ("/unknown", true, true),
        ("/help", false, false),
        ("/skill:repo-review", true, false),
        ("/tmp/file", true, false),
        ("?? /help", true, false),
    ] {
        let capture = capture(text, composer);
        let mut state = CardInputState::default();
        state.apply_capture(&capture);
        let events = state.consume(&capture, b"\r");
        assert_eq!(submitted(&events), (text, slash));
    }
}

#[test]
fn composer_enter_preserves_arguments_when_editing_the_command_token() {
    let capture = capture("/ho details", true);
    let mut state = CardInputState::default();
    state.apply_capture(&capture);
    let events = state.consume(&capture, b"\x01\x1b[C\x1b[C\x1b[C\r");
    assert_eq!(submitted(&events), ("/ho details", true));
}

#[test]
fn composer_paste_keeps_tabs_and_newlines_as_data() {
    let capture = capture("", true);
    let mut state = CardInputState::default();
    state.apply_capture(&capture);
    let events = state.consume(
        &capture,
        b"\x1b[200~/skill:repo-review\tinspect\n@src\x1b[201~\r",
    );
    assert_eq!(
        submitted(&events),
        ("/skill:repo-review\tinspect\n@src", false)
    );
}

#[test]
fn composer_rejects_runtime_completion_after_text_or_cursor_changes() {
    let mut capture = capture("review @sr", true);
    if let RawInputCapture::PromptDraft { completion, .. } = &mut capture {
        *completion = Some(Box::new(("@src/".into(), (0, 10))));
    }
    for (input, expected) in [
        (b"c\t\r".as_slice(), "review @src"),
        (b"\x01\t\r".as_slice(), "review @sr"),
    ] {
        let mut state = CardInputState::default();
        state.apply_capture(&capture);
        let events = state.consume(&capture, input);
        assert_eq!(submitted(&events), (expected, false));
    }
}

#[test]
fn composer_bare_skill_uses_the_runtime_skill_completion() {
    let mut capture = capture("/skill", true);
    if let RawInputCapture::PromptDraft { completion, .. } = &mut capture {
        *completion = Some(Box::new(("/skill:repo-review ".into(), (0, 6))));
    }
    let mut state = CardInputState::default();
    state.apply_capture(&capture);
    let events = state.consume(&capture, b"\tinspect\r");
    assert_eq!(submitted(&events), ("/skill:repo-review inspect", false));
}

#[test]
fn composer_submit_retains_its_workspace() {
    let mut capture = capture("/health", true);
    if let RawInputCapture::PromptDraft { workspace_cwd, .. } = &mut capture {
        *workspace_cwd = Some("/workspace/project".into());
    }
    let mut state = CardInputState::default();
    state.apply_capture(&capture);
    let events = state.consume(&capture, b"\r");
    assert!(
        matches!(events.last(), Some(RawInputEvent::PromptDraftSubmit {
        workspace_cwd: Some(cwd), slash: true, ..
    }) if cwd == "/workspace/project")
    );
}

#[test]
fn composer_existing_paths_survive_completion_and_submit() {
    for path in ["/tmp", "/etc", "/tmp/file"] {
        for keys in [b"\r".as_slice(), b"\t\r", b"\x01\x1b[C\t\r"] {
            let capture = capture(path, true);
            let mut state = CardInputState::default();
            state.apply_capture(&capture);
            let events = state.consume(&capture, keys);
            assert_eq!(submitted(&events), (path, false));
        }
    }
}
