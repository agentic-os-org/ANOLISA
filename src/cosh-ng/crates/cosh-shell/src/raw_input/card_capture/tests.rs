use super::{CardInputState, RawInputCapture, RawInputEvent};

#[test]
fn submitted_capture_preserves_same_read_suffix() {
    let capture = RawInputCapture::Question {
        id: "question-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    let (events, remainder) = state.consume_split(&capture, b"yes\nWho are you\n");

    assert_eq!(
        events.last(),
        Some(&RawInputEvent::CardAnswer("yes".to_string()))
    );
    assert_eq!(remainder, b"Who are you\n");
}

#[test]
fn question_capture_custom_option_waits_for_text_before_submit() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 2,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\t\t\n"),
        vec![
            RawInputEvent::CardFocus("q-1".to_string(), 1),
            RawInputEvent::CardFocus("q-1".to_string(), 2),
            RawInputEvent::QuestionSubmitAttempt("q-1".to_string()),
        ]
    );
    assert_eq!(
        state.consume(&capture, "红色\n".as_bytes()),
        vec![
            RawInputEvent::CardInput("q-1".to_string(), "红色".to_string()),
            RawInputEvent::CardAnswer("红色".to_string())
        ]
    );
}

#[test]
fn question_capture_clears_free_text_after_submit() {
    let capture = RawInputCapture::Question {
        id: "q-clear".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"main\n").last(),
        Some(&RawInputEvent::CardAnswer("main".to_string()))
    );
    // free_text is cleared after submit; a second Enter must NOT replay the
    // previous answer.
    assert_eq!(
        state.consume(&capture, b"\n"),
        vec![RawInputEvent::QuestionSubmitAttempt("q-clear".to_string())]
    );
}

#[test]
fn text_question_backspaces_from_initial_text() {
    let capture = RawInputCapture::TextQuestion {
        id: "auth@field-0-1".to_string(),
        initial_text: "qwen3.7-max".to_string(),
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\x7f"),
        vec![RawInputEvent::CardInput(
            "auth@field-0-1".to_string(),
            "qwen3.7-ma".to_string(),
        )]
    );
}

#[test]
fn question_capture_emits_one_submission_per_input_batch() {
    let capture = RawInputCapture::Question {
        id: "q-burst".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    let events = state.consume(&capture, b"main\n\n");

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, RawInputEvent::CardAnswer(_)))
            .count(),
        1
    );
    assert_eq!(
        state.consume(&capture, b"\n"),
        vec![RawInputEvent::QuestionSubmitAttempt("q-burst".to_string())]
    );
}

#[test]
fn question_capture_custom_option_empty_submit_emits_attempt() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 2,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\t\t\n"),
        vec![
            RawInputEvent::CardFocus("q-1".to_string(), 1),
            RawInputEvent::CardFocus("q-1".to_string(), 2),
            RawInputEvent::QuestionSubmitAttempt("q-1".to_string()),
        ]
    );
}

#[test]
fn question_capture_multiple_empty_submit_emits_attempt() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 2,
        selected: 0,
        allow_free_text: true,
        multiple: true,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\n"),
        vec![RawInputEvent::QuestionSubmitAttempt("q-1".to_string())]
    );
}

#[test]
fn question_capture_strips_bracketed_paste_wrappers() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\x1b[200~sk-test\x1b[201~\n"),
        vec![
            RawInputEvent::CardInput("q-1".to_string(), "s".to_string()),
            RawInputEvent::CardInput("q-1".to_string(), "sk".to_string()),
            RawInputEvent::CardInput("q-1".to_string(), "sk-".to_string()),
            RawInputEvent::CardInput("q-1".to_string(), "sk-t".to_string()),
            RawInputEvent::CardInput("q-1".to_string(), "sk-te".to_string()),
            RawInputEvent::CardInput("q-1".to_string(), "sk-tes".to_string()),
            RawInputEvent::CardInput("q-1".to_string(), "sk-test".to_string()),
            RawInputEvent::CardAnswer("sk-test".to_string()),
        ]
    );
}

#[test]
fn secret_question_capture_marks_input_as_sensitive() {
    let capture = RawInputCapture::Question {
        id: "auth-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: true,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"hunter2\n"),
        vec![
            RawInputEvent::CardSecretInput("auth-1".to_string(), "h".to_string()),
            RawInputEvent::CardSecretInput("auth-1".to_string(), "hu".to_string()),
            RawInputEvent::CardSecretInput("auth-1".to_string(), "hun".to_string()),
            RawInputEvent::CardSecretInput("auth-1".to_string(), "hunt".to_string()),
            RawInputEvent::CardSecretInput("auth-1".to_string(), "hunte".to_string()),
            RawInputEvent::CardSecretInput("auth-1".to_string(), "hunter".to_string()),
            RawInputEvent::CardSecretInput("auth-1".to_string(), "hunter2".to_string()),
            RawInputEvent::CardSecretAnswer("hunter2".to_string()),
        ]
    );
}

#[test]
fn question_capture_strips_split_bracketed_paste_wrappers() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert!(state.consume(&capture, b"\x1b[20").is_empty());
    assert!(state.consume(&capture, b"0~").is_empty());
    assert_eq!(
        state.consume(&capture, b"sk"),
        vec![
            RawInputEvent::CardInput("q-1".to_string(), "s".to_string()),
            RawInputEvent::CardInput("q-1".to_string(), "sk".to_string()),
        ]
    );
    assert!(state.consume(&capture, b"\x1b[201").is_empty());
    assert_eq!(
        state.consume(&capture, b"~\n"),
        vec![RawInputEvent::CardAnswer("sk".to_string())]
    );
}

#[test]
fn question_capture_buffers_split_utf8_input() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);
    let input = "你好🙂";
    let mut events = Vec::new();

    for byte in input.as_bytes() {
        events.extend(state.consume(&capture, &[*byte]));
    }
    events.extend(state.consume(&capture, b"\n"));

    assert_eq!(
        events.last(),
        Some(&RawInputEvent::CardAnswer(input.to_string()))
    );
    assert!(
        events.iter().all(|event| !matches!(
            event,
            RawInputEvent::CardInput(_, value) if value.contains('\u{fffd}')
        )),
        "{events:?}"
    );
}

#[test]
fn question_capture_ignores_tilde_control_sequences() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"sk\x1b[3~\n"),
        vec![
            RawInputEvent::CardInput("q-1".to_string(), "s".to_string()),
            RawInputEvent::CardInput("q-1".to_string(), "sk".to_string()),
            RawInputEvent::CardAnswer("sk".to_string()),
        ]
    );
}

#[test]
fn question_capture_ignores_removed_answer_slash() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 2,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(state.consume(&capture, b"/answer Blue\n"), vec![]);
    assert_eq!(
        state.consume(&capture, b"\x1b[C\n"),
        vec![
            RawInputEvent::CardFocus("q-1".to_string(), 1),
            RawInputEvent::CardAnswer("2".to_string())
        ]
    );
}

#[test]
fn approval_capture_ignores_removed_decision_slashes() {
    let capture = RawInputCapture::Approval {
        id: "req-1".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(state.consume(&capture, b"/approve\n"), vec![]);
    assert_eq!(state.consume(&capture, b"/deny\n"), vec![]);
    assert_eq!(
        state.consume(&capture, b"\n"),
        vec![RawInputEvent::CardApprove("req-1".to_string())]
    );
}

#[test]
fn question_capture_still_submits_selected_option() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 2,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\t\n"),
        vec![
            RawInputEvent::CardFocus("q-1".to_string(), 1),
            RawInputEvent::CardAnswer("2".to_string())
        ]
    );
}

#[test]
fn question_capture_uses_initial_selected_option() {
    let capture = RawInputCapture::Question {
        id: "q-restored".to_string(),
        option_count: 5,
        selected: 2,
        allow_free_text: false,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\x1b[B\n"),
        vec![
            RawInputEvent::CardFocus("q-restored".to_string(), 3),
            RawInputEvent::CardAnswer("4".to_string())
        ]
    );
}

#[test]
fn question_capture_multiple_toggles_options_and_submits_indices() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 3,
        selected: 0,
        allow_free_text: true,
        multiple: true,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b" \t \n"),
        vec![
            RawInputEvent::CardToggle("q-1".to_string(), 0),
            RawInputEvent::CardFocus("q-1".to_string(), 1),
            RawInputEvent::CardToggle("q-1".to_string(), 1),
            RawInputEvent::CardAnswer("1,2".to_string())
        ]
    );
}

#[test]
fn question_capture_multiple_marks_custom_only_answer() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 3,
        selected: 0,
        allow_free_text: true,
        multiple: true,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"1\n").last(),
        Some(&RawInputEvent::CardAnswer("\n1".to_string()))
    );
}

#[test]
fn question_capture_multiple_preserves_checked_options_with_custom_answer() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 3,
        selected: 0,
        allow_free_text: true,
        multiple: true,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b" \t\t\tDocs\n"),
        vec![
            RawInputEvent::CardToggle("q-1".to_string(), 0),
            RawInputEvent::CardFocus("q-1".to_string(), 1),
            RawInputEvent::CardFocus("q-1".to_string(), 2),
            RawInputEvent::CardFocus("q-1".to_string(), 3),
            RawInputEvent::CardInput("q-1".to_string(), "D".to_string()),
            RawInputEvent::CardInput("q-1".to_string(), "Do".to_string()),
            RawInputEvent::CardInput("q-1".to_string(), "Doc".to_string()),
            RawInputEvent::CardInput("q-1".to_string(), "Docs".to_string()),
            RawInputEvent::CardAnswer("1\nDocs".to_string())
        ]
    );
}

#[test]
fn mode_capture_moves_focus_and_submits_selected_option() {
    let capture = RawInputCapture::Mode {
        id: "mode".to_string(),
        option_count: 2,
        selected: 0,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\x1b[C\n"),
        vec![
            RawInputEvent::ModeFocus("mode".to_string(), 1),
            RawInputEvent::ModeSet("mode".to_string(), 1)
        ]
    );
}

#[test]
fn mode_capture_uses_initial_selected_option() {
    let capture = RawInputCapture::Mode {
        id: "mode".to_string(),
        option_count: 2,
        selected: 1,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\n"),
        vec![RawInputEvent::ModeSet("mode".to_string(), 1)]
    );
}

#[test]
fn mode_capture_supports_tab_and_shift_tab_navigation() {
    let capture = RawInputCapture::Mode {
        id: "mode".to_string(),
        option_count: 3,
        selected: 1,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\t\x1b[Z"),
        vec![
            RawInputEvent::ModeFocus("mode".to_string(), 2),
            RawInputEvent::ModeFocus("mode".to_string(), 1),
        ]
    );
}

#[test]
fn mode_capture_supports_escape_and_ctrl_c_cancel() {
    let capture = RawInputCapture::Mode {
        id: "mode".to_string(),
        option_count: 3,
        selected: 0,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\x1b\x1b\x03"),
        vec![RawInputEvent::ModeCancel("mode".to_string())]
    );
}

#[test]
fn config_capture_saves_default_selection_and_cancels_second_option() {
    let capture = RawInputCapture::Config {
        id: "config".to_string(),
        option_count: 2,
        selected: 0,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\n"),
        vec![RawInputEvent::ConfigSave("config".to_string())]
    );

    state.apply_capture(&capture);
    assert_eq!(
        state.consume(&capture, b"\x1b[C\n"),
        vec![
            RawInputEvent::ConfigFocus("config".to_string(), 1),
            RawInputEvent::ConfigCancel("config".to_string())
        ]
    );
}

#[test]
fn config_language_capture_selects_language_and_cancels() {
    let capture = RawInputCapture::ConfigLanguage {
        id: "config-language".to_string(),
        option_count: 3,
        selected: 0,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\x1b[C\x1b[C\n"),
        vec![
            RawInputEvent::ConfigLanguageFocus("config-language".to_string(), 1),
            RawInputEvent::ConfigLanguageFocus("config-language".to_string(), 2),
            RawInputEvent::ConfigLanguageSet("config-language".to_string(), 2)
        ]
    );

    state.apply_capture(&capture);
    assert_eq!(
        state.consume(&capture, b"\x1b\n"),
        vec![RawInputEvent::ConfigLanguageCancel(
            "config-language".to_string()
        )]
    );
}

#[test]
fn session_capture_navigates_toggles_deletes_and_resumes() {
    let capture = RawInputCapture::Session {
        id: "session-panel".to_string(),
        option_count: 3,
        selected: 0,
        marked_for_clear: vec![false; 3],
        confirming_clear: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"j d"),
        vec![
            RawInputEvent::SessionFocus("session-panel".to_string(), 1),
            RawInputEvent::SessionToggle("session-panel".to_string(), 1),
            RawInputEvent::SessionDelete("session-panel".to_string()),
        ]
    );

    state.reset();
    state.apply_capture(&capture);
    assert_eq!(
        state.consume(&capture, b"\x1b[B\n"),
        vec![
            RawInputEvent::SessionFocus("session-panel".to_string(), 1),
            RawInputEvent::SessionResume("session-panel".to_string(), 1),
        ]
    );
}

#[test]
fn session_toggle_then_enter_uses_effective_marks_within_the_chunk() {
    let unmarked = RawInputCapture::Session {
        id: "session-panel".to_string(),
        option_count: 2,
        selected: 0,
        marked_for_clear: vec![false, false],
        confirming_clear: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&unmarked);

    assert_eq!(
        state.consume(&unmarked, b" \n"),
        vec![
            RawInputEvent::SessionToggle("session-panel".to_string(), 0),
            RawInputEvent::SessionDelete("session-panel".to_string()),
        ]
    );

    let marked = RawInputCapture::Session {
        id: "session-panel".to_string(),
        option_count: 2,
        selected: 0,
        marked_for_clear: vec![true, false],
        confirming_clear: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&marked);

    assert_eq!(
        state.consume(&marked, b" \n"),
        vec![
            RawInputEvent::SessionToggle("session-panel".to_string(), 0),
            RawInputEvent::SessionResume("session-panel".to_string(), 0),
        ]
    );
}

#[test]
fn session_clear_confirmation_accepts_and_cancels() {
    let capture = RawInputCapture::Session {
        id: "session-panel".to_string(),
        option_count: 0,
        selected: 0,
        marked_for_clear: Vec::new(),
        confirming_clear: true,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"y"),
        vec![RawInputEvent::SessionClearConfirm(
            "session-panel".to_string()
        )]
    );
    state.apply_capture(&capture);
    assert_eq!(
        state.consume(&capture, &[0x03]),
        vec![RawInputEvent::SessionCancel("session-panel".to_string())]
    );
}

/// TurnConsent 动作集（issue #1773）：线性索引到达 index 1 时回车产生
/// CardApproveTurn；max index 为 4（Details），不受渲染折行影响。
#[test]
fn approval_capture_turn_consent_selects_approve_turn_and_details() {
    let capture = RawInputCapture::Approval {
        id: "req-1".to_string(),
        action_set: crate::ui::ApprovalActionSet::TurnConsent,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\x1b[C\n"),
        vec![
            RawInputEvent::CardFocus("req-1".to_string(), 1),
            RawInputEvent::CardApproveTurn("req-1".to_string())
        ]
    );

    let mut state = CardInputState::default();
    state.apply_capture(&capture);
    // 右移 6 次在 max index 4（Details）处饱和。
    let events = state.consume(&capture, b"\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\x1b[C\n");
    assert_eq!(
        events.last(),
        Some(&RawInputEvent::CardDetails("req-1".to_string()))
    );
}

/// 已有焦点后新请求到达（Standard -> TurnConsent）：选择索引按动作值重映射，
/// 回车提交的仍是切换前高亮的动作，而不是新动作集里同索引的动作。
#[test]
fn approval_capture_remaps_selection_when_action_set_switches() {
    let standard = RawInputCapture::Approval {
        id: "req-1".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&standard);
    // 右移两次到 Deny（Standard index 2）。
    state.consume(&standard, b"\x1b[C\x1b[C");

    // 第二个同 run 请求到达，同一张卡切到 TurnConsent（Deny 变为 index 3）。
    let turn = RawInputCapture::Approval {
        id: "req-1".to_string(),
        action_set: crate::ui::ApprovalActionSet::TurnConsent,
    };
    state.apply_capture(&turn);
    assert_eq!(
        state.consume(&turn, b"\n"),
        vec![RawInputEvent::CardDeny("req-1".to_string())]
    );
}

/// 反向收缩（TurnConsent -> Standard）：原选中动作在新动作集缺失
/// （ApproveTurn）时回退到 Approve，与渲染侧 focus 回退一致。
#[test]
fn approval_capture_action_set_shrink_falls_back_to_approve() {
    let turn = RawInputCapture::Approval {
        id: "req-1".to_string(),
        action_set: crate::ui::ApprovalActionSet::TurnConsent,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&turn);
    // 右移到 ApproveTurn（TurnConsent index 1）。
    state.consume(&turn, b"\x1b[C");

    let standard = RawInputCapture::Approval {
        id: "req-1".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    state.apply_capture(&standard);
    assert_eq!(
        state.consume(&standard, b"\n"),
        vec![RawInputEvent::CardApprove("req-1".to_string())]
    );
}

/// 不同 id 的审批卡切换不走重映射：选择照常重置。
#[test]
fn approval_capture_new_card_resets_selection() {
    let first = RawInputCapture::Approval {
        id: "req-1".to_string(),
        action_set: crate::ui::ApprovalActionSet::TurnConsent,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&first);
    state.consume(&first, b"\x1b[C\x1b[C\x1b[C");

    let second = RawInputCapture::Approval {
        id: "req-2".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    state.apply_capture(&second);
    assert_eq!(
        state.consume(&second, b"\n"),
        vec![RawInputEvent::CardApprove("req-2".to_string())]
    );
}

#[test]
fn approval_capture_handles_split_escape_arrow_sequence() {
    let capture = RawInputCapture::Approval {
        id: "req-1".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert!(state.consume(&capture, b"\x1b[").is_empty());
    assert_eq!(
        state.consume(&capture, b"C\n"),
        vec![
            RawInputEvent::CardFocus("req-1".to_string(), 1),
            RawInputEvent::CardAlwaysTrust("req-1".to_string())
        ]
    );
}

#[test]
fn approval_capture_escape_then_enter_cancels_without_submit() {
    let capture = RawInputCapture::Approval {
        id: "req-1".to_string(),
        action_set: crate::ui::ApprovalActionSet::Standard,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    let (events, remainder) = state.consume_split(&capture, b"\x1b");
    assert_eq!(events, vec![RawInputEvent::CardCancel("req-1".to_string())]);
    assert!(remainder.is_empty());

    state.apply_capture(&capture);
    let (events, remainder) = state.consume_split(&capture, b"\x1bxnext");
    assert_eq!(events, vec![RawInputEvent::CardCancel("req-1".to_string())]);
    assert_eq!(remainder, b"xnext");
}

#[test]
fn question_capture_ctrl_c_and_escape_cancel_question() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 2,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    // Ctrl+C and ESC both abandon the capture, but stay distinguishable: a multi-step prompt
    // lets ESC step back, and only Ctrl+C means "abandon the whole thing".
    assert_eq!(
        state.consume(&capture, &[0x03]),
        vec![RawInputEvent::QuestionAbort("q-1".to_string())]
    );

    state.apply_capture(&capture);
    assert_eq!(
        state.consume(&capture, b"\x1b"),
        vec![RawInputEvent::QuestionCancel("q-1".to_string())]
    );
}

/// Abandoning the capture also has to stop the consumer. Whatever follows the interrupt in the
/// same read belongs to the shell, not to the question that just went away — otherwise the bytes
/// are swallowed and reported as edits to a prompt that no longer exists.
#[test]
fn question_capture_ctrl_c_stops_consuming_and_returns_the_rest() {
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 2,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    let (events, remainder) = state.consume_split(&capture, b"\x03xnext");

    assert_eq!(
        events,
        vec![RawInputEvent::QuestionAbort("q-1".to_string())]
    );
    assert_eq!(remainder, b"xnext");
}

#[test]
fn evidence_capture_sends_ignores_and_cancels() {
    let capture = RawInputCapture::Evidence {
        id: "evidence-1".to_string(),
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\n"),
        vec![RawInputEvent::EvidenceSend("evidence-1".to_string())]
    );

    state.apply_capture(&capture);
    assert_eq!(
        state.consume(&capture, b"i"),
        vec![RawInputEvent::EvidenceIgnore("evidence-1".to_string())]
    );

    state.apply_capture(&capture);
    assert_eq!(
        state.consume(&capture, &[0x03]),
        vec![RawInputEvent::EvidenceCancel("evidence-1".to_string())]
    );
}

// ---------------------------------------------------------------------------
// free_text must not carry over across capture switches or repeated submits
// ---------------------------------------------------------------------------

/// Helper: create a free-text Question capture with the given ID.
fn text_question(id: &str, secret: bool) -> RawInputCapture {
    RawInputCapture::Question {
        id: id.to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret,
    }
}

#[test]
fn submit_clears_free_text_for_next_capture() {
    // After submitting an answer on one capture and switching to a new
    // capture, the new capture must start with an empty free_text buffer.
    let cap_a = text_question("q-a", false);
    let cap_b = text_question("q-b", false);
    let mut state = CardInputState::default();

    state.apply_capture(&cap_a);
    let events = state.consume(&cap_a, b"answer-a\n");
    assert_eq!(
        events.last(),
        Some(&RawInputEvent::CardAnswer("answer-a".to_string()))
    );

    // reset() is called by consume_captured_input on release
    state.reset();

    state.apply_capture(&cap_b);
    let events = state.consume(&cap_b, b"x");
    assert_eq!(
        events.last(),
        Some(&RawInputEvent::CardInput(
            "q-b".to_string(),
            "x".to_string()
        ))
    );
}

#[test]
fn apply_capture_clears_free_text_when_id_changes_without_reset() {
    // apply_capture alone (without reset) must clear free_text when the
    // capture ID changes. This covers the direct Capture→Capture transition
    // path in mode.rs where reset() is NOT called.
    let cap_a = text_question("q-a", false);
    let cap_b = text_question("q-b", false);
    let mut state = CardInputState::default();

    state.apply_capture(&cap_a);
    state.consume(&cap_a, b"answer-a");
    // Do NOT call reset() — simulate direct Capture→Capture transition
    state.apply_capture(&cap_b);
    let events = state.consume(&cap_b, b"x");
    assert_eq!(
        events.last(),
        Some(&RawInputEvent::CardInput(
            "q-b".to_string(),
            "x".to_string()
        ))
    );
}

#[test]
fn second_submit_on_same_capture_yields_empty_attempt() {
    // After submitting on a capture, a second Enter must produce an
    // empty submit attempt, not a replay of the previous answer.
    let capture = text_question("q-a", false);
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    let (events, remainder) = state.consume_split(&capture, b"answer-a\n");
    assert_eq!(
        events.last(),
        Some(&RawInputEvent::CardAnswer("answer-a".to_string()))
    );
    assert!(remainder.is_empty());

    // free_text must be empty after submit — second Enter yields an attempt
    let (events, _) = state.consume_split(&capture, b"\n");
    assert_eq!(
        events.last(),
        Some(&RawInputEvent::QuestionSubmitAttempt("q-a".to_string()))
    );
}

#[test]
fn secret_submit_clears_free_text_for_next_capture() {
    // Secret captures must also clear free_text after submit.
    let cap_secret = text_question("q-secret", true);
    let cap_next = text_question("q-next", false);
    let mut state = CardInputState::default();

    state.apply_capture(&cap_secret);
    let events = state.consume(&cap_secret, b"sk-secret\n");
    assert_eq!(
        events.last(),
        Some(&RawInputEvent::CardSecretAnswer("sk-secret".to_string()))
    );

    state.reset();
    state.apply_capture(&cap_next);
    let events = state.consume(&cap_next, b"plain");
    assert_eq!(
        events.last(),
        Some(&RawInputEvent::CardInput(
            "q-next".to_string(),
            "plain".to_string()
        ))
    );
}

fn draft_capture() -> RawInputCapture {
    RawInputCapture::PromptDraft {
        id: "draft-1".to_string(),
        initial_text: "第一行".into(),
        completion: None,
        agent_composer: false,
        workspace_cwd: None,
    }
}

#[test]
fn draft_tab_accepts_the_runtime_completion() {
    let capture = RawInputCapture::PromptDraft {
        id: "draft-1".to_string(),
        initial_text: "review @sr".into(),
        completion: Some(Box::new(("@src/".into(), (0, 10)))),
        agent_composer: true,
        workspace_cwd: None,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    let events = state.consume(&capture, b"\t");
    let text = events.iter().find_map(|event| match event {
        RawInputEvent::PromptDraftChanged { text, .. } => Some(text.as_str()),
        _ => None,
    });
    assert_eq!(text, Some("review @src/"));
}

// Split legacy Alt+Enter (#1721): a trailing bare ESC is held (possible split legacy Alt+Enter);
// a following CR resolves to a soft newline instead of a cancel.
#[test]
fn draft_bare_esc_then_cr_inserts_newline_across_chunks() {
    let capture = draft_capture();
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    let (events, _) = state.consume_split(&capture, b"\x1b");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::PromptDraftCancel { .. })),
        "bare ESC must be held, not cancel: {events:?}"
    );
    assert!(state.draft_escape_pending(), "ESC must be pending");

    let (events, _) = state.consume_split(&capture, b"\r");
    let changed = events
        .iter()
        .find_map(|event| match event {
            RawInputEvent::PromptDraftChanged { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("split ESC+CR must insert a newline");
    assert_eq!(changed, "第一行\n");
    assert!(!state.draft_escape_pending());
}

// Split legacy Alt+Enter (#1721): on timeout the relay injects a second ESC; the held ESC plus
// the injected one resolve to the explicit cancel path.
#[test]
fn draft_bare_esc_timeout_injection_cancels() {
    let capture = draft_capture();
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    let (_, _) = state.consume_split(&capture, b"\x1b");
    assert!(state.draft_escape_pending());
    let (events, _) = state.consume_split(&capture, b"\x1b");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RawInputEvent::PromptDraftCancel { .. })),
        "injected second ESC must cancel: {events:?}"
    );
}

// Pasted data integrity (#1721): tabs inside a bracketed paste are data, not completion keys.
#[test]
fn draft_pasted_tab_is_inserted_as_data() {
    let capture = draft_capture();
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    let (events, _) = state.consume_split(&capture, b"\x1b[200~A\tB\x1b[201~");
    let changed = events
        .iter()
        .filter_map(|event| match event {
            RawInputEvent::PromptDraftChanged { text, .. } => Some(text.clone()),
            _ => None,
        })
        .next_back()
        .expect("paste must report a draft change");
    assert_eq!(changed, "第一行A\tB", "pasted tab must survive: {changed}");
}

// CSI-u Backspace (#2150) edits the draft like 0x7f: one character per
// sequence, numeric modifier variants included.
#[test]
fn draft_csi_u_backspace_deletes_one_char() {
    let capture = draft_capture();
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    let (events, _) = state.consume_split(&capture, b"\x1b[127u");
    let changed = events
        .iter()
        .find_map(|event| match event {
            RawInputEvent::PromptDraftChanged { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("CSI-u backspace must report a draft change");
    assert_eq!(changed, "第一");

    let (events, _) = state.consume_split(&capture, b"\x1b[127;2u");
    let changed = events
        .iter()
        .find_map(|event| match event {
            RawInputEvent::PromptDraftChanged { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("modifier variant must report a draft change");
    assert_eq!(changed, "第");
}

// CSI-u Backspace (#2150) pops free-text captures exactly like 0x7f.
#[test]
fn text_question_csi_u_backspace_pops_like_delete() {
    let capture = RawInputCapture::TextQuestion {
        id: "auth@field-0-1".to_string(),
        initial_text: "qwen3.7-max".to_string(),
        secret: false,
    };
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    assert_eq!(
        state.consume(&capture, b"\x1b[127u"),
        vec![RawInputEvent::CardInput(
            "auth@field-0-1".to_string(),
            "qwen3.7-ma".to_string(),
        )]
    );
    // An empty buffer stays a silent no-op, matching 0x7f.
    state.reset();
    state.apply_capture(&capture);
    let _ = state.consume(&capture, b"\x7f".repeat(16).as_slice());
    assert_eq!(state.consume(&capture, b"\x1b[127u"), vec![]);
}

// A CSI-u Backspace split across reads reassembles through the pending
// CSI holder before it edits the draft: no fragment leaks as text.
#[test]
fn draft_split_csi_u_backspace_reassembles_before_editing() {
    let capture = draft_capture();
    let mut state = CardInputState::default();
    state.apply_capture(&capture);

    let (events, _) = state.consume_split(&capture, b"\x1b[127;2");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::PromptDraftChanged { .. })),
        "held CSI prefix must not edit the draft yet: {events:?}"
    );

    let (events, _) = state.consume_split(&capture, b"u");
    let changed = events
        .iter()
        .find_map(|event| match event {
            RawInputEvent::PromptDraftChanged { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("joined CSI-u backspace must delete one char");
    assert_eq!(changed, "第一");
}
