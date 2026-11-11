use crate::agent::run::{
    accept_deferred_agent_run, activate_deferred_agent_run, poll_accepted_agent_run,
};
use crate::config::HealthConfig;
use crate::diagnostics::health::runtime::HealthScanHandle;
use crate::diagnostics::health::HealthScanReport;
use crate::runtime::prelude::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};

mod prompt;
#[cfg(test)]
mod welcome_tests;

pub(crate) fn integration_for_launch(
    options: &crate::runtime::cli_args::LaunchOptions,
    configured: crate::shell_host::ShellIntegration,
) -> crate::shell_host::ShellIntegration {
    if options.resume.is_some() || options.initial_question.is_some() {
        crate::shell_host::ShellIntegration::Enhanced
    } else {
        configured
    }
}

pub(crate) fn load_failure_context(
    options: &crate::runtime::cli_args::LaunchOptions,
) -> Result<Option<crate::insight::guidance::BashEvent>, crate::config::guide::Error> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    options
        .failure_context
        .as_deref()
        .map(|id| crate::config::guide::load_failure_context(id, now_ms))
        .transpose()
}

#[derive(Default)]
pub(crate) struct StartupHealthState {
    pub(crate) pending: Option<HealthScanHandle>,
    pub(crate) config: HealthConfig,
    pub(crate) report: Option<HealthScanReport>,
    pub(crate) rendered: bool,
}

impl StartupHealthState {
    pub(crate) fn wait_ready(&mut self, timeout: Duration) {
        if self.report.is_some() || self.rendered {
            return;
        }
        let Some(mut handle) = self.pending.take() else {
            return;
        };
        if let Some(report) = handle.finish_within(timeout) {
            self.store_report(report);
        } else {
            handle.cancel();
            self.rendered = true;
        }
    }

    pub(crate) fn poll_ready(&mut self) {
        if self.report.is_some() || self.rendered {
            return;
        }
        let Some(handle) = self.pending.as_mut() else {
            return;
        };
        if let Some(report) = handle.poll_ready() {
            self.pending = None;
            self.store_report(report);
        }
    }

    fn store_report(&mut self, report: HealthScanReport) {
        self.report = Some(report);
    }
}

/// Resolves credentials for the ordinary welcome screen without blocking startup.
/// Explicit initial questions let Core select trial access or request authentication.
#[derive(Default)]
pub(crate) struct StartupAuthState {
    pub(crate) pending: Option<mpsc::Receiver<Option<bool>>>,
    pub(crate) resolved: Option<bool>,
}

impl StartupAuthState {
    pub(crate) fn should_probe(
        ai_enabled: bool,
        compact_startup: bool,
        banner_enabled: bool,
    ) -> bool {
        !compact_startup && ai_enabled && banner_enabled
    }

    pub(crate) fn wait_ready(&mut self, timeout: Duration) {
        if self.resolved.is_some() {
            return;
        }
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.recv_timeout(timeout) {
            Ok(resolved) => {
                self.resolved = resolved;
                self.pending = None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.pending = None;
            }
        }
    }

    pub(crate) fn poll_ready(&mut self) {
        if self.resolved.is_some() {
            return;
        }
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(resolved) => {
                self.resolved = resolved;
                self.pending = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
            }
        }
    }

    /// True only when the authority explicitly reported "no usable
    /// credentials"; uncertainty never shows the hint.
    pub(crate) fn ai_unconfigured(&self) -> bool {
        self.resolved == Some(false)
    }
}

#[derive(Default)]
pub(crate) struct StartupSessionState {
    pub(crate) login_invitation: crate::runtime::login::invitation::InvitationState,
    pub(crate) initial_question: Option<String>,
    pub(crate) failure_context: Option<crate::insight::guidance::BashEvent>,
    pub(crate) compact: bool,
    pub(crate) prompt_snapshot: Option<crate::raw_input::PromptSnapshot>,
    prompt_wait_started: Option<Instant>,
    pending_notice: Option<&'static str>,
    pub(crate) continuation_selected: Option<usize>,
    continuation_generation: u64,
    continuation_submitted: bool,
}

pub(crate) fn render_initial_question<W: Write>(
    events: &[ShellEvent],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if state.rendered_startup_banner {
        return render_initial_question_notice(state, output);
    }
    if events.iter().any(|event| {
        event.kind == ShellEventKind::CommandStarted
            || (event.kind == ShellEventKind::UserInputIntercepted
                && event.capture.is_none()
                && !(event.component.as_deref() == Some("shell_prompt_ready")
                    && event.message.as_deref() == Some("painted"))
                && !(event.component.as_deref() == Some("shell_input")
                    && event.message.as_deref() == Some("input empty")))
    }) {
        cancel_automatic_initial_question(state);
        return Ok(());
    }
    if let Some(ready) = events
        .iter()
        .find(|event| event.kind == ShellEventKind::ShellReady)
    {
        state
            .startup_session
            .prompt_wait_started
            .get_or_insert_with(Instant::now);
        if state.shell_session_id.is_none() {
            state.shell_session_id = Some(ready.session_id.clone());
        }
        state.shell_prompt_cwd = ready.cwd.clone();
    }
    let Some(started) = state.startup_session.prompt_wait_started else {
        return Ok(());
    };
    if state.startup_session.prompt_snapshot.is_none() {
        if started.elapsed() >= Duration::from_secs(5) {
            state.rendered_startup_banner = true;
            cancel_initial_question(state);
            writeln!(output, "\r\nCosh AI Shell: the initial question was not submitted because the Shell prompt is unavailable. Continue using Shell or ask the question directly.")?;
            crate::slash::prompt::write_shell_prompt(state, output)?;
            output.flush()?;
        }
        return Ok(());
    }
    submit_initial_question(adapter, state, output, true)
}

fn mark_initial_question_displayed(state: &mut InlineState) {
    state.rendered_startup_banner = true;
    state.startup_health.rendered = true;
    state.startup_upgrade.rendered = true;
}

fn cancel_automatic_initial_question(state: &mut InlineState) {
    cancel_initial_question(state);
    state.startup_session.prompt_snapshot = None;
    state.startup_session.pending_notice = Some("Cosh AI Shell: the initial question was not submitted because another input started. Continue using Shell or ask the question directly.");
    state.rendered_startup_banner = true;
}

fn render_initial_question_notice<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if !state.shell_at_prompt
        || state.personalization.shell_input_active
        || state.agent_run.active.is_some()
        || crate::runtime::controller::pending_card_capture(state).is_some()
    {
        return Ok(());
    }
    if let Some(notice) = state.startup_session.pending_notice.take() {
        writeln!(output, "\r\n{notice}")?;
        crate::slash::prompt::write_shell_prompt(state, output)?;
        output.flush()?;
    }
    Ok(())
}

fn submit_initial_question<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    show_identity: bool,
) -> std::io::Result<()> {
    let Some(question) = state.startup_session.initial_question.clone() else {
        return Ok(());
    };
    let mut input = ShellEvent::user_input_intercepted(
        state.shell_session_id.clone().unwrap_or_default(),
        question.clone(),
    );
    input.cwd = state.shell_prompt_cwd.clone();
    let Some(mut request) = agent_request_from_intercepted_input(&input, 0, true) else {
        return Ok(());
    };
    if let Some(event) = &state.startup_session.failure_context {
        request
            .context_hints
            .push(crate::insight::guidance::external_failure_hint(event));
    }
    request.id = format!("agent-request-initial-{}", input.session_id);
    request.command_block.id = format!("initial-question-{}", input.session_id);
    let snapshot = state.startup_session.prompt_snapshot.clone();
    let mut accepted = false;
    let mut commit = || {
        if show_identity {
            mark_initial_question_displayed(state);
        }
        let disposition = accept_deferred_agent_run(&request, state);
        if disposition == AgentStartDisposition::Queued {
            cancel_initial_question(state);
            state.agent_run.needs_prompt_after_run = true;
            accepted = true;
        }
        if show_identity {
            write_initial_question(state, &question, output)?;
            output.flush()?;
        }
        Ok(disposition)
    };
    let result = if show_identity {
        snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.with_ready(&mut commit))
    } else {
        Some(commit())
    };
    let Some(result): Option<std::io::Result<AgentStartDisposition>> = result else {
        cancel_automatic_initial_question(state);
        return Ok(());
    };
    let activated = if accepted {
        activate_deferred_agent_run(adapter, state)
    } else {
        None
    };
    let disposition = result?;
    if activated == Some(AgentStartDisposition::Started) {
        return poll_accepted_agent_run(state, output, adapter);
    }
    if disposition == AgentStartDisposition::QueueFull {
        crate::slash::session::render_agent_queue_full_notice(state, output)?;
        offer_initial_question(state, output)?;
    }
    output.flush()
}

fn write_initial_question<W: Write>(
    state: &InlineState,
    question: &str,
    output: &mut W,
) -> std::io::Result<()> {
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    if let Some(prompt) = state
        .startup_session
        .prompt_snapshot
        .as_ref()
        .filter(|prompt| !renderer.plain && prompt.starts_at_line_start())
    {
        prompt::clear_initial_prompt(prompt.bytes(), output)?;
    } else {
        write!(output, "\r\n")?;
    }
    renderer.write_welcome(output, super::welcome::welcome_model(state, None))?;
    writeln!(output)?;
    if let Some(prompt) = state.startup_session.prompt_snapshot.as_ref() {
        output.write_all(prompt.bytes())?;
    }
    let question = crate::evidence::clean_terminal_control_sequences(question);
    writeln!(output, "{question}")
}

pub(crate) fn cancel_initial_question(state: &mut InlineState) {
    state.startup_session.initial_question = None;
    state.startup_session.failure_context = None;
    state.startup_session.continuation_selected = None;
    state.startup_session.continuation_submitted = false;
    state
        .questions
        .items
        .retain(|question| !question.id.starts_with("startup-initial-question-"));
}

fn continuation_id(state: &InlineState) -> String {
    format!(
        "startup-initial-question-{}",
        state.startup_session.continuation_generation
    )
}

pub(crate) fn initial_question_capture(state: &InlineState) -> Option<RawInputCapture> {
    if state.agent_run.active.is_some()
        || crate::auth::runtime::has_auth_operation(state)
        || state.startup_session.initial_question.is_none()
    {
        return None;
    }
    Some(RawInputCapture::Question {
        id: continuation_id(state),
        option_count: 2,
        selected: state.startup_session.continuation_selected?,
        allow_free_text: false,
        multiple: false,
        secret: false,
    })
}

pub(crate) fn offer_initial_question<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    if state.startup_session.initial_question.is_none()
        || state.agent_run.active.is_some()
        || crate::auth::runtime::has_auth_operation(state)
    {
        return Ok(false);
    }
    if crate::runtime::controller::pending_card_capture(state).is_some_and(|capture| {
        !matches!(capture, RawInputCapture::Question { id, .. } if id == continuation_id(state))
    }) { return Ok(false); }
    if state
        .questions
        .active_panel_id
        .as_deref()
        .is_some_and(|id| id.starts_with("startup-initial-question-"))
    {
        crate::runtime::question_terminal::clear_active_question_panel(state, output)?;
    }
    state.startup_session.continuation_generation += 1;
    state.startup_session.continuation_selected = Some(0);
    state.startup_session.continuation_submitted = false;
    render_initial_question_choice(state, output)?;
    Ok(true)
}

fn render_initial_question_choice<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let (title, continue_label, cancel_label) = match state.language {
        Language::ZhCn => ("继续处理刚才的问题？", "继续原问题", "取消原问题"),
        Language::EnUs => (
            "Continue with your original question?",
            "Continue original question",
            "Cancel original question",
        ),
    };
    let options = vec![continue_label.to_string(), cancel_label.to_string()];
    let question = format!(
        "{title}\n{}",
        crate::evidence::clean_terminal_control_sequences(
            state
                .startup_session
                .initial_question
                .as_deref()
                .unwrap_or_default()
        )
    );
    let id = continuation_id(state);
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    let selected_option = state.startup_session.continuation_selected.unwrap_or(0);
    let model = QuestionPanelModel {
        id: &id,
        question: &question,
        options: &options,
        selected_option,
        selected_options: &[],
        custom_answer: "",
        allow_free_text: false,
        selection_mode: QuestionSelectionMode::Single,
        input_feedback: crate::ui::QuestionInputFeedback::Disabled,
    };
    let cursor_row = renderer
        .active_question_cursor_placement(&model)
        .map(|placement| placement.row);
    let height = renderer.write_question_panel(output, model)?;
    state
        .questions
        .items
        .retain(|question| !question.id.starts_with("startup-initial-question-"));
    state
        .questions
        .items
        .push(crate::question::runtime::RuntimeUserQuestion {
            id: id.clone(),
            question,
            options,
            selected_option,
            selected_options: Vec::new(),
            custom_answer: String::new(),
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: crate::ui::QuestionInputFeedback::Disabled,
            provider_request_id: None,
            provider_owner_request_id: None,
            origin: AgentRunOrigin::Standard,
            answer: None,
        });
    state.questions.active_panel_id = Some(id);
    state.questions.active_panel_height = height;
    state.questions.active_panel_width = Some(renderer.panel_standard_width());
    state.questions.active_panel_cursor_row = cursor_row;
    output.flush()
}

pub(crate) fn handle_initial_question_actions<W: Write>(
    events: &[ShellEvent],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    if crate::auth::runtime::has_auth_operation(state) || state.agent_run.active.is_some() {
        state.startup_session.continuation_submitted = false;
        return Ok(());
    }
    if !matches!(crate::runtime::controller::pending_card_capture(state),
        Some(RawInputCapture::Question { id, .. }) if id == continuation_id(state))
    {
        state.startup_session.continuation_submitted = false;
        return Ok(());
    }
    let id = continuation_id(state);
    for (index, event) in events.iter().enumerate() {
        if let Some(capture) = event.capture.as_ref() {
            state.startup_session.continuation_submitted = capture.lifecycle
                == crate::types::ShellCaptureLifecycle::Submitted
                && capture.kind.as_deref() == Some("question")
                && capture.target_id.as_deref() == Some(&id);
            continue;
        }
        if event.kind != ShellEventKind::UserInputIntercepted {
            state.startup_session.continuation_submitted = false;
            continue;
        }
        if event.component.as_deref() != Some("card") {
            state.startup_session.continuation_submitted = false;
            continue;
        }
        let key = stable_event_key("question-answer", event_index_base + index, event);
        match event.message.as_deref() {
            Some("focus") => {
                let Some((target, selected)) = event
                    .input
                    .as_deref()
                    .and_then(|input| input.split_once(':'))
                else {
                    continue;
                };
                if target != id {
                    state.startup_session.continuation_submitted = false;
                    continue;
                }
                if !state.questions.handled_focus.insert(stable_event_key(
                    "question-focus",
                    event_index_base + index,
                    event,
                )) {
                    continue;
                }
                if let Ok(selected) = selected.parse::<usize>() {
                    state.startup_session.continuation_selected = Some(selected.min(1));
                    crate::runtime::question_terminal::clear_active_question_panel(state, output)?;
                    render_initial_question_choice(state, output)?;
                }
            }
            Some("answer") => {
                if !state.questions.handled_answers.insert(key) {
                    continue;
                }
                if !state.startup_session.continuation_submitted {
                    continue;
                }
                state.startup_session.continuation_submitted = false;
                let answer = event.input.as_deref();
                if !matches!(answer, Some("1" | "2")) {
                    continue;
                }
                crate::runtime::question_terminal::clear_active_question_panel(state, output)?;
                if answer == Some("1") {
                    submit_initial_question(adapter, state, output, false)?;
                } else {
                    cancel_initial_question(state);
                    crate::slash::prompt::write_shell_prompt(state, output)?;
                }
            }
            Some("cancel" | "question_cancel" | "question_abort")
                if event.input.as_deref() == Some(&id) =>
            {
                if !state.questions.handled_answers.insert(key) {
                    continue;
                }
                cancel_initial_question(state);
                crate::runtime::question_terminal::clear_active_question_panel(state, output)?;
                crate::slash::prompt::write_shell_prompt(state, output)?;
            }
            _ => state.startup_session.continuation_submitted = false,
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
