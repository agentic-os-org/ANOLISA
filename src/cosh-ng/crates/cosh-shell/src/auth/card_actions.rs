//! Card-action routing for the auth flow, split out of the runtime owner so
//! the state machine file stays within the crate's size budget.

use crate::adapter::AdapterInstance;
use crate::runtime::dispatcher::stable_event_key;
use crate::runtime::prelude::{ShellEvent, ShellEventKind};
use crate::runtime::state::InlineState;

use super::runtime::{
    cancel_auth_panel, event_targets_pending_auth, handle_auth_answer, handle_auth_back,
    handle_auth_focus, handle_auth_input, has_pending_auth,
};

pub(crate) fn render_auth_card_actions<W: std::io::Write>(
    events: &[ShellEvent],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    if !has_pending_auth(state) {
        return Ok(());
    }
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        if event.kind != ShellEventKind::UserInputIntercepted {
            continue;
        }
        if !is_auth_card_component(event.component.as_deref()) {
            continue;
        }
        let dedup_key = stable_event_key("auth-card", event_index, event);
        if !state.auth.handled_card_events.insert(dedup_key) {
            continue;
        }
        match event.message.as_deref() {
            Some("focus") => {
                if let Some((id, selected)) = parse_card_id_usize(event) {
                    handle_auth_focus(state, &id, selected, output)?;
                }
            }
            Some("input") => {
                if let Some((id, text)) = parse_card_id_text(event) {
                    handle_auth_input(state, &id, &text, output)?;
                }
            }
            Some("answer") => {
                if let Some(answer) = event.input.as_deref() {
                    let auth_id = state.auth.state.as_ref().map(|a| a.id.clone());
                    if let Some(id) = auth_id {
                        handle_auth_answer(adapter, state, &id, answer, output)?;
                        let key = stable_event_key("question-answer", event_index, event);
                        state.questions.handled_answers.insert(key);
                    }
                }
            }
            // An empty Enter on a non-secret field arrives as `question_submit_empty`, not
            // `answer` (secret fields send an empty `answer` instead). Without this arm the
            // keystroke is dropped, so "Enter to keep current value" never advances an edit.
            // The event carries the scoped capture id, and routing it through the same
            // matches_auth_capture check is what keeps a stale `field-N` submission from
            // advancing whichever field is live now.
            Some("question_submit_empty") => {
                if let Some(capture_id) = event.input.as_deref() {
                    if handle_auth_answer(adapter, state, capture_id.trim(), "", output)? {
                        let key = stable_event_key("question-answer", event_index, event);
                        state.questions.handled_answers.insert(key);
                    }
                }
            }
            // ESC steps back through the form one prompt at a time; Ctrl+C
            // (`question_abort`) keeps abandoning `/auth` outright, so the multi-step flow
            // never costs the user their usual interrupt.
            Some("question_cancel") if event_targets_pending_auth(state, event) => {
                handle_auth_back(state, output)?;
            }
            Some("cancel") | Some("question_abort") if event_targets_pending_auth(state, event) => {
                cancel_auth_panel(state, output)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_auth_card_component(component: Option<&str>) -> bool {
    matches!(component, Some("card") | Some("card_secret"))
}

fn parse_card_id_usize(event: &ShellEvent) -> Option<(String, usize)> {
    let (id, val) = event.input.as_deref()?.split_once(':')?;
    let val = val.trim().parse::<usize>().ok()?;
    Some((id.trim().to_string(), val))
}

fn parse_card_id_text(event: &ShellEvent) -> Option<(String, String)> {
    let (id, text) = event.input.as_deref()?.split_once(':')?;
    Some((id.trim().to_string(), text.to_string()))
}
