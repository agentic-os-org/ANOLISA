use super::*;

struct TerminalEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl TerminalEnv {
    fn new() -> Self {
        let mut saved = Vec::new();
        for (key, value) in [
            ("TERM", "xterm-256color"),
            ("COSH_SHELL_RENDER", "terminal"),
        ] {
            saved.push((key, std::env::var_os(key)));
            std::env::set_var(key, value);
        }
        Self(saved)
    }
}

impl Drop for TerminalEnv {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn snapshot(bytes: &[u8]) -> crate::raw_input::PromptSnapshot {
    let exchange = crate::raw_input::PromptEpochExchange::default();
    let epoch = exchange.open();
    exchange.publish_with_origin(epoch, bytes, true);
    exchange.ready_snapshot(epoch).unwrap()
}

#[test]
fn initial_question_uses_a_compact_card_and_preserves_the_original_question() {
    let _env = crate::diagnostics::test_env::env_guard();
    let _terminal = TerminalEnv::new();
    let mut state = InlineState::default();
    state.startup_session.prompt_snapshot = Some(snapshot(b"native$ "));
    let question = "  Check memory; do not change files.  ";
    let mut out = Vec::new();
    write_initial_question(&state, question, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains('╭') && text.contains('╯'), "{text:?}");
    assert!(text.contains("/help"), "{text}");
    assert!(!text.contains("auto-approved"), "{text}");
    assert!(!text.contains("█▀▀"), "{text}");
    assert!(text.ends_with(&format!("native$ {question}\n")), "{text:?}");
}

#[test]
fn initial_question_does_not_leave_a_blank_native_prompt_before_the_identity() {
    let _env = crate::diagnostics::test_env::env_guard();
    let _terminal = TerminalEnv::new();
    let mut state = InlineState::default();
    state.startup_session.prompt_snapshot = Some(snapshot(b"native$ "));
    let mut out = Vec::new();
    write_initial_question(&state, "original question", &mut out).unwrap();
    assert!(out.starts_with(b"\r\x1b[2K"), "{out:?}");
}

#[test]
fn initial_question_replaces_a_linear_multiline_prompt_before_replaying_it() {
    let _env = crate::diagnostics::test_env::env_guard();
    let _terminal = TerminalEnv::new();
    let mut state = InlineState::default();
    state.startup_session.prompt_snapshot = Some(snapshot(b"project\r\nnative$ "));
    let mut out = Vec::new();
    write_initial_question(&state, "original question", &mut out).unwrap();
    assert!(out.starts_with(b"\r\x1b[2K\x1b[1A\r\x1b[2K"), "{out:?}");
    assert!(out.ends_with(b"project\r\nnative$ original question\n"));
}

#[test]
fn initial_question_keeps_the_disabled_ai_scope_and_significant_modes() {
    let _env = crate::diagnostics::test_env::env_guard();
    let _terminal = TerminalEnv::new();
    for language in [Language::ZhCn, Language::EnUs] {
        for mode in [CoshApprovalMode::Recommend, CoshApprovalMode::Trust] {
            let mut state = InlineState {
                language,
                approval_mode: mode,
                ..InlineState::default()
            };
            state.personalization.ai_disabled = true;
            state.startup_session.prompt_snapshot = Some(snapshot(b"native$ "));
            let mut out = Vec::new();
            write_initial_question(&state, "original question", &mut out).unwrap();
            let text = String::from_utf8(out).unwrap();
            assert!(text.contains("/help"), "{text}");
            assert!(state.personalization.ai_disabled);
            assert!(
                text.contains(match language {
                    Language::ZhCn => "后续",
                    Language::EnUs => "subsequent",
                }),
                "{text}"
            );
            let expected = state.i18n().t(match mode {
                CoshApprovalMode::Recommend => MessageId::ApprovalModeRecommendFooter,
                _ => MessageId::ApprovalModeTrustFooter,
            });
            let compact = |text: &str| {
                text.chars()
                    .filter(|ch| !ch.is_whitespace() && !"│╭╮╰╯─".contains(*ch))
                    .collect::<String>()
            };
            assert!(compact(&text).contains(&compact(expected)), "{text}");
        }
    }
}

#[test]
fn initial_question_does_not_erase_an_unknown_prompt_origin() {
    let _env = crate::diagnostics::test_env::env_guard();
    let _terminal = TerminalEnv::new();
    let exchange = crate::raw_input::PromptEpochExchange::default();
    let epoch = exchange.open();
    exchange.publish(epoch, b"\r\nnative$ ");
    let mut state = InlineState::default();
    state.startup_session.prompt_snapshot = Some(exchange.ready_snapshot(epoch).unwrap());
    let mut out = Vec::new();
    write_initial_question(&state, "question", &mut out).unwrap();
    assert!(out.starts_with(b"\r\n"));
    assert!(!out.windows(4).any(|part| part == b"\x1b[2K"));
    assert!(!out.windows(4).any(|part| part == b"\x1b[1A"));
}

#[test]
fn initial_question_plain_output_does_not_emit_ansi_cleanup() {
    let _env = crate::diagnostics::test_env::env_guard();
    let _terminal = TerminalEnv::new();
    std::env::set_var("COSH_SHELL_RENDER", "plain");
    let mut state = InlineState::default();
    state.startup_session.prompt_snapshot = Some(snapshot(b"native$ "));
    let mut out = Vec::new();
    write_initial_question(&state, "question", &mut out).unwrap();
    assert!(!out.contains(&0x1b), "{out:?}");
    assert!(out.ends_with(b"native$ question\n"));
}
