//! Finite, in-process auth management for the wizard.
//!
//! Runs the existing auth state machine without a shell host: no `run_tui`,
//! no bootstrap, no PTY, and no working shell prompt. The task returns typed
//! facts to the wizard when the user finishes, cancels, or it fails.

use std::io::{self, Write};

use nix::libc;
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use crate::adapter::AdapterInstance;
use crate::config::Language;
use crate::raw_input::{AuthCardInput, RawInputEvent};
use crate::runtime::guide::input::{Batch, GuideInput};
use crate::runtime::guide::result::{EndReason, TaskOutcome};
use crate::runtime::prelude::{NoticePanelModel, RatatuiInlineRenderer, ShellEvent};
use crate::runtime::state::InlineState;
use crate::types::ShellEventKind;

use super::card_actions::render_auth_card_actions;
use super::host::AuthHost;
use super::runtime::{cancel_auth_panel, has_auth_operation, trigger_auth_from_slash};

pub(crate) struct TaskExit {
    pub(crate) outcome: TaskOutcome,
    /// The terminal input source closed; the wizard must not reopen.
    pub(crate) terminal_lost: bool,
    /// A termination signal arrived; the whole wizard exits after cleanup.
    pub(crate) terminate: Option<i32>,
}

struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

pub(crate) fn run(
    input: &mut GuideInput,
    adapter: &AdapterInstance,
    language: Language,
) -> TaskExit {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let mut output = crate::runtime::terminal::CrLfWriter::new(&mut handle);
    let guard = match RawModeGuard::enter() {
        Ok(guard) => guard,
        Err(error) => return failed(format!("raw mode: {error}")),
    };
    // The wizard just left its alternate screen. Start the inline card on a
    // clean screen so it never overlaps the page the user was reading.
    let _ = output.write_all(b"\x1b[H\x1b[2J");
    let _ = output.flush();
    let exit = run_loop(input, adapter, language, &mut output);
    drop(guard);
    let _ = output.flush();
    exit
}

fn failed(message: String) -> TaskExit {
    let mut outcome = TaskOutcome::default();
    outcome.reason = EndReason::Failed;
    outcome.error = Some(message);
    TaskExit {
        outcome,
        terminal_lost: false,
        terminate: None,
    }
}

fn run_loop<W: Write>(
    input: &mut GuideInput,
    adapter: &AdapterInstance,
    language: Language,
    output: &mut W,
) -> TaskExit {
    let mut state = InlineState {
        language,
        ..Default::default()
    };
    state.auth.host = AuthHost::Guide;
    let mut card = AuthCardInput::default();
    let mut event_index = 0usize;
    let mut terminal_lost = false;
    let mut terminate = None;

    if let Err(error) = trigger_auth_from_slash(adapter, &mut state, output) {
        return failed(format!("auth open: {error}"));
    }

    loop {
        if let Some(signal) = crate::runtime::terminal::take_guide_signal() {
            if signal == libc::SIGINT {
                state.auth.outcome.reason = EndReason::Cancelled;
                state.auth.exit_requested = true;
            } else {
                state.auth.outcome.reason = EndReason::Interrupted;
                terminate = Some(signal);
                state.auth.exit_requested = true;
            }
        }
        if state.auth.exit_requested {
            break;
        }
        if state.auth.menu_reload {
            if has_auth_operation(&state) {
                // A lingering ECS flow makes the trigger a no-op; keep the
                // request pending until the poll below drains it, otherwise the
                // task would sit with no capture and drop every key.
                if let Err(error) = crate::auth::ecs_poll::poll(adapter, &mut state, output)
                    .and_then(|()| crate::auth::trial::poll(adapter, &mut state, output))
                {
                    state.auth.outcome.reason = EndReason::Failed;
                    state.auth.outcome.error = Some(format!("auth task: {error}"));
                    break;
                }
                if has_auth_operation(&state) {
                    match input.read_raw() {
                        Batch::Lost => {
                            terminal_lost = true;
                            state.auth.outcome.reason = EndReason::Interrupted;
                            break;
                        }
                        _ => continue,
                    }
                }
            }
            state.auth.menu_reload = false;
            state.auth.state = None;
            state.auth.handled_card_events.clear();
            card.reset();
            if let Err(error) = trigger_auth_from_slash(adapter, &mut state, output) {
                state.auth.outcome.reason = EndReason::Failed;
                state.auth.outcome.error = Some(format!("auth reload: {error}"));
                break;
            }
            if state.auth.state.is_none() && !state.auth.exit_requested {
                state.auth.outcome.reason = EndReason::Failed;
                state.auth.outcome.error =
                    Some("auth management could not be reopened".to_string());
                break;
            }
            if state.auth.exit_requested {
                break;
            }
        }
        if let Err(error) = crate::auth::ecs_poll::poll(adapter, &mut state, output)
            .and_then(|()| crate::auth::trial::poll(adapter, &mut state, output))
        {
            state.auth.outcome.reason = EndReason::Failed;
            state.auth.outcome.error = Some(format!("auth task: {error}"));
            break;
        }
        if state.auth.exit_requested {
            break;
        }
        match input.read_raw() {
            Batch::Lost => {
                terminal_lost = true;
                state.auth.outcome.reason = EndReason::Interrupted;
                break;
            }
            Batch::Idle => {}
            Batch::Bytes(bytes) => {
                let Some(capture) = crate::auth::runtime::pending_auth_capture(&state) else {
                    // No card is armed (menu preparing, for example). Ctrl-C and
                    // Ctrl-D are bytes in raw mode, so they must still work.
                    if bytes.contains(&0x03) || bytes.contains(&0x04) {
                        state.auth.outcome.reason = EndReason::Cancelled;
                        state.auth.exit_requested = true;
                    }
                    continue;
                };
                let events = card.consume(&capture, &bytes);
                let shell_events = auth_events(&events, &mut event_index);
                if shell_events.is_empty() {
                    continue;
                }
                let base = event_index;
                if let Err(error) =
                    render_auth_card_actions(&shell_events, adapter, &mut state, output, base)
                {
                    state.auth.outcome.reason = EndReason::Failed;
                    state.auth.outcome.error = Some(format!("auth action: {error}"));
                    break;
                }
            }
        }
    }

    converge(&mut state, output);
    TaskExit {
        outcome: std::mem::take(&mut state.auth.outcome),
        terminal_lost,
        terminate,
    }
}

/// Cancels what can still be cancelled and reaps what this task owns before
/// the wizard takes the terminal back.
fn converge<W: Write>(state: &mut InlineState, output: &mut W) {
    // An operation still in flight may already have committed. Record that
    // uncertainty before cancelling so the task never reports "nothing
    // changed" for a write whose answer it stopped waiting for.
    let in_flight = crate::auth::ecs_poll::operation_in_flight(state)
        || state.auth.ecs_trial.preference_job.is_some();
    if in_flight {
        use crate::runtime::guide::result::{ActionKind, ChangeFact, Verification};
        let kind = match state.auth.state.as_ref().map(|auth| &auth.phase) {
            Some(super::runtime::AuthPhase::ConfirmDelete { .. }) => ActionKind::Delete,
            Some(super::runtime::AuthPhase::ProviderAction { .. }) => ActionKind::Activate,
            Some(super::runtime::AuthPhase::FillingField) => ActionKind::Configure,
            _ => ActionKind::TrialPreference,
        };
        state
            .auth
            .record_fact(kind, ChangeFact::Unknown, Verification::Unconfirmed);
    }
    let reason = state.auth.outcome.reason;
    let error = state.auth.outcome.error.clone();
    // Only an abandoned task is cancelled. A user who chose "finish and return"
    // keeps their Done reason, and a failure keeps its error text.
    if in_flight || (reason == EndReason::Done && !state.auth.exit_requested) {
        let _ = cancel_auth_panel(state, output);
    }
    state.auth.outcome.reason = reason;
    state.auth.outcome.error = error;
    crate::auth::ecs_poll::shutdown(state);
    state.auth.ecs_trial.settle_preference_worker();
    state.auth.state = None;
    state.auth.exit_requested = false;
    state.auth.menu_reload = false;
}

fn auth_events(events: &[RawInputEvent], counter: &mut usize) -> Vec<ShellEvent> {
    events
        .iter()
        .filter_map(|event| {
            let (component, message, input) = match event {
                RawInputEvent::CardFocus(id, selected) => {
                    ("card", "focus", format!("{id}:{selected}"))
                }
                RawInputEvent::CardInput(id, text) => ("card", "input", format!("{id}:{text}")),
                RawInputEvent::CardSecretInput(id, text) => {
                    ("card_secret", "input", format!("{id}:{text}"))
                }
                RawInputEvent::CardAnswer(answer) => ("card", "answer", answer.clone()),
                RawInputEvent::CardSecretAnswer(answer) => {
                    ("card_secret", "answer", answer.clone())
                }
                RawInputEvent::QuestionSubmitAttempt(id) => {
                    ("card", "question_submit_empty", id.clone())
                }
                RawInputEvent::QuestionCancel(id) => ("card", "question_cancel", id.clone()),
                RawInputEvent::QuestionAbort(id) => ("card", "question_abort", id.clone()),
                _ => return None,
            };
            *counter += 1;
            let mut shell_event = ShellEvent::user_input_intercepted("guide-auth", &input);
            shell_event.kind = ShellEventKind::UserInputIntercepted;
            shell_event.component = Some(component.to_string());
            shell_event.message = Some(message.to_string());
            shell_event.started_at_ms = Some(*counter as u64);
            Some(shell_event)
        })
        .collect()
}

/// Refuses to open management when the core cannot write configuration safely.
pub(crate) fn refuse_unsafe_core<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> io::Result<()> {
    let i18n = state.i18n();
    let message = i18n.t(crate::i18n::MessageId::GuideWizardAiCoreCapability);
    RatatuiInlineRenderer::for_terminal()
        .with_language(state.language)
        .write_notice_panel(
            output,
            NoticePanelModel {
                title: "Auth unavailable",
                body: vec![message.to_string()],
                footer: None,
            },
        )?;
    state.auth.outcome.reason = EndReason::Failed;
    state.auth.outcome.error = Some(message.to_string());
    state.auth.exit_requested = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_auth_card_actions_are_forwarded() {
        let mut counter = 0;
        let events = vec![
            RawInputEvent::CardFocus("id".into(), 2),
            RawInputEvent::QuestionAbort("id".into()),
            RawInputEvent::CtrlC,
            RawInputEvent::CandidateCommit(b"ls".to_vec()),
        ];
        let mapped = auth_events(&events, &mut counter);
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].message.as_deref(), Some("focus"));
        assert_eq!(mapped[0].input.as_deref(), Some("id:2"));
        assert_eq!(mapped[1].message.as_deref(), Some("question_abort"));
        assert!(mapped
            .iter()
            .all(|event| event.kind == ShellEventKind::UserInputIntercepted));
    }

    #[test]
    fn event_keys_stay_unique_across_batches() {
        let mut counter = 0;
        let first = auth_events(&[RawInputEvent::CardAnswer("a".into())], &mut counter);
        let second = auth_events(&[RawInputEvent::CardAnswer("a".into())], &mut counter);
        assert_ne!(first[0].started_at_ms, second[0].started_at_ms);
    }

    #[test]
    fn secret_answers_keep_the_secret_component() {
        let mut counter = 0;
        let mapped = auth_events(
            &[RawInputEvent::CardSecretAnswer("token".into())],
            &mut counter,
        );
        assert_eq!(mapped[0].component.as_deref(), Some("card_secret"));
        assert_eq!(mapped[0].message.as_deref(), Some("answer"));
    }
}
