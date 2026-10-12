//! Emits relay-facing input events and maintains prompt evidence barriers.

use super::OscParser;

impl OscParser {
    pub(in crate::shell_host) fn push_control_event(&mut self, input: &str) {
        self.push_self_session_input_event(
            "control",
            "control input observed while relaying to bash",
            Some(input),
        );
    }

    /// Observe-only soft-newline shortcut signal on a passthrough path
    /// (#1721 T-c): the bytes were relayed to bash unchanged; the runtime
    /// may surface a one-time discoverability tip at the next prompt-ready.
    pub(in crate::shell_host) fn push_soft_newline_shortcut_event(&mut self) {
        self.push_self_session_input_event(
            "soft_newline_shortcut",
            "soft-newline shortcut observed while relaying to bash",
            None,
        );
    }

    /// #1932 F5: a multi-line bracketed paste was relayed straight to bash;
    /// the runtime may attach a multi-line entry hint to a failure insight.
    pub(in crate::shell_host) fn push_multiline_paste_event(&mut self) {
        self.push_self_session_input_event(
            "multiline_paste",
            "multi-line bracketed paste relayed to bash",
            None,
        );
    }

    /// #1721 D13: forwards prompt-draft card lifecycle events (open/changed/
    /// submit/cancel) to the runtime as structured JSON payloads.
    pub(in crate::shell_host) fn push_prompt_draft_event(
        &mut self,
        action: &str,
        payload: Option<&str>,
    ) {
        self.push_self_session_input_event("prompt_draft", action, payload);
    }

    pub(in crate::shell_host) fn push_shell_input_activity_event(&mut self, empty: bool) {
        self.push_self_session_input_event(
            "shell_input",
            if empty {
                "input empty"
            } else {
                "input editing"
            },
            None,
        );
    }

    /// User bytes were written to the shell's PTY: whatever the shell
    /// does with them (a custom `accept-line` binding cannot be told
    /// apart from editing keys in the byte stream), prompt occupancy
    /// becomes active and a previously reported prompt cwd stops being
    /// provably current. Decisive shell boundaries restore cwd evidence;
    /// prompt occupancy additionally requires proof of an idle prompt.
    pub(in crate::shell_host) fn push_shell_pty_input_events(&mut self) {
        if !self.pty_input_barrier_pushed {
            self.pty_input_barrier_pushed = true;
            self.push_self_session_input_event("shell_pty_input", "write", None);
        }
        self.push_shell_prompt_input_barrier();
    }

    /// Reports every accept-line candidate without collapsing it into the
    /// prompt-input barrier, so runtime can match queued typeahead one-to-one.
    pub(in crate::shell_host) fn push_shell_prompt_submits(&mut self, count: usize) {
        if count > 0 {
            self.push_self_session_input_event("shell_prompt_submit", &count.to_string(), None);
        }
    }

    /// Reconciles a submit consumed by a foreground program while preserving
    /// whether newer input already occupies the painted prompt.
    pub(in crate::shell_host) fn push_reconciled_shell_prompt_state(&mut self, at_prompt: bool) {
        if at_prompt {
            self.prompt_input_barrier_pushed = false;
        }
        let state = if at_prompt { "painted" } else { "occupied" };
        self.push_self_session_input_event("shell_prompt_ready", state, None);
    }

    pub(in crate::shell_host) fn push_shell_prompt_input_barrier(&mut self) {
        if !self.prompt_input_barrier_pushed {
            self.prompt_input_barrier_pushed = true;
            self.push_self_session_input_event("shell_prompt_input", "write", None);
        }
    }
}
