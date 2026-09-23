//! Tracks prompt-bound state derived from decisive shell events.

use crate::runtime::prelude::{ShellEvent, ShellEventKind};
use crate::runtime::state::InlineState;

pub(super) fn update_shell_prompt_state(events: &[ShellEvent], state: &mut InlineState) {
    // A `ShellReady` cwd is current only until the next PTY write. Submit detection
    // in the byte stream is heuristic, and a submitted `cd` may lose its markers.
    for event in events.iter().rev() {
        if is_pty_input(event) {
            state.shell_prompt_cwd = None;
            break;
        }
        if event.kind == ShellEventKind::ShellReady {
            if let Some(cwd) = event.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
                state.shell_prompt_cwd = Some(cwd.to_string());
                break;
            }
        }
    }

    // Match submitted lines one-to-one with prompt boundaries. A completion
    // cannot establish idleness while a typeahead line still awaits its own
    // precmd, even when the two markers arrive in separate dispatcher batches.
    for event in events {
        if let Some(count) = prompt_submit_count(event) {
            state.pending_shell_submits = state.pending_shell_submits.saturating_add(count);
            state.shell_at_prompt = false;
            continue;
        }
        if let Some(at_prompt) = reconciled_prompt_state(event) {
            state.pending_shell_submits = 0;
            state.shell_at_prompt = at_prompt;
            continue;
        }
        if is_prompt_input(event) {
            state.shell_at_prompt = false;
            continue;
        }
        match event.kind {
            ShellEventKind::ShellReady
            | ShellEventKind::CommandCompleted
            | ShellEventKind::CommandFailed => {
                state.pending_shell_submits = state.pending_shell_submits.saturating_sub(1);
                state.shell_at_prompt = state.pending_shell_submits == 0;
            }
            ShellEventKind::CommandStarted => state.shell_at_prompt = false,
            _ => {}
        }
    }
}

fn is_pty_input(event: &ShellEvent) -> bool {
    is_input_barrier(event, "shell_pty_input")
}

fn is_prompt_input(event: &ShellEvent) -> bool {
    is_input_barrier(event, "shell_prompt_input")
}

fn prompt_submit_count(event: &ShellEvent) -> Option<usize> {
    (event.kind == ShellEventKind::UserInputIntercepted
        && event.component.as_deref() == Some("shell_prompt_submit"))
    .then(|| event.message.as_deref()?.parse().ok())
    .flatten()
}

fn reconciled_prompt_state(event: &ShellEvent) -> Option<bool> {
    if event.kind != ShellEventKind::UserInputIntercepted
        || event.component.as_deref() != Some("shell_prompt_ready")
    {
        return None;
    }
    match event.message.as_deref() {
        Some("painted") => Some(true),
        Some("occupied") => Some(false),
        _ => None,
    }
}

fn is_input_barrier(event: &ShellEvent, component: &str) -> bool {
    event.kind == ShellEventKind::UserInputIntercepted
        && event.component.as_deref() == Some(component)
        && event.message.as_deref() == Some("write")
}
