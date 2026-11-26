use std::io::{self, Write};

use ratatui::crossterm::event::KeyCode;

use crate::adapter::AdapterInstance;
use crate::i18n::{I18n, MessageId};
use crate::raw_input::{AuthNavigation, RawInputEvent};
use crate::runtime::guide::input::{GuideInput, Input};
use crate::runtime::guide::result::{ActionKind, EndReason};
use crate::runtime::state::InlineState;
use crate::ui::guide::{Screen, WizardRow, WizardView};

use crate::auth::provider_management::{provider_action_choice, ProviderAction};
use crate::auth::{capture, delete_confirm, ecs_poll, prompt, runtime};
use runtime::AuthPhase;

#[derive(Clone, Copy)]
enum Decision {
    Discard(MessageId),
    Activate,
    Delete,
}

pub(super) enum Answer {
    Confirm,
    Cancel,
    Quit,
    Home,
    Signal(i32),
    Lost,
}

fn confirm(
    screen: &mut Screen,
    input: &mut GuideInput,
    i18n: I18n,
    decision: Decision,
    service: &str,
) -> io::Result<Answer> {
    let (title, disclosure, action, cancel, back) = match decision {
        Decision::Discard(destination) => (
            MessageId::GuideAuthDiscardTitle,
            i18n.format(
                MessageId::GuideAuthDiscardBody,
                &[("destination", i18n.t(destination))],
            ),
            MessageId::GuideAuthDiscardAction,
            MessageId::GuideAuthContinueEditing,
            MessageId::GuideAuthContinueKey,
        ),
        Decision::Activate => (
            MessageId::GuideAuthActivateTitle,
            i18n.format(MessageId::GuideAuthActivateBody, &[("service", service)]),
            MessageId::GuideAuthActivateAction,
            MessageId::GuideWizardCancel,
            MessageId::GuideWizardCancelKey,
        ),
        Decision::Delete => (
            MessageId::GuideAuthDeleteTitle,
            i18n.format(MessageId::GuideAuthDeleteBody, &[("service", service)]),
            MessageId::GuideAuthDeleteAction,
            MessageId::GuideWizardCancel,
            MessageId::GuideWizardCancelKey,
        ),
    };
    let mut view = WizardView {
        title: i18n.t(title).to_string(),
        rows: vec![
            WizardRow::Text(disclosure),
            WizardRow::Blank,
            WizardRow::Choice {
                label: i18n.t(action).to_string(),
                value: String::new(),
            },
            WizardRow::Choice {
                label: i18n.t(cancel).to_string(),
                value: String::new(),
            },
        ],
        cursor: 3,
        hint: String::new(),
        message: None,
        markdown: None,
        scroll: 0,
        language: i18n.language(),
        message_fresh: false,
        follow_cursor: false,
        confirmation: true,
        review: None,
    };
    input.discard_pending();
    screen.resync()?;
    let mut dirty = true;
    let answer = loop {
        if let Some(signal) = crate::runtime::terminal::take_guide_signal() {
            break Answer::Signal(signal);
        }
        if dirty {
            view.hint = i18n.format(
                if matches!(decision, Decision::Discard(_)) {
                    MessageId::GuideAuthDiscardKeys
                } else {
                    MessageId::GuideWizardConfirmHint
                },
                &[(
                    "action",
                    i18n.t(if view.cursor == 2 { action } else { back }),
                )],
            );
            let drawn = screen.draw(&view)?;
            view.scroll = drawn.scroll;
            view.review = drawn.review;
            dirty = false;
        }
        match input.next() {
            Input::Lost => break Answer::Lost,
            Input::Resize => {
                view.cursor = 3;
                view.review = None;
                view.follow_cursor = false;
            }
            Input::Key(key) if screen.too_small() => match key.code {
                KeyCode::Esc | KeyCode::Char('q' | 'Q') => break Answer::Quit,
                _ => continue,
            },
            Input::Key(key) => {
                let ready = view.review.is_some_and(|review| review.ready);
                view.follow_cursor = false;
                match key.code {
                    KeyCode::Esc => break Answer::Cancel,
                    KeyCode::Char('q' | 'Q') if !matches!(decision, Decision::Discard(_)) => {
                        break Answer::Quit
                    }
                    KeyCode::Char('h') | KeyCode::F(2)
                        if !matches!(decision, Decision::Discard(_)) =>
                    {
                        break Answer::Home;
                    }
                    KeyCode::Enter if ready && view.cursor == 2 => break Answer::Confirm,
                    KeyCode::Enter => break Answer::Cancel,
                    KeyCode::Up | KeyCode::Down if ready => {
                        view.cursor = if view.cursor == 2 { 3 } else { 2 };
                        view.follow_cursor = true;
                    }
                    KeyCode::Up => view.scroll = view.scroll.saturating_sub(1),
                    KeyCode::Down => view.scroll += 1,
                    KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(10),
                    KeyCode::PageDown => view.scroll += 10,
                    _ => continue,
                }
            }
            Input::Tick => continue,
        }
        dirty = true;
    };
    input.discard_pending();
    screen.resync()?;
    Ok(answer)
}

fn ask<W: Write>(
    state: &mut InlineState,
    screen: &mut Screen,
    input: &mut GuideInput,
    output: &mut W,
    decision: Decision,
    service: &str,
) -> io::Result<Answer> {
    prompt::clear_active_auth_panel(state, output)?;
    output.flush()?;
    confirm(screen, input, state.i18n(), decision, service)
}

pub(super) fn route<W: Write>(
    state: &mut InlineState,
    adapter: &AdapterInstance,
    screen: &mut Screen,
    input: &mut GuideInput,
    output: &mut W,
    event: RawInputEvent,
    counter: &mut usize,
) -> io::Result<Option<Answer>> {
    let Some(auth) = state.auth.state.as_ref() else {
        return Ok(None);
    };
    let phase = auth.phase.clone();
    let navigation = match event {
        RawInputEvent::GuideAuthNavigation(navigation) => Some(navigation),
        _ => None,
    };
    let esc = matches!(event, RawInputEvent::QuestionCancel(_));
    let abort = matches!(event, RawInputEvent::QuestionAbort(_));
    if ecs_poll::handles(auth) && ecs_poll::option_count(state) == 0 {
        if !state
            .auth
            .ecs
            .as_ref()
            .is_some_and(ecs_poll::EcsFlow::can_cancel)
        {
            return Ok(None);
        }
        if navigation.is_none() {
            if esc {
                runtime::handle_auth_back(state, output)?;
            }
            if abort {
                runtime::cancel_auth_panel(state, output)?;
            }
            return Ok(None);
        }
    }
    if phase == AuthPhase::FillingField {
        let first = auth.current_field == auth.first_editable_field();
        if esc && !first {
            state
                .auth
                .guide_drafts
                .insert(auth.current_field, auth.field_input.clone());
        } else if esc || navigation.is_some() {
            let back_to = match navigation {
                Some(AuthNavigation::Home) => MessageId::GuideAuthDestinationHome,
                Some(AuthNavigation::Overview | AuthNavigation::Quit) => {
                    MessageId::GuideAuthDestinationOverview
                }
                None if auth.editing_provider_name.is_some() => {
                    MessageId::GuideAuthDestinationService
                }
                None if auth.from_sysom_shortcut => MessageId::GuideAuthDestinationManagement,
                None => MessageId::GuideAuthDestinationTemplates,
            };
            if state.auth.guide_edited {
                match ask(state, screen, input, output, Decision::Discard(back_to), "")? {
                    Answer::Confirm => state.auth.outcome.reason = EndReason::Cancelled,
                    Answer::Cancel => {
                        prompt::render_current_auth_panel(state, output)?;
                        return Ok(None);
                    }
                    answer => return Ok(Some(answer)),
                }
            }
            state.auth.guide_drafts.clear();
            state.auth.guide_edited = false;
        }
    }
    match navigation {
        Some(AuthNavigation::Home) => return Ok(Some(Answer::Home)),
        Some(AuthNavigation::Quit) => return Ok(Some(Answer::Quit)),
        Some(AuthNavigation::Overview) => {
            state.auth.exit_requested = true;
            return Ok(None);
        }
        None => {}
    }
    if esc && phase == AuthPhase::ManagingProviders {
        state.auth.exit_requested = true;
        return Ok(None);
    }
    if let (RawInputEvent::CardAnswer(_), AuthPhase::ProviderAction { provider_idx }) =
        (&event, phase)
    {
        let auth = state.auth.state.as_ref().expect("provider action state");
        let provider = &auth.existing_providers[provider_idx];
        let decision = match provider_action_choice(
            provider.is_active,
            provider.editable,
            provider.deletable(),
            auth.selected_provider,
        ) {
            ProviderAction::Activate => Some(Decision::Activate),
            ProviderAction::Delete => Some(Decision::Delete),
            _ => None,
        };
        if let Some(decision) = decision {
            let kind = if matches!(decision, Decision::Delete) {
                ActionKind::Delete
            } else {
                ActionKind::Activate
            };
            let service = provider.name.clone();
            if super::refuse_unconfirmed_write(state, kind, output)? {
                return Ok(None);
            }
            match ask(state, screen, input, output, decision, &service)? {
                Answer::Confirm => {
                    if matches!(decision, Decision::Delete) {
                        let auth = state.auth.state.as_mut().expect("provider action state");
                        delete_confirm::begin_delete_confirmation(auth, provider_idx);
                        delete_confirm::focus_delete_confirmation(auth, 1);
                        let id = capture::auth_capture_id(auth);
                        runtime::handle_auth_answer(adapter, state, &id, "", output)?;
                        return Ok(None);
                    }
                }
                Answer::Cancel => {
                    prompt::render_current_auth_panel(state, output)?;
                    return Ok(None);
                }
                answer => return Ok(Some(answer)),
            }
        }
    }
    let events = super::auth_events(&[event], counter);
    super::render_auth_card_actions(&events, adapter, state, output, *counter)?;
    Ok(None)
}
