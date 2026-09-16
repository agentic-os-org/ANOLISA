use std::io::Write;

use crate::activity::runtime::{
    close_untracked_shell_handoffs, record_approved_shell_handoff_blocks, render_activity_rows,
};
use crate::adapter::AgentAdapter;
use crate::agent::events::flush_held_agent_events;
use crate::agent::failed_command::{
    block_end_event_index, collect_failed_command_insights_with_history_base,
    failed_command_candidate, failed_command_intervention, render_post_failure_actions,
    start_agent_for_block, FailedCommandAgentStartOptions, FailedCommandAnalysisTrigger,
};
use crate::agent::intercept::render_intercept_agent_guidance;
use crate::agent::poll::{poll_active_agent_run, poll_active_agent_run_deferred};
use crate::agent::run::{
    start_agent_run_with_origin, stop_active_agent_run_without_rendering, AgentStartIntent,
};
use crate::approval::runtime::render_approval_actions;
use crate::i18n::MessageId;
use crate::insight::model::InterventionDecision;
use crate::insight::policy::InterventionGates;
use crate::question::runtime::{
    render_question_answer_actions, render_question_cancel_actions, render_question_focus_actions,
    render_question_input_actions, render_question_toggle_actions,
};
use crate::recommendation::personal_integration::record_completed_command_blocks;
use crate::recommendation::runtime::render_selection_actions;
use crate::runtime::cancel::render_agent_cancel_actions;
use crate::runtime::details::render_runtime_details_card_actions;
use crate::runtime::evidence_delivery::shell_handoff_continuation_requests;
use crate::runtime::evidence_requests::render_evidence_request_actions;
use crate::runtime::hooks::{
    handle_consultation_events, record_blocks_followed_by_user_input,
    record_command_hook_findings_with_history_base, render_queued_hook_consultation,
    render_recorded_hook_findings,
};
use crate::runtime::insight::render_pending_command_insight;
use crate::runtime::prelude::{
    build_command_blocks, findings_from_blocks, AdapterInstance, CommandBlock, ShellEvent,
    ShellEventKind,
};
use crate::runtime::state::InlineState;
use crate::slash::runtime::render_slash_actions;
use crate::slash::session::poll_background_compaction;

use super::controller::pending_card_capture;
use super::events::{ShellEventBatch, ShellEventCursor, ShellEventSnapshot};
use super::startup::{
    render_pending_recommendation_notice, render_startup_banner, render_startup_health_banner,
};

pub(crate) enum RuntimeAction {
    AdvanceEventCursor(ShellEventCursor),
}

pub(crate) fn stable_event_key(prefix: &str, idx: usize, event: &ShellEvent) -> String {
    match event.started_at_ms {
        Some(started_at_ms) if event.component.as_deref() == Some("card_secret") => {
            format!("{prefix}:{started_at_ms}:card_secret:{idx}")
        }
        Some(started_at_ms) => format!(
            "{prefix}:{}:{}:{}",
            started_at_ms,
            event.component.as_deref().unwrap_or_default(),
            event.input.as_deref().unwrap_or_default()
        ),
        None => format!("{prefix}:{idx}"),
    }
}

pub(crate) struct RuntimeDispatcher;
pub(crate) struct QuestionConsumer;
pub(crate) struct SlashConsumer;
pub(crate) struct ApprovalConsumer;
pub(crate) struct ActivityConsumer;
pub(crate) struct EvidenceRequestConsumer;

impl RuntimeDispatcher {
    pub(crate) fn dispatch_inline_batch<W: Write>(
        snapshot: &ShellEventSnapshot,
        adapter: &AdapterInstance,
        shell_label: &str,
        state: &mut InlineState,
        output: &mut W,
    ) -> std::io::Result<Vec<RuntimeAction>> {
        let batch = snapshot.batch_since(state.control.event_cursor());
        render_inline_guidance_from_batch(snapshot, &batch, adapter, shell_label, state, output)?;
        crate::auth::ecs_poll::poll(adapter, state, output)?;
        Ok(vec![RuntimeAction::AdvanceEventCursor(batch.to)])
    }

    pub(crate) fn apply_actions(actions: Vec<RuntimeAction>, state: &mut InlineState) {
        for action in actions {
            match action {
                RuntimeAction::AdvanceEventCursor(cursor) => {
                    state.control.set_event_cursor(cursor);
                }
            }
        }
    }
}

fn render_inline_guidance_from_batch<W: Write>(
    snapshot: &ShellEventSnapshot,
    batch: &ShellEventBatch,
    adapter: &AdapterInstance,
    shell_label: &str,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    state.personalization.poll_ready();
    let events = snapshot.events();
    let history_index_base = snapshot.base();
    let action_events = batch.events;
    let event_index_base = batch.global_index(0);
    if action_events.is_empty() {
        return poll_inline_runtime_without_shell_events(adapter, state, output);
    }

    state.control.observe_shell_command_activity(action_events);
    state.shell_exited |= action_events
        .iter()
        .any(|event| event.kind == ShellEventKind::ShellExited);
    #[cfg(test)]
    state.control.record_ledger_rebuild();
    let ledger = build_command_blocks(events);
    record_completed_command_blocks(state, &ledger.blocks);
    state.session_blocks = ledger.blocks.clone();
    // Positive evidence of command activity (R9): incomplete or
    // unmatched markers produce ledger errors instead of blocks, so an
    // empty `session_blocks` alone never proves the shell has not run
    // (and cd'd inside) a command. Absolute cursors make each marker part of
    // an incremental batch at most once even after the retained event window
    // compacts, so this flag stays monotonic without full-history rescans.
    state.shell_command_activity_observed |= action_events.iter().any(|event| {
        matches!(
            event.kind,
            ShellEventKind::CommandStarted
                | ShellEventKind::CommandCompleted
                | ShellEventKind::CommandFailed
        )
    });
    // The shell's latest prompt-time cwd report: a `ShellReady` event
    // is a precmd marker with no command in flight and carries the
    // shell's `$PWD`, so it is positive evidence both that the marker
    // channel works and of where the shell sits. Any later PTY input
    // write invalidates the report — submit-detection in the byte
    // stream is a documented heuristic (a custom `accept-line`
    // binding is indistinguishable from editing keys), and the
    // submitted line may have been a `cd` whose markers were lost
    // entirely — so the report is only current while no user input
    // follows it. Scanned newest first: the most recent decisive
    // event wins, and an event-free dispatch never erases the last
    // known state.
    for event in action_events.iter().rev() {
        if event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("shell_pty_input")
            && event.message.as_deref() == Some("write")
        {
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
    if state.shell_exited {
        if let Some(event) = events
            .iter()
            .rev()
            .find(|event| event.kind == ShellEventKind::ShellExited)
        {
            crate::agent::intercept::finalize_personal_prompt_feedback_on_exit(event, state);
        }
        stop_active_agent_run_without_rendering(state, output)?;
        return Ok(());
    }
    // Freeze/release the draft before any control consumer renders or opens a card.
    crate::runtime::prompt_draft::handle_prompt_draft_events(
        action_events,
        state,
        output,
        adapter.name(),
    )?;
    // Task forms reuse Question/TextQuestion captures. Route them first and
    // reserve their answer events so QuestionConsumer never emits a false
    // "No pending question" notice for Task-owned input.
    crate::slash::render_task_form_actions(action_events, state, output, event_index_base)?;
    crate::slash::render_task_snapshot_actions(action_events, state, output, event_index_base)?;
    let question_actions =
        QuestionConsumer::consume(action_events, adapter, state, output, event_index_base)?;
    RuntimeDispatcher::apply_actions(question_actions, state);
    crate::auth::runtime::render_auth_card_actions(
        action_events,
        adapter,
        state,
        output,
        event_index_base,
    )?;
    // Hook-action disambiguation panel (#1629): route focus/answer/cancel
    // events for the pending hook-action question panel.
    crate::slash::hooks::render_hook_action_card_actions(
        action_events,
        adapter,
        state,
        output,
        event_index_base,
    )?;
    let evidence_actions = EvidenceRequestConsumer::consume(
        action_events,
        &ledger.blocks,
        adapter,
        state,
        output,
        event_index_base,
    )?;
    RuntimeDispatcher::apply_actions(evidence_actions, state);
    let approval_actions = ApprovalConsumer::consume(
        action_events,
        &ledger.blocks,
        adapter,
        state,
        output,
        event_index_base,
    )?;
    RuntimeDispatcher::apply_actions(approval_actions, state);
    let shell_busy = state.control.shell_busy();
    if shell_busy {
        if let Some(cancellation) = state.personalization.analyzer_cancellation.as_ref() {
            cancellation.set_foreground_idle(false);
        }
        state.personalization.idle_since = None;
        let slash_actions = SlashConsumer::consume(
            action_events,
            &ledger.blocks,
            adapter,
            state,
            output,
            event_index_base,
        )?;
        RuntimeDispatcher::apply_actions(slash_actions, state);
        render_runtime_details_card_actions(
            action_events,
            &ledger.blocks,
            state,
            output,
            event_index_base,
        )?;
        poll_active_agent_run_deferred(state, output, adapter)?;
        // Foreground output is active: harvest compactor results but defer
        // completion rendering to the next safe prompt boundary.
        poll_background_compaction(state, output, adapter, true)?;
        return Ok(());
    }

    render_startup_banner(events, adapter, shell_label, state, output)?;
    render_startup_health_banner(state, output)?;
    render_pending_recommendation_notice(state, output)?;
    update_personal_shell_input_state(action_events, state);
    update_soft_newline_tip_state(action_events, state);
    let personal_idle = state.agent_run.active.is_none()
        && !state.personalization.shell_input_active
        && !action_events
            .iter()
            .any(|event| event.kind == ShellEventKind::UserInputIntercepted);
    crate::recommendation::personal_session::poll_personal_session(state, adapter, personal_idle);
    let slash_actions = SlashConsumer::consume(
        action_events,
        &ledger.blocks,
        adapter,
        state,
        output,
        event_index_base,
    )?;
    RuntimeDispatcher::apply_actions(slash_actions, state);
    render_runtime_details_card_actions(
        action_events,
        &ledger.blocks,
        state,
        output,
        event_index_base,
    )?;
    let activity_actions = ActivityConsumer::consume(events, &ledger.blocks, state, output)?;
    RuntimeDispatcher::apply_actions(activity_actions, state);
    let findings = findings_from_blocks(&ledger.blocks);
    record_blocks_followed_by_user_input(events, &ledger.blocks, state);
    handle_consultation_events(action_events, &ledger.blocks, adapter, state, output)?;
    render_queued_hook_consultation(state, output)?;
    record_command_hook_findings_with_history_base(
        events,
        &ledger.blocks,
        state,
        history_index_base,
        event_index_base,
    );
    render_recorded_hook_findings(&ledger.blocks, state, output)?;
    render_intercept_agent_guidance(
        action_events,
        &ledger.blocks,
        adapter,
        state,
        output,
        event_index_base,
    )?;
    render_agent_cancel_actions(
        action_events,
        &ledger.blocks,
        state,
        output,
        event_index_base,
    )?;

    let analysis_mode = state.analysis_mode;
    let auto_runtime_available =
        state.agent_run.active.is_none() && pending_card_capture(state).is_none();
    let auto_blocks = ledger
        .blocks
        .iter()
        .rev()
        .filter(|block| {
            auto_runtime_available
                && block.origin == crate::types::CommandOrigin::UserInteractive
                && !state.hooks.block_followed_by_user_input(&block.id)
                && block_end_event_index(events, block)
                    .map(|idx| history_index_base.saturating_add(idx))
                    .is_some_and(|idx| idx >= event_index_base)
        })
        .collect::<Vec<_>>();
    for block in auto_blocks {
        if state.agent_run.active.is_some() {
            break;
        }
        let Some(candidate) = failed_command_candidate(events, block) else {
            continue;
        };
        let user_has_not_continued = !state.hooks.block_followed_by_user_input(&block.id);
        let gates = InterventionGates {
            same_dispatch_batch: block_end_event_index(events, block)
                .map(|idx| history_index_base.saturating_add(idx))
                .is_some_and(|idx| idx >= event_index_base),
            input_empty: user_has_not_continued,
            foreground_idle: !shell_busy,
            active_runtime_idle: state.agent_run.active.is_none()
                && pending_card_capture(state).is_none(),
            user_has_not_continued,
            user_interactive_origin: block.origin == crate::types::CommandOrigin::UserInteractive,
            budget_available: !state.insight_budget.is_suppressed(
                &candidate.suppression_key,
                candidate.severity,
                block.ended_at_ms,
            ),
        };
        if !matches!(
            failed_command_intervention(events, block, &candidate, analysis_mode, gates),
            InterventionDecision::AutoAnalyze { .. }
        ) {
            continue;
        }
        if state.insight_budget.should_suppress(
            candidate.suppression_key,
            candidate.severity,
            block.ended_at_ms,
        ) {
            continue;
        }
        // Auto starts from the same precmd batch, whose native prompt is already cached.
        state.agent_run.native_prompt_after_run = true;
        start_agent_for_block(
            block,
            &ledger.blocks,
            &findings,
            adapter,
            state,
            output,
            FailedCommandAgentStartOptions {
                selectable_after_event_index: block_end_event_index(events, block)
                    .map(|idx| history_index_base.saturating_add(idx)),
                trigger: FailedCommandAnalysisTrigger::Auto,
            },
        )?;
        output.flush()?;
    }

    collect_failed_command_insights_with_history_base(
        events,
        &ledger.blocks,
        state,
        output,
        history_index_base,
        event_index_base,
    )?;
    if pending_card_capture(state).is_none() {
        render_pending_command_insight(state, output)?;
    } else {
        state.pending_command_insight = None;
    }

    render_post_failure_actions(
        action_events,
        &ledger.blocks,
        &findings,
        adapter,
        state,
        output,
        event_index_base,
    )?;

    render_selection_actions(action_events, state, output, event_index_base)?;
    flush_held_agent_events(state, output)?;
    if !shell_busy && !state.control.shell_handoff().has_active_handoff() {
        poll_active_agent_run(state, output, adapter)?;
    }
    flush_held_agent_events(state, output)?;
    // Shell-evidence recovery is scheduled only after this batch's final agent
    // poll. Claiming it earlier sees a run that is about to finish inside that
    // poll as still active, which skips the pending continuation for this batch;
    // it would then only be picked up if some later shell event triggered
    // another one.
    start_pending_shell_handoff_continuations(adapter, state, output)?;
    poll_background_compaction(state, output, adapter, false)?;
    render_soft_newline_tip(events, state, output)?;
    render_owned_shell_prompt(state, output)?;

    Ok(())
}

fn poll_inline_runtime_without_shell_events<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if state.shell_exited {
        stop_active_agent_run_without_rendering(state, output)?;
        return Ok(());
    }

    if state.control.shell_busy() {
        if let Some(cancellation) = state.personalization.analyzer_cancellation.as_ref() {
            cancellation.set_foreground_idle(false);
        }
        state.personalization.idle_since = None;
        poll_active_agent_run_deferred(state, output, adapter)?;
        poll_background_compaction(state, output, adapter, true)?;
        return Ok(());
    }

    render_startup_health_banner(state, output)?;
    render_pending_recommendation_notice(state, output)?;
    let personal_idle =
        state.agent_run.active.is_none() && !state.personalization.shell_input_active;
    crate::recommendation::personal_session::poll_personal_session(state, adapter, personal_idle);
    render_queued_hook_consultation(state, output)?;
    if pending_card_capture(state).is_none() {
        render_pending_command_insight(state, output)?;
    } else {
        state.pending_command_insight = None;
    }
    flush_held_agent_events(state, output)?;
    if !state.control.shell_handoff().has_active_handoff() {
        poll_active_agent_run(state, output, adapter)?;
    }
    flush_held_agent_events(state, output)?;
    start_pending_shell_handoff_continuations(adapter, state, output)?;
    poll_background_compaction(state, output, adapter, false)?;
    render_owned_shell_prompt(state, output)
}

/// Starts the shell-evidence continuation whose delivery to the owning provider
/// run failed. Reuses the existing `PendingRecovery` claim, so a given approval
/// recovers at most once.
///
/// Claiming is a one-way move (`PendingRecovery` -> `RecoveryQueued`, plus a
/// dedup entry keyed by approval id), so it must not happen unless the run can
/// actually start: a pending or active compaction makes
/// `start_agent_run_with_origin` drop this `InternalBestEffort` request, which
/// would leave the evidence claimed and unrecoverable. Hence the gate check
/// before claiming, and — because starting a run polls the provider and can
/// itself surface a compaction recommendation — at most one recovery per
/// boundary. Any remaining recoveries are claimed at the next idle boundary.
fn start_pending_shell_handoff_continuations<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if state.agent_run.active.is_some()
        || pending_card_capture(state).is_some()
        || crate::slash::session::compaction_pending_or_active(state)
    {
        return Ok(());
    }
    for (request, origin) in shell_handoff_continuation_requests(state) {
        // Shell-handoff continuations are automatic conversation resumptions,
        // not fresh user requests.
        start_agent_run_with_origin(
            &request,
            origin,
            AgentStartIntent::InternalBestEffort,
            adapter,
            state,
            output,
            None,
        )?;
    }
    Ok(())
}

pub(super) fn update_personal_shell_input_state(events: &[ShellEvent], state: &mut InlineState) {
    for event in events {
        match event.kind {
            ShellEventKind::ShellReady
            | ShellEventKind::CommandStarted
            | ShellEventKind::CommandCompleted
            | ShellEventKind::CommandFailed => state.personalization.shell_input_active = false,
            ShellEventKind::UserInputIntercepted
                if event.component.as_deref() == Some("shell_input") =>
            {
                state.personalization.shell_input_active =
                    event.message.as_deref() != Some("input empty");
            }
            _ => {}
        }
    }
}

fn update_soft_newline_tip_state(events: &[ShellEvent], state: &mut InlineState) {
    // #1932 F5: remember a straight-to-bash multi-line paste for the
    // failure-insight hint; consumed by render_pending_command_insight.
    if events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("multiline_paste")
    }) {
        state.prompt_entry_hints.multiline_paste_observed = true;
    }
    if state.prompt_entry_hints.shown_soft_newline_tip {
        return;
    }
    let observed = events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("soft_newline_shortcut")
    });
    if observed {
        state.prompt_entry_hints.pending_soft_newline_tip = true;
    }
}

/// One-time discoverability tip (#1721 T-c): a soft-newline shortcut was
/// pressed on the bash-owned input path where it cannot take effect. Rendered
/// only at a prompt-ready boundary; output-side only, never touches input
/// relaying or marker timing.
fn render_soft_newline_tip<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if !state.prompt_entry_hints.pending_soft_newline_tip
        || state.prompt_entry_hints.shown_soft_newline_tip
    {
        return Ok(());
    }
    if !events
        .iter()
        .any(|event| event.kind == ShellEventKind::ShellReady)
    {
        return Ok(());
    }
    // D12: never interleave the tip with an in-progress draft; only render
    // at a quiet prompt boundary.
    if state.personalization.shell_input_active {
        return Ok(());
    }
    let tip = state.i18n().t(MessageId::PromptSoftNewlineTip);
    // The cursor may sit anywhere on the echoed input line: move to a
    // fresh line before the tip and repaint the prompt afterwards so the
    // tip never splices into user input (#1932).
    write!(output, "\r\n\x1b[2m{tip}\x1b[0m\r\n")?;
    output.flush()?;
    state.trigger_pty_prompt = true;
    state.prompt_entry_hints.pending_soft_newline_tip = false;
    state.prompt_entry_hints.shown_soft_newline_tip = true;
    Ok(())
}

fn render_owned_shell_prompt<W: Write>(
    state: &mut InlineState,
    _output: &mut W,
) -> std::io::Result<()> {
    if state.agent_run.active.is_some()
        || state.shell_exited
        || pending_card_capture(state).is_some()
    {
        return Ok(());
    }

    if !state.agent_run.needs_prompt_after_run {
        state.agent_run.native_prompt_after_run = false;
        return Ok(());
    }

    if state.agent_run.native_prompt_after_run {
        state.agent_run.needs_prompt_after_run = false;
        state.agent_run.native_prompt_after_run = false;
        return Ok(());
    }

    // PromptPresentation owns the out-of-band input marker. Even an isolated
    // shell must restore through the PTY relay instead of writing PS1 here,
    // otherwise this synthetic repaint bypasses that single presentation
    // owner and leaves one editable prompt undecorated.
    state.trigger_pty_prompt = true;
    state.agent_run.needs_prompt_after_run = false;
    Ok(())
}

impl QuestionConsumer {
    pub(crate) fn consume<W: Write>(
        events: &[ShellEvent],
        adapter: &AdapterInstance,
        state: &mut InlineState,
        output: &mut W,
        event_index_base: usize,
    ) -> std::io::Result<Vec<RuntimeAction>> {
        render_question_focus_actions(events, state, output, event_index_base)?;
        render_question_toggle_actions(events, state, output, event_index_base)?;
        render_question_input_actions(events, state, output, event_index_base)?;
        render_question_cancel_actions(events, state, output, event_index_base)?;
        render_question_answer_actions(events, adapter, state, output, event_index_base)?;
        Ok(Vec::new())
    }
}

impl SlashConsumer {
    pub(crate) fn consume<W: Write>(
        events: &[ShellEvent],
        blocks: &[CommandBlock],
        adapter: &AdapterInstance,
        state: &mut InlineState,
        output: &mut W,
        event_index_base: usize,
    ) -> std::io::Result<Vec<RuntimeAction>> {
        render_slash_actions(events, blocks, adapter, state, output, event_index_base)?;
        Ok(Vec::new())
    }
}

impl ApprovalConsumer {
    pub(crate) fn consume<W: Write>(
        events: &[ShellEvent],
        blocks: &[CommandBlock],
        adapter: &AdapterInstance,
        state: &mut InlineState,
        output: &mut W,
        event_index_base: usize,
    ) -> std::io::Result<Vec<RuntimeAction>> {
        render_approval_actions(events, blocks, adapter, state, output, event_index_base)?;
        Ok(Vec::new())
    }
}

impl EvidenceRequestConsumer {
    pub(crate) fn consume<W: Write>(
        events: &[ShellEvent],
        blocks: &[CommandBlock],
        adapter: &AdapterInstance,
        state: &mut InlineState,
        output: &mut W,
        event_index_base: usize,
    ) -> std::io::Result<Vec<RuntimeAction>> {
        render_evidence_request_actions(events, blocks, adapter, state, output, event_index_base)?;
        Ok(Vec::new())
    }
}

impl ActivityConsumer {
    pub(crate) fn consume<W: Write>(
        events: &[ShellEvent],
        blocks: &[CommandBlock],
        state: &mut InlineState,
        output: &mut W,
    ) -> std::io::Result<Vec<RuntimeAction>> {
        let mut handoff_activity_ids = record_approved_shell_handoff_blocks(state, blocks);
        // Fallback: close emitted handoffs that reached a prompt boundary
        // without ever producing command tracking (lost preexec marker).
        handoff_activity_ids.extend(close_untracked_shell_handoffs(state, events));
        render_activity_rows(state, &handoff_activity_ids, output)?;
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod recovery_tests;
