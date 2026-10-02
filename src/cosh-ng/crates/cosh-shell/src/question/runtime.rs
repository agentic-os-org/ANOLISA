use crate::question::answer::{resolve_pending_question_answer, QuestionAnswerResolution};
use crate::question::choices::{
    question_choice_count, question_custom_answer_index, toggle_question_option,
};
use crate::question::ingress::{
    core_question_store_decision, reject_core_question_store, CoreQuestionStoreDecision,
    IncomingQuestion,
};
use crate::runtime::prelude::*;
use crate::runtime::question_terminal::clear_active_question_panel;
use crate::ui::QuestionInputFeedback;

#[derive(Debug, Clone)]
pub(crate) struct RuntimeUserQuestion {
    pub(crate) id: String,
    pub(crate) question: String,
    pub(crate) options: Vec<String>,
    pub(crate) selected_option: usize,
    pub(crate) selected_options: Vec<usize>,
    pub(crate) custom_answer: String,
    pub(crate) allow_free_text: bool,
    pub(crate) selection_mode: QuestionSelectionMode,
    pub(crate) input_feedback: QuestionInputFeedback,
    pub(crate) provider_request_id: Option<String>,
    pub(crate) provider_owner_request_id: Option<String>,
    pub(crate) origin: AgentRunOrigin,
    pub(crate) answer: Option<String>,
}

pub(crate) struct QuestionAnswerRun {
    pub(crate) question_id: String,
    pub(crate) question: String,
    pub(crate) answer: String,
    pub(crate) provider_request_id: Option<String>,
    pub(crate) provider_owner_request_id: Option<String>,
    pub(crate) request: AgentRequest,
    pub(crate) origin: AgentRunOrigin,
}

pub(crate) fn pending_question_capture(state: &InlineState) -> Option<RawInputCapture> {
    if let Some(question_id) = state.questions.pending_id.as_ref() {
        if let Some(question) = state
            .questions
            .items
            .iter()
            .find(|question| question.id == *question_id && question.answer.is_none())
        {
            return Some(RawInputCapture::Question {
                id: question.id.clone(),
                option_count: question.options.len(),
                selected: question.selected_option,
                allow_free_text: question.allow_free_text,
                multiple: question.selection_mode == QuestionSelectionMode::Multiple,
                secret: false,
            });
        }
    }

    None
}

pub(crate) fn has_pending_question(state: &InlineState) -> bool {
    state.questions.pending_id.is_some()
}

pub(crate) fn render_question_answer_actions<W: Write>(
    events: &[ShellEvent],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        if !is_question_answer_card_event(event) {
            continue;
        }

        let key = stable_event_key("question-answer", event_index, event);
        if !state.questions.handled_answers.insert(key) {
            continue;
        }

        // Skip if auth or hook-action panel is active — let the dedicated
        // card-action handlers consume the event (#1629).
        if state.auth.state.is_some() || state.hooks.pending_action.is_some() {
            continue;
        }

        // Reserve a control-queue slot BEFORE consuming the pending question,
        // but only when answering would actually enqueue a fallback Agent
        // continuation. Direct delivery to the active provider owner (and
        // paths that stop the run and start immediately) must never be gated
        // on queue capacity: the provider is waiting for exactly this answer,
        // and rejecting it would deadlock until the provider times out.
        if question_answer_needs_queue_slot(state) && !control_queue_has_capacity(state) {
            crate::slash::session::render_control_queue_full_notice(state, output)?;
            output.flush()?;
            continue;
        }

        let answer_run = match resolve_pending_question_answer(event, event_index, state) {
            QuestionAnswerResolution::Accepted(answer_run) => answer_run,
            QuestionAnswerResolution::NoPending => {
                let i18n = state.i18n();
                RatatuiInlineRenderer::for_terminal()
                    .with_language(state.language)
                    .write_notice_panel(
                        output,
                        NoticePanelModel {
                            title: i18n.t(MessageId::QuestionNoPendingTitle),
                            body: vec![i18n.t(MessageId::QuestionNoPendingBody).to_string()],
                            footer: None,
                        },
                    )?;
                output.flush()?;
                continue;
            }
            QuestionAnswerResolution::Ignored => continue,
            QuestionAnswerResolution::EmptyAnswer
            | QuestionAnswerResolution::SelectionRequired
            | QuestionAnswerResolution::InvalidAnswer => {
                let feedback = resolve_pending_question_answer_kind(event, state);
                apply_question_feedback(state, feedback);
                redraw_current_question(state, output)?;
                output.flush()?;
                continue;
            }
            QuestionAnswerResolution::RequestBuildFailed => {
                clear_active_question_panel(state, output)?;
                let i18n = state.i18n();
                RatatuiInlineRenderer::for_terminal()
                    .with_language(state.language)
                    .write_notice_panel(
                        output,
                        NoticePanelModel {
                            title: i18n.t(MessageId::QuestionAnswerNotSentTitle),
                            body: vec![i18n.t(MessageId::QuestionAnswerNotSentBody).to_string()],
                            footer: None,
                        },
                    )?;
                redraw_current_question(state, output)?;
                output.flush()?;
                continue;
            }
        };

        render_question_answer_notice(state, &answer_run, output)?;
        match respond_question_answer_to_provider(state, &answer_run) {
            ProviderQuestionResponse::Responded => {
                output.flush()?;
                continue;
            }
            ProviderQuestionResponse::OwnerUnavailable => {}
            ProviderQuestionResponse::NotProviderBacked
            | ProviderQuestionResponse::DeliveryFailed => {
                stop_active_agent_run_without_rendering(state, output)?;
            }
        }
        // The pending question was already consumed above (answer set, panel
        // cleared); a queue-full rejection here would strand a response the
        // user can no longer re-issue, so this control-protocol continuation is
        // guaranteed a queue slot.
        start_agent_run_control_response(
            &answer_run.request,
            answer_run.origin,
            adapter,
            state,
            output,
            Some(event_index),
        )?;
        output.flush()?;
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderQuestionResponse {
    NotProviderBacked,
    Responded,
    OwnerUnavailable,
    DeliveryFailed,
}

/// Whether answering the pending question would consume a control-queue slot.
///
/// Mirrors the delivery plan in [`render_question_answer_actions`]: a pending
/// or running compaction holds every continuation in the queue; with no
/// active run the continuation starts immediately; a provider-backed question
/// owned by the active run is answered directly through the owner's handle
/// (a delivery failure stops that run, so its fallback also starts
/// immediately), and a non-provider question stops the run before continuing.
/// Only an owner mismatch / missing owner keeps the active run alive and
/// forces the continuation into the queue.
fn question_answer_needs_queue_slot(state: &InlineState) -> bool {
    if crate::slash::session::compaction_pending_or_active(state) {
        return true;
    }
    let Some(active_run) = state.agent_run.active.as_ref() else {
        return false;
    };
    let Some(question_id) = state.questions.pending_id.as_ref() else {
        return false;
    };
    let Some(question) = state
        .questions
        .items
        .iter()
        .find(|question| question.id == *question_id && question.answer.is_none())
    else {
        return false;
    };
    if question.provider_request_id.is_none() {
        // NotProviderBacked stops the active run before continuing.
        return false;
    }
    // Provider-backed: direct delivery needs the active run to own the
    // question; anything else ends OwnerUnavailable with the run kept alive.
    question.provider_owner_request_id.as_deref() != Some(active_run.request.id.as_str())
}

fn respond_question_answer_to_provider(
    state: &InlineState,
    answer_run: &QuestionAnswerRun,
) -> ProviderQuestionResponse {
    let Some(request_id) = answer_run.provider_request_id.as_ref() else {
        return ProviderQuestionResponse::NotProviderBacked;
    };
    let Some(owner_request_id) = answer_run.provider_owner_request_id.as_ref() else {
        return ProviderQuestionResponse::OwnerUnavailable;
    };
    let Some(active_run) = state.agent_run.active.as_ref() else {
        return ProviderQuestionResponse::OwnerUnavailable;
    };
    if active_run.request.id != *owner_request_id {
        return ProviderQuestionResponse::OwnerUnavailable;
    };
    if active_run
        .handle
        .respond_question_answer(ApprovalResponse {
            request_id: request_id.clone(),
            tool_use_id: None,
            tool_input: None,
            decision: ApprovalDecision::Answer {
                answer: answer_run.answer.clone(),
            },
        })
        .is_ok()
    {
        ProviderQuestionResponse::Responded
    } else {
        ProviderQuestionResponse::DeliveryFailed
    }
}

pub(crate) fn render_question_cancel_actions<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        let Some(question_id) = question_cancel_from_event(event) else {
            continue;
        };

        let key = stable_event_key("question-cancel", event_index, event);
        if !state.questions.handled_cancellations.insert(key) {
            continue;
        }

        let Some(question_index) = state
            .questions
            .items
            .iter()
            .position(|question| question.id == question_id && question.answer.is_none())
        else {
            continue;
        };
        let active_run_owns_question = state.questions.items[question_index]
            .provider_owner_request_id
            .as_ref()
            .is_some_and(|owner_request_id| {
                state
                    .agent_run
                    .active
                    .as_ref()
                    .is_some_and(|run| run.request.id == *owner_request_id)
            });

        clear_active_question_panel(state, output)?;
        state.questions.items[question_index].answer = Some(String::new());
        if state.questions.pending_id.as_deref() == Some(question_id.as_str()) {
            state.questions.pending_id = None;
        }
        state.questions.active_panel_id = None;
        state.questions.active_panel_height = 0;
        state.questions.active_panel_cursor_row = None;
        state.questions.active_panel_width = None;
        if active_run_owns_question {
            stop_active_agent_run_without_rendering(state, output)?;
        }
        state.agent_run.needs_prompt_after_run = true;
        output.flush()?;
    }

    Ok(())
}

pub(crate) fn render_question_focus_actions<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        let Some((id, selected_option)) = question_focus_from_event(event) else {
            continue;
        };

        let key = stable_event_key("question-focus", event_index, event);
        if !state.questions.handled_focus.insert(key) {
            continue;
        }

        let Some(question) = state
            .questions
            .items
            .iter_mut()
            .find(|question| question.id == id && question.answer.is_none())
        else {
            continue;
        };

        let choice_count = question_choice_count(question.options.len(), question.allow_free_text);
        if choice_count == 0 {
            continue;
        }
        question.selected_option = selected_option.min(choice_count - 1);
        let custom = question_custom_answer_index(question.options.len(), question.allow_free_text);
        if custom != Some(question.selected_option) {
            question.input_feedback = QuestionInputFeedback::None;
        }
        redraw_current_question(state, output)?;
        output.flush()?;
    }

    Ok(())
}

pub(crate) fn render_question_toggle_actions<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        let Some((id, selected_option)) = question_toggle_from_event(event) else {
            continue;
        };

        let key = stable_event_key("question-toggle", event_index, event);
        if !state.questions.handled_focus.insert(key) {
            continue;
        }

        let Some(question) = state
            .questions
            .items
            .iter_mut()
            .find(|question| question.id == id && question.answer.is_none())
        else {
            continue;
        };
        if question.selection_mode != QuestionSelectionMode::Multiple {
            continue;
        }
        if selected_option >= question.options.len() {
            continue;
        }
        toggle_question_option(&mut question.selected_options, selected_option);
        question.input_feedback = QuestionInputFeedback::None;
        redraw_current_question(state, output)?;
        output.flush()?;
    }

    Ok(())
}

pub(crate) fn render_question_input_actions<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        let Some((id, text)) = question_input_from_event(event) else {
            continue;
        };

        let key = stable_event_key("question-input", event_index, event);
        if !state.questions.handled_focus.insert(key) {
            continue;
        }

        let Some(question) = state
            .questions
            .items
            .iter_mut()
            .find(|question| question.id == id && question.answer.is_none())
        else {
            continue;
        };
        if !question.allow_free_text {
            continue;
        }
        question.custom_answer = text;
        if !question.custom_answer.trim().is_empty() {
            question.input_feedback = QuestionInputFeedback::None;
        }
        if let Some(custom_idx) =
            question_custom_answer_index(question.options.len(), question.allow_free_text)
        {
            question.selected_option = custom_idx;
        }
        redraw_current_question(state, output)?;
        output.flush()?;
    }

    Ok(())
}

fn resolve_pending_question_answer_kind(
    event: &ShellEvent,
    state: &InlineState,
) -> QuestionInputFeedback {
    let Some(question_id) = state.questions.pending_id.as_ref() else {
        return QuestionInputFeedback::None;
    };
    let Some(question) = state
        .questions
        .items
        .iter()
        .find(|question| question.id == *question_id && question.answer.is_none())
    else {
        return QuestionInputFeedback::None;
    };
    let submitted = if event.message.as_deref() == Some("question_submit_empty") {
        ""
    } else {
        event.input.as_deref().unwrap_or_default()
    };
    if question.selection_mode == QuestionSelectionMode::Multiple && submitted.trim().is_empty() {
        QuestionInputFeedback::SelectionRequired
    } else if submitted.trim().is_empty() {
        QuestionInputFeedback::Required
    } else {
        QuestionInputFeedback::Invalid
    }
}

fn apply_question_feedback(state: &mut InlineState, feedback: QuestionInputFeedback) {
    let Some(question_id) = state.questions.pending_id.as_ref() else {
        return;
    };
    if let Some(question) = state
        .questions
        .items
        .iter_mut()
        .find(|question| question.id == *question_id && question.answer.is_none())
    {
        question.input_feedback = feedback;
    }
}

fn question_focus_from_event(event: &ShellEvent) -> Option<(String, usize)> {
    question_card_event(event, "focus")
}

fn question_toggle_from_event(event: &ShellEvent) -> Option<(String, usize)> {
    question_card_event(event, "toggle")
}

fn question_input_from_event(event: &ShellEvent) -> Option<(String, String)> {
    if event.kind != ShellEventKind::UserInputIntercepted
        || event.component.as_deref() != Some("card")
        || event.message.as_deref() != Some("input")
    {
        return None;
    }

    let (id, text) = event.input.as_deref()?.split_once(':')?;
    Some((id.trim().to_string(), text.to_string()))
}

/// A single-step question has nothing to step back through, so ESC (`question_cancel`) and Ctrl+C
/// (`question_abort`) both abandon it.
fn question_cancel_from_event(event: &ShellEvent) -> Option<String> {
    if event.kind != ShellEventKind::UserInputIntercepted
        || event.component.as_deref() != Some("card")
        || !matches!(
            event.message.as_deref(),
            Some("question_cancel") | Some("question_abort")
        )
    {
        return None;
    }
    Some(event.input.as_deref()?.trim().to_string())
}

fn question_card_event(event: &ShellEvent, message: &str) -> Option<(String, usize)> {
    if event.kind != ShellEventKind::UserInputIntercepted
        || event.component.as_deref() != Some("card")
        || event.message.as_deref() != Some(message)
    {
        return None;
    }

    let (id, selected) = event.input.as_deref()?.split_once(':')?;
    let selected = selected.trim().parse::<usize>().ok()?;
    Some((id.trim().to_string(), selected))
}

fn redraw_current_question<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    clear_active_question_panel(state, output)?;
    let Some(question_id) = state.questions.pending_id.clone() else {
        return Ok(());
    };
    render_user_questions(state, &[question_id], output)
}

pub(crate) fn agent_request_from_pending_question_answer(
    event: &ShellEvent,
    sequence: usize,
    state: &mut InlineState,
) -> Option<QuestionAnswerRun> {
    match resolve_pending_question_answer(event, sequence, state) {
        QuestionAnswerResolution::Accepted(answer_run) => Some(*answer_run),
        _ => None,
    }
}

pub(crate) fn render_question_answer_notice<W: Write>(
    state: &mut InlineState,
    answer_run: &QuestionAnswerRun,
    output: &mut W,
) -> std::io::Result<()> {
    clear_active_question_panel(state, output)?;
    RatatuiInlineRenderer::for_terminal()
        .with_language(state.language)
        .write_question_answer_panel(
            output,
            QuestionAnswerPanelModel {
                id: &answer_run.question_id,
                question: &answer_run.question,
                answer: &answer_run.answer,
                message: "",
            },
        )?;
    Ok(())
}

fn is_question_answer_card_event(event: &ShellEvent) -> bool {
    if event.component.as_deref() == Some("card") {
        return matches!(
            event.message.as_deref(),
            Some("answer" | "question_submit_empty")
        );
    }
    false
}

pub(crate) fn record_user_questions(
    state: &mut InlineState,
    governed_events: &[GovernedEvent],
    origin: AgentRunOrigin,
    provider_owner_request_id: Option<&str>,
) -> (Vec<String>, Option<(&'static str, usize, bool)>) {
    let mut ids = Vec::new();
    let mut rejection = None;
    for (event_index, event) in governed_events.iter().enumerate() {
        let AgentEvent::UserQuestion {
            run_id: _,
            provider_request_id,
            question,
            options,
            allow_free_text,
            selection_mode,
        } = &event.event
        else {
            continue;
        };
        match core_question_store_decision(
            state,
            IncomingQuestion {
                provider_request_id: provider_request_id.as_deref(),
                question,
                options,
                allow_free_text: *allow_free_text,
                selection_mode: *selection_mode,
            },
        ) {
            CoreQuestionStoreDecision::Accept => {}
            CoreQuestionStoreDecision::Duplicate => continue,
            CoreQuestionStoreDecision::Reject(reason) => {
                rejection = Some((reason, event_index, reject_core_question_store(state)));
                break;
            }
        }
        let id = next_question_id(state);
        let question = display_question_text(state, question);
        state.questions.items.push(RuntimeUserQuestion {
            id: id.clone(),
            question,
            options: options.clone(),
            selected_option: 0,
            selected_options: Vec::new(),
            custom_answer: String::new(),
            allow_free_text: *allow_free_text,
            selection_mode: *selection_mode,
            input_feedback: QuestionInputFeedback::None,
            provider_request_id: provider_request_id.clone(),
            provider_owner_request_id: provider_owner_request_id.map(ToString::to_string),
            origin,
            answer: None,
        });
        state.questions.pending_id = Some(id.clone());
        ids.push(id);
    }
    (ids, rejection)
}

fn display_question_text(state: &InlineState, question: &str) -> String {
    let question = question.trim();
    if question.is_empty() {
        state.i18n().t(MessageId::QuestionDefaultPrompt).to_string()
    } else {
        question.to_string()
    }
}

fn next_question_id(state: &InlineState) -> String {
    format!("q-{}", state.questions.items.len() + 1)
}

pub(crate) fn render_user_questions<W: Write>(
    state: &mut InlineState,
    question_ids: &[String],
    output: &mut W,
) -> std::io::Result<()> {
    for question_id in question_ids {
        let Some(question) = state
            .questions
            .items
            .iter()
            .find(|question| question.id == *question_id)
        else {
            continue;
        };

        let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
        let model = QuestionPanelModel {
            id: &question.id,
            question: &question.question,
            options: &question.options,
            selected_option: question.selected_option,
            selected_options: &question.selected_options,
            custom_answer: &question.custom_answer,
            allow_free_text: question.allow_free_text,
            selection_mode: question.selection_mode,
            input_feedback: question.input_feedback,
        };
        let cursor_row = renderer
            .active_question_cursor_placement(&model)
            .map(|placement| placement.row);
        let height = renderer.write_question_panel(output, model)?;
        state.questions.active_panel_id = Some(question.id.clone());
        state.questions.active_panel_height = height;
        state.questions.active_panel_cursor_row = cursor_row;
        state.questions.active_panel_width = Some(renderer.panel_standard_width());
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
