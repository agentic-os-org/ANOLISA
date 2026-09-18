use crate::input::InterceptReason;

mod capture_bridge;
mod card_capture;
mod draft_editor;
mod event_parser;
mod event_sender;

pub(crate) use draft_editor::PromptDraftEditor;
mod generation;
mod mode;
mod path_prompt_candidate;
mod prompt_epoch;
mod pty;
mod relay;
mod relay_action;
mod soft_newline;
mod spawn;

pub(crate) use event_parser::redact_extension_setting_value;
pub(crate) use generation::UserPtyInputGeneration;
pub(crate) use mode::{update_input_mode, update_locked_input_mode, RawInputMode};
pub use mode::{PromptGhostCandidate, PromptGhostRoute, RawInputCapture, RawObserverAction};
pub(crate) use prompt_epoch::PromptEpochExchange;
pub(crate) use pty::{
    foreground_process_group_for_fds, process_group_exists, set_pty_winsize,
    signal_foreground_process_group, signal_process_group, signal_process_group_id, write_all_pty,
};
pub use relay_action::RawRelayAction;
pub(crate) use spawn::{
    spawn_raw_action_relay, spawn_raw_action_relay_with_wake, spawn_raw_input_relay,
    spawn_raw_input_relay_with_wake, RawInputShellRoute, ZshPathPromptBuffering,
};

pub(super) const CTRL_C: u8 = 0x03;
pub(super) const CTRL_U: u8 = 0x15;
pub(super) const ESC: u8 = 0x1b;

mod main_prompt_gate;
pub(crate) use main_prompt_gate::MainPromptGate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawInputEvent {
    ShellInputActivity {
        empty: bool,
    },
    /// User bytes the relay wrote to the PTY: the write generation plus how
    /// many recognized line submissions the write carried. Anchors prompt
    /// replay state so real user input expires stale replays.
    PtyUserWrite {
        generation: u64,
        line_submits: usize,
    },
    CtrlC,
    Esc,
    CandidateRedraw {
        input: Vec<u8>,
        hint: Option<String>,
    },
    CandidateCommit(Vec<u8>),
    PromptGhostClear,
    PromptGhostAccepted {
        suggestion_id: Option<String>,
    },
    PromptGhostCycle {
        text: String,
    },
    PromptGhostDismissed,
    /// Shift+Tab changed whether the Enhanced session may route input to AI.
    AssistanceToggled,
    PromptGhostIntercept {
        input: String,
        suggestion_id: Option<String>,
    },
    CandidateClearLine,
    UserIntercept(String, InterceptReason),
    UserInterceptWithRouting {
        input: String,
        reason: InterceptReason,
        cwd: String,
        sensitive: bool,
    },
    /// A missing-path prompt withheld from shell execution, so the runtime
    /// must acknowledge it on a stable UI surface.
    NativePathPromptIntercept {
        input: String,
        cwd: String,
        sensitive: bool,
    },
    /// Delivers a queued control only after all preceding shell submissions
    /// in the same input batch reach their primary prompts.
    UserInterceptAtPrompt {
        input: String,
        reason: InterceptReason,
        pending_submits: usize,
    },
    /// A whitelisted soft-newline shortcut was observed on a passthrough
    /// path (candidate buffer inactive). Observe-only: the bytes were still
    /// relayed to the shell unchanged; downstream may surface a one-time
    /// discoverability tip at the next prompt-ready (#1721).
    SoftNewlineShortcutObserved,
    /// A multi-line bracketed paste was relayed straight to bash (#1932):
    /// feeds the failure-insight multi-line entry hint, observe-only.
    MultilinePasteObserved,
    /// Input ended while the Shell-owned line could not be proven empty.
    /// The host must terminate the PTY session out-of-band; writing `exit`
    /// here could append to and execute the user's partial line.
    EofShutdownRequested,
    /// #1721 D13: the first soft newline in a candidate upgrades the draft
    /// into the multi-line prompt card; carries the buffered text.
    PromptDraftOpen {
        text: String,
    },
    /// Draft card state snapshot after an editing keystroke (D14).
    PromptDraftChanged {
        id: String,
        text: String,
        viewport: Box<draft_editor::DraftViewport>,
        line_count: usize,
        selected_completion: usize,
    },
    /// Enter inside the draft card: submit the multi-line prompt.
    PromptDraftSubmit {
        id: String,
        text: String,
        slash: bool,
        workspace_cwd: Option<String>,
    },
    /// Esc/Ctrl+C inside the draft card: cancel composition (D15).
    PromptDraftCancel {
        id: String,
    },
    /// The soft-newline upgrade submitted a synthetic empty line so bash
    /// repaints PS1 (#1932); its visually blank accept echo is dropped
    /// at the next prompt boundary instead of surfacing as a blank line.
    SyntheticPromptRepaint,
    CaptureSubmitted {
        kind: &'static str,
        target_id: String,
        generation: u64,
    },
    CaptureDrained {
        generation: u64,
    },
    CaptureExpired {
        generation: u64,
    },
    CaptureOverflow {
        generation: u64,
    },
    /// Quarantined submit-window bytes were discarded on an unsafe chain
    /// terminal state (follow-up card, invalidated chain, late arrival);
    /// the runtime renders a visible rejection notice (#1913).
    CaptureInputRejected {
        generation: u64,
        byte_len: usize,
    },
    CardFocus(String, usize),
    CardToggle(String, usize),
    CardInput(String, String),
    CardSecretInput(String, String),
    CardApprove(String),
    CardApproveTurn(String),
    CardAlwaysTrust(String),
    CardDeny(String),
    CardDetails(String),
    CardCancel(String),
    CardAnswer(String),
    QuestionSubmitAttempt(String),
    CardSecretAnswer(String),
    QuestionCancel(String),
    /// Ctrl+C on a question capture: abandon the whole prompt.
    ///
    /// Distinct from [`RawInputEvent::QuestionCancel`] (ESC) only so a multi-step prompt can let
    /// ESC step back while Ctrl+C keeps its usual meaning. Panels with a single step treat both
    /// identically.
    QuestionAbort(String),
    EvidenceSend(String),
    EvidenceIgnore(String),
    EvidenceCancel(String),
    ModeFocus(String, usize),
    ModeSet(String, usize),
    ModeCancel(String),
    ConfigFocus(String, usize),
    ConfigSave(String),
    ConfigCancel(String),
    ConfigLanguageFocus(String, usize),
    ConfigLanguageSet(String, usize),
    ConfigLanguageCancel(String),
    SessionFocus(String, usize),
    SessionToggle(String, usize),
    SessionResume(String, usize),
    SessionDelete(String),
    SessionClearConfirm(String),
    SessionCancel(String),
}

#[cfg(test)]
mod tests {
    use super::event_parser::{
        candidate_inline_hint, candidate_line_status, native_candidate_should_return_to_shell,
        redact_extension_setting_value, CandidateLineBuffer, CandidateLineStatus, NativeLineState,
    };
    use super::relay::ExplicitExitTracker;
    use super::soft_newline::render_soft_newline_markers;
    use crate::input::InputClassifier;

    fn soft_buffer() -> CandidateLineBuffer {
        let mut line = CandidateLineBuffer::default();
        line.soft_newline_enabled = true;
        line
    }

    fn status_of(line: &CandidateLineBuffer) -> CandidateLineStatus {
        candidate_line_status(&line.bytes, line.soft_newline_enabled)
    }

    // Split paste delimiters (#1721): PTY reads may split the paste delimiter itself; a half
    // closer must never become draft content nor leave in_paste stuck.
    #[test]
    fn split_paste_delimiters_resolve_across_pushes() {
        let mut line = soft_buffer();
        line.push(b"\x1b[200~");
        line.push("\u{5206}\u{6790}".as_bytes());
        line.push(b"\x1b[20");
        assert!(line.is_active(), "held partial keeps the candidate open");
        line.push(b"1~");
        assert!(!line.in_paste(), "joined closer must end the paste");
        let text = String::from_utf8_lossy(line.visible_line_bytes()).into_owned();
        assert!(
            !text.contains("20") && !text.contains("1~"),
            "no delimiter fragments in the draft: {text}"
        );

        // Split opener while the buffer is already active.
        let mut line = soft_buffer();
        line.push("\u{95ee}".as_bytes());
        line.push(b"\x1b[2");
        line.push(b"00~x\x1b[201~");
        assert!(!line.in_paste());
        assert_eq!(line.visible_line_bytes(), "\u{95ee}x".as_bytes());
    }

    // Cross-chunk pasted CRLF (#1721): a pasted CRLF split across reads folds into one newline.
    #[test]
    fn split_pasted_crlf_folds_into_one_newline() {
        let mut line = soft_buffer();
        line.push(b"\x1b[200~");
        line.push("\u{7b2c}\u{4e00}\u{884c}\r".as_bytes());
        line.push("\n\u{7b2c}\u{4e8c}\u{884c}".as_bytes());
        line.push(b"\x1b[201~");
        line.push(b"\r");
        let CandidateLineStatus::Complete { line: text, .. } = status_of(&line) else {
            panic!("draft must submit");
        };
        assert_eq!(text, "\u{7b2c}\u{4e00}\u{884c}\n\u{7b2c}\u{4e8c}\u{884c}");
    }

    // Matrix #3-#6: every whitelisted shortcut becomes one soft newline and
    // never completes or flushes the line; no suffix bytes leak (I3).
    #[test]
    fn soft_newline_shortcuts_insert_without_submitting() {
        for sequence in [
            b"\x1b\r".as_slice(),
            b"\x1b\n".as_slice(),
            b"\x1b[13;2u".as_slice(),
            b"\x1b[13;3u".as_slice(),
            b"\x1b[27;2;13~".as_slice(),
            b"\x1b[27;3;13~".as_slice(),
        ] {
            let mut line = soft_buffer();
            line.push("请帮我分析系统负载".as_bytes());
            line.push(sequence);
            line.push("给出优化建议".as_bytes());
            assert_eq!(
                status_of(&line),
                CandidateLineStatus::Pending,
                "sequence {sequence:?} must stay pending",
            );
            let display =
                String::from_utf8_lossy(&render_soft_newline_markers(line.visible_line_bytes()))
                    .into_owned();
            assert!(
                !display.contains(";2u") && !display.contains("13;"),
                "no literal leak in display for {sequence:?}: {display}"
            );

            // Matrix #1: Enter submits the whole multi-line draft.
            line.push(b"\r");
            let CandidateLineStatus::Complete {
                line: text,
                line_len,
            } = status_of(&line)
            else {
                panic!("enter must complete for {sequence:?}");
            };
            assert_eq!(text, "请帮我分析系统负载\n给出优化建议");
            assert_eq!(line_len, line.bytes.len());
        }
    }

    // Matrix #2: bare Ctrl+J keeps its submit semantics (D6).
    #[test]
    fn bare_ctrl_j_still_submits() {
        let mut line = soft_buffer();
        line.push("请帮我分析".as_bytes());
        line.push(b"\n");
        let CandidateLineStatus::Complete { line: text, .. } = status_of(&line) else {
            panic!("bare LF must complete");
        };
        assert_eq!(text, "请帮我分析");
    }

    // Matrix #7: bracketed-paste newlines become soft newlines; CRLF folds.
    #[test]
    fn pasted_newlines_stay_soft() {
        let mut line = soft_buffer();
        line.push(b"\x1b[200~");
        line.push("分析负载\r\n给出建议\r不要 sudo\n结束".as_bytes());
        line.push(b"\x1b[201~");
        assert_eq!(status_of(&line), CandidateLineStatus::Pending);
        line.push(b"\r");
        let CandidateLineStatus::Complete { line: text, .. } = status_of(&line) else {
            panic!("paste draft must complete on enter");
        };
        assert_eq!(text, "分析负载\n给出建议\n不要 sudo\n结束");
    }

    // Matrix #8/#9: non-whitelisted escapes and control bytes stay unsafe
    // even in a multi-line draft (I2).
    #[test]
    fn foreign_controls_stay_unsafe_with_soft_newlines() {
        let mut line = soft_buffer();
        line.push("分析".as_bytes());
        line.push(b"\x1b\r");
        line.push(b"\x1b[A");
        assert_eq!(status_of(&line), CandidateLineStatus::Unsafe);

        let mut line = soft_buffer();
        line.push("分析".as_bytes());
        line.push(b"\x1b\r");
        line.push(&[0x16]);
        assert_eq!(status_of(&line), CandidateLineStatus::Unsafe);
    }

    // Matrix #10: the 4096-byte cap still applies to multi-line drafts.
    #[test]
    fn oversized_multiline_draft_is_unsafe() {
        let mut line = soft_buffer();
        line.push(&[b'a'; 4000]);
        line.push(b"\x1b\r");
        line.push(&[b'b'; 200]);
        assert_eq!(status_of(&line), CandidateLineStatus::Unsafe);
    }

    // Matrix #11: a CSI-u shortcut split across chunks normalizes once whole.
    #[test]
    fn split_shortcut_normalizes_after_completion() {
        let mut line = soft_buffer();
        line.push("分析".as_bytes());
        line.push(b"\x1b[13;2");
        assert_eq!(status_of(&line), CandidateLineStatus::Pending);
        line.push(b"u");
        line.push("结束".as_bytes());
        line.push(b"\r");
        let CandidateLineStatus::Complete { line: text, .. } = status_of(&line) else {
            panic!("split shortcut must complete on enter");
        };
        assert_eq!(text, "分析\n结束");
    }

    // Matrix #12: backspace removes a soft newline as one visible character.
    #[test]
    fn backspace_deletes_soft_newline_whole() {
        let mut line = soft_buffer();
        line.push("分析".as_bytes());
        line.push(b"\x1b[13;2u");
        line.push(&[0x7f]);
        line.push(b"\r");
        let CandidateLineStatus::Complete { line: text, .. } = status_of(&line) else {
            panic!("draft must complete after backspace");
        };
        assert_eq!(text, "分析");
    }

    // Matrix #13: Ctrl+U clears the whole multi-line draft.
    #[test]
    fn ctrl_u_clears_multiline_draft() {
        let mut line = soft_buffer();
        line.push("分析".as_bytes());
        line.push(b"\x1b\r");
        line.push("建议".as_bytes());
        line.push(&[super::CTRL_U]);
        assert!(!line.is_active());
    }

    // Matrix #14/I1+I2: the native path keeps pre-#1721 semantics — shortcut
    // bytes in a native candidate stay Unsafe and flush to the shell.
    #[test]
    fn native_buffer_keeps_shortcut_bytes_unsafe() {
        let mut line = CandidateLineBuffer::default();
        line.push(b"/mo");
        line.push(b"\x1b[13;2u");
        assert_eq!(status_of(&line), CandidateLineStatus::Unsafe);
        assert!(
            line.bytes
                .windows(b"\x1b[13;2u".len())
                .any(|window| window == b"\x1b[13;2u"),
            "native buffer must keep the raw bytes untouched"
        );
    }

    // CSI-u Backspace (#2150) deletes like 0x7f on the native candidate
    // path: `/au` erases to an empty inactive buffer, and every deletion
    // keeps the line Pending instead of poisoning it Unsafe.
    #[test]
    fn csi_u_backspace_deletes_native_candidate_to_empty() {
        let mut line = CandidateLineBuffer::default();
        line.push(b"/au");
        for _ in 0..3 {
            line.push(b"\x1b[127u");
            assert_eq!(status_of(&line), CandidateLineStatus::Pending);
        }
        assert!(line.visible_line_bytes().is_empty());
        assert!(!line.is_active());
        // One more Backspace on the empty buffer stays a clean no-op.
        line.push(b"\x1b[127u");
        assert!(!line.is_active());
    }

    // CSI-u Backspace modifier variants (#2150): any single numeric
    // modifier deletes one character, matching readline's indifference.
    #[test]
    fn csi_u_backspace_modifier_variants_delete_one_char() {
        for sequence in [
            b"\x1b[127;2u".as_slice(),
            b"\x1b[127;3u".as_slice(),
            b"\x1b[127;5u".as_slice(),
            b"\x1b[127;13u".as_slice(),
        ] {
            let mut line = CandidateLineBuffer::default();
            line.push(b"/au");
            line.push(sequence);
            assert_eq!(
                line.visible_line_bytes(),
                b"/a",
                "variant {:?} must delete one char",
                String::from_utf8_lossy(sequence)
            );
        }
    }

    // CSI-u Backspace removes a full UTF-8 character, not a single byte.
    #[test]
    fn csi_u_backspace_deletes_whole_utf8_char() {
        let mut line = soft_buffer();
        line.push("??分析".as_bytes());
        line.push(b"\x1b[127u");
        assert_eq!(line.visible_line_bytes(), "??分".as_bytes());
    }

    // CSI-u Backspace removes a soft newline as one visible character,
    // matching the 0x7f semantics of matrix #12.
    #[test]
    fn csi_u_backspace_deletes_soft_newline_whole() {
        let mut line = soft_buffer();
        line.push("分析".as_bytes());
        line.push(b"\x1b[13;2u");
        line.push(b"\x1b[127u");
        line.push(b"\r");
        let CandidateLineStatus::Complete { line: text, .. } = status_of(&line) else {
            panic!("draft must complete after CSI-u backspace");
        };
        assert_eq!(text, "分析");
    }

    // Malformed or unrelated CSI-u forms stay fail-closed: the bytes are
    // buffered untouched and the line flushes Unsafe, byte-identical to
    // the pre-fix route.
    #[test]
    fn malformed_csi_u_backspace_forms_stay_unsafe() {
        for sequence in [
            b"\x1b[127;u".as_slice(),
            b"\x1b[127;2;3u".as_slice(),
            b"\x1b[127:1u".as_slice(),
            b"\x1b[1270u".as_slice(),
            b"\x1b[12u".as_slice(),
            b"\x1b[127~".as_slice(),
        ] {
            let mut line = CandidateLineBuffer::default();
            line.push(b"/au");
            line.push(sequence);
            assert_eq!(
                status_of(&line),
                CandidateLineStatus::Unsafe,
                "form {:?} must stay Unsafe",
                String::from_utf8_lossy(sequence)
            );
            assert_eq!(
                &line.bytes[..3],
                b"/au",
                "draft prefix must stay intact for the flush"
            );
        }
    }

    // A CSI-u Backspace split across PTY reads is not reassembled: the
    // fragments stay buffered and the completed sequence flushes Unsafe
    // (fail-closed; terminals write key sequences atomically).
    #[test]
    fn split_csi_u_backspace_stays_fail_closed() {
        let mut line = CandidateLineBuffer::default();
        line.push(b"/au");
        line.push(b"\x1b[12");
        assert_eq!(status_of(&line), CandidateLineStatus::Pending);
        line.push(b"7u");
        assert_eq!(status_of(&line), CandidateLineStatus::Unsafe);
    }

    // Matrix #19 precondition: a draft of only soft newlines normalizes to
    // whitespace-only text (relay consumes it without submitting).
    #[test]
    fn soft_newline_only_draft_normalizes_to_whitespace() {
        let mut line = soft_buffer();
        line.push(b"\x1b\r");
        line.push(b"\x1b[13;2u");
        line.push(b"\r");
        let CandidateLineStatus::Complete { line: text, .. } = status_of(&line) else {
            panic!("whitespace draft must complete");
        };
        assert!(text.trim().is_empty());
        assert_eq!(text, "\n\n");
    }

    #[test]
    fn bare_slash_has_no_inline_hint() {
        assert_eq!(candidate_inline_hint("/"), None);
        assert_eq!(candidate_inline_hint("  /"), None);
        assert_eq!(
            candidate_inline_hint("/mo"),
            Some("/mode approval [recommend|auto|trust]".to_string())
        );
        assert_eq!(candidate_inline_hint("/approval"), None);
        assert_eq!(
            candidate_inline_hint("/sk"),
            Some("/skills [list|detail] [name]".to_string())
        );
    }

    #[test]
    fn ambiguous_prefix_hint_lists_all_matching_candidates() {
        assert_eq!(
            candidate_inline_hint("/sta"),
            Some("/status · /stats".to_string())
        );
        assert_eq!(
            candidate_inline_hint("/st"),
            Some("/status · /stats".to_string())
        );
        assert_eq!(
            candidate_inline_hint("/s"),
            Some("/status · /stats · /session · /skills".to_string())
        );
        assert_eq!(
            candidate_inline_hint("/h"),
            Some("/health · /hooks".to_string())
        );
        // `/mode` has two specs but deduplicates to one candidate name.
        assert_eq!(
            candidate_inline_hint("/m"),
            Some("/mode · /mcp".to_string())
        );
    }

    #[test]
    fn exact_command_token_excludes_itself_from_hint() {
        // `/stats` is complete: no other visible command shares the prefix.
        assert_eq!(candidate_inline_hint("/stats"), None);
        // `/status` is complete and no sibling extends it either.
        assert_eq!(candidate_inline_hint("/status"), None);
    }

    #[test]
    fn extension_setting_values_are_redacted_from_candidate_echo() {
        let command = b"/extensions settings set fixture token secret-value --scope user";
        let redacted = redact_extension_setting_value(command);
        let shown = String::from_utf8(redacted).expect("redacted command remains UTF-8");
        assert_eq!(
            shown,
            "/extensions settings set fixture token ************ ******* ****"
        );
        assert!(!shown.contains("secret-value"));
    }

    #[test]
    fn extension_setting_value_is_redacted_from_first_typed_byte() {
        let prefix = b"/extensions settings set fixture token ";
        assert_eq!(redact_extension_setting_value(prefix), prefix);
        assert_eq!(
            redact_extension_setting_value(b"/extensions settings set fixture token s"),
            b"/extensions settings set fixture token *"
        );
    }

    #[test]
    fn other_slash_values_are_not_redacted() {
        let command = b"/extensions settings get fixture token";
        assert_eq!(redact_extension_setting_value(command), command);
    }

    #[test]
    fn native_slash_candidate_returns_paths_and_tab_to_shell() {
        let classifier = InputClassifier::default();
        let mut line = CandidateLineBuffer::default();

        line.push(b"/m");
        assert!(!native_candidate_should_return_to_shell(&classifier, &line));

        line.push(b"ode agent");
        assert!(!native_candidate_should_return_to_shell(&classifier, &line));

        line.clear();
        line.push(b"/Users");
        assert!(native_candidate_should_return_to_shell(&classifier, &line));

        line.clear();
        line.push(b"/tmp/");
        assert!(native_candidate_should_return_to_shell(&classifier, &line));

        line.clear();
        line.push(b"/\t");
        assert!(native_candidate_should_return_to_shell(&classifier, &line));
    }

    #[test]
    fn candidate_line_ctrl_u_clears_pending_input() {
        let mut line = CandidateLineBuffer::default();

        line.push(b"Analyze memory pressure");
        line.push(&[super::CTRL_U]);

        assert!(!line.is_active());
        assert!(line.visible_line_bytes().is_empty());
    }

    #[test]
    fn explicit_exit_tracker_detects_split_exit_zero() {
        let mut tracker = ExplicitExitTracker::default();

        tracker.observe_shell_bytes(b"ex");
        assert!(!tracker.saw_explicit_exit());
        tracker.observe_shell_bytes(b"it 0\n");

        assert!(tracker.saw_explicit_exit());
    }

    #[test]
    fn explicit_exit_tracker_ignores_non_exit_lines() {
        let mut tracker = ExplicitExitTracker::default();

        tracker.observe_shell_bytes(b"echo exit\n");
        tracker.observe_shell_bytes(b"printf logout\n");

        assert!(!tracker.saw_explicit_exit());
    }
}
