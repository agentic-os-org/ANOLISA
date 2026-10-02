use crate::input::InputClassifier;

use super::super::event_parser::NativeLineState;
use super::super::generation::LineSubmitCounter;

const BASH_SLASH_SUBMISSION_GUARD: &[u8] = b"\x1b[99~";
const BASH_PRIVATE_SLASH_SUBMISSION_GUARD: &[u8] = b"\x1b[100~";
const BASH_RECOVERABLE_HISTORY_SUBMISSION_GUARD: &[u8] = b"\x1b[101~";

pub(super) struct PrivateHistorySubmission {
    pub(super) bytes: Vec<u8>,
    pub(super) recoverable: bool,
}

pub(super) fn history_private_submission(
    bash_readline_history_privacy: bool,
    state: &NativeLineState,
    line_submits: &LineSubmitCounter,
    at_prompt: bool,
    bytes: &[u8],
) -> Option<PrivateHistorySubmission> {
    if !bash_readline_history_privacy || !at_prompt {
        return None;
    }
    let submit = line_submits.first_submission(bytes)?;
    let (private, recoverable) = match state.clean_visible_line() {
        Some(prior) => {
            let mut command = Vec::with_capacity(prior.len() + submit);
            command.extend_from_slice(prior);
            command.extend_from_slice(&bytes[..submit]);
            let command = std::str::from_utf8(&command).ok()?;
            (crate::evidence::redact_sensitive_text(command).1, false)
        }
        // Readline cursor movement, completion, or a multiline paste makes
        // the mirror unable to prove the final accepted line is non-secret.
        // Exclude that submission unless the shell-side widget later proves
        // the accepted line is safe and provider-bound.
        None => (
            state.history_mirror_requires_fail_closed(),
            state.history_mirror_requires_fail_closed(),
        ),
    };
    if !private {
        return None;
    }

    // Enhanced Bash enables `HISTCONTROL=ignorespace`. Insert a leading
    // blank through Readline immediately before accepting the line.
    let mut private = Vec::with_capacity(bytes.len() + 3);
    private.extend_from_slice(&bytes[..submit]);
    private.extend_from_slice(b"\x01 \x05");
    private.extend_from_slice(&bytes[submit..]);
    Some(PrivateHistorySubmission {
        bytes: private,
        recoverable,
    })
}

pub(super) fn bash_submission_has_leading_whitespace(
    enabled: bool,
    at_prompt: bool,
    state: &NativeLineState,
    line_submits: &LineSubmitCounter,
    bytes: &[u8],
) -> bool {
    if !enabled || !at_prompt {
        return false;
    }
    let Some(submit) = line_submits.first_submission(bytes) else {
        return false;
    };
    let mut submitted = state.clone();
    submitted.observe_shell_bytes(&bytes[..submit]);
    submitted.clean_visible_line().is_some_and(|line| {
        line.first()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
            && line.iter().any(|byte| !byte.is_ascii_whitespace())
    })
}

pub(super) fn guarded_bash_submission(
    required: bool,
    history_private: bool,
    history_recoverable: bool,
    input_classifier: &InputClassifier,
    line_submits: &LineSubmitCounter,
    bytes: &[u8],
) -> Option<Vec<u8>> {
    let submissions = line_submits.submission_positions(bytes);
    let mut guards = Vec::new();
    let mut line_start = 0;
    let mut readline_owned = false;
    let mut batch_readline_safe = required;
    for (index, submit) in submissions.into_iter().enumerate() {
        let line = &bytes[line_start..submit];
        let exact_slash = exact_slash_submission(input_classifier, line);
        let blank = line.iter().all(u8::is_ascii_whitespace);
        let ctrl_o = bytes[submit] == 0x0f;
        let needs_guard = if !batch_readline_safe {
            false
        } else if index == 0 {
            required
        } else if readline_owned {
            // Ctrl-O loads another history entry before the next submitted
            // bytes, so the relay cannot prove the resulting Readline line.
            true
        } else {
            exact_slash
        };
        if needs_guard {
            let guard = if index == 0 && history_recoverable {
                BASH_RECOVERABLE_HISTORY_SUBMISSION_GUARD
            } else if index == 0 && history_private {
                BASH_PRIVATE_SLASH_SUBMISSION_GUARD
            } else {
                BASH_SLASH_SUBMISSION_GUARD
            };
            guards.push((submit, guard));
        }
        if index == 0 {
            batch_readline_safe &= exact_slash || ctrl_o;
        } else if (readline_owned && !ctrl_o) || (!exact_slash && !blank && !ctrl_o) {
            // An ordinary command ends the part of the batch whose future
            // consumer is provably Readline. Later bytes stay byte-exact for
            // whichever shell/foreground reader owns the PTY next.
            batch_readline_safe = false;
        }
        readline_owned = batch_readline_safe && ctrl_o;
        line_start = submit + 1;
    }
    if guards.is_empty() {
        return None;
    }

    // Keep each guard adjacent to its submission boundary while preserving
    // #2918's single-write batch ownership. Do not assume the PTY delivers
    // this buffer atomically: write_all_pty may short-write. In vi-insert, ESC
    // is itself a complete binding, so a delivery gap longer than the user's
    // keyseq-timeout can reinterpret the remaining bytes as vi commands and
    // silently mutate the input line.
    let extra = guards.iter().map(|(_, guard)| guard.len()).sum::<usize>();
    let mut guarded = Vec::with_capacity(bytes.len() + extra);
    let mut cursor = 0;
    for (submit, guard) in guards {
        guarded.extend_from_slice(&bytes[cursor..submit]);
        guarded.extend_from_slice(guard);
        cursor = submit;
    }
    guarded.extend_from_slice(&bytes[cursor..]);
    Some(guarded)
}

fn exact_slash_submission(input_classifier: &InputClassifier, bytes: &[u8]) -> bool {
    let Ok(line) = std::str::from_utf8(bytes) else {
        return false;
    };
    let first_word = line.split_whitespace().next().unwrap_or_default();
    input_classifier.is_exact_slash_control_command(first_word)
}

pub(super) fn bash_submission_needs_guard(
    enabled: bool,
    at_prompt: bool,
    input_classifier: &InputClassifier,
    state: &NativeLineState,
    line_submits: &LineSubmitCounter,
    bytes: &[u8],
) -> bool {
    if !enabled || !at_prompt {
        return false;
    }
    let Some(submit) = line_submits.first_submission(bytes) else {
        return false;
    };

    // Readline owns history recall, completion, and cursor edits, so only its
    // submission widget can inspect a buffer once the relay mirror is dirty.
    // Keep clean ordinary lines and empty Enters off the widget path to avoid
    // a visible Readline redisplay before accept-line. Dirty ordinary lines
    // may still redisplay once because invoking bind-x is observable; that is
    // the cost of distinguishing them from recalled slash controls without a
    // global pre-execution hook.
    let mut submitted = state.clone();
    submitted.observe_shell_bytes(&bytes[..submit]);
    let Some(line) = submitted.clean_visible_line() else {
        return true;
    };
    let Ok(line) = std::str::from_utf8(line) else {
        return true;
    };
    let first_word = line.split_whitespace().next().unwrap_or_default();
    input_classifier.is_exact_slash_control_command(first_word)
}
