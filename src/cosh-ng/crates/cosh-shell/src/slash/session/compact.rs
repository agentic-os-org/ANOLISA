//! Background session compaction: `/session compact [status|cancel]`.
//!
//! Compaction runs in a separate `cosh-core --compact` process so the shell
//! prompt returns immediately. The runtime state here is intentionally
//! independent of the selected/restoring/active session recovery machine;
//! recovery transitions never mutate compaction state and vice versa.
//!
//! Automatic compaction reuses this exact path: cosh-core only emits a
//! `compaction_recommended_v1` status at idle boundaries, and the shell starts
//! the same background compactor it uses for `/session compact` — bound to the
//! exact session and context revision the recommendation names.
//!
//! Module layout:
//! - this file: command dispatch, target resolution, and result rendering;
//! - [`runtime`]: runtime state, pending-auto handling, completion polling;
//! - [`process`]: compactor spawn, child lifecycle, stdout parsing.

mod process;
mod runtime;

#[cfg(test)]
mod tests;

use crate::adapter::CoshCoreAdapter;
use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

use super::panel::{core_adapter, render_unavailable, session_management_idle, workspace_scope};

use self::process::{
    bounded, spawn_compactor, CompactionKind, CompactionOrigin, CompactionOutcome,
    TerminationReason,
};
use self::runtime::CompactionCompletion;
pub(crate) use self::runtime::CompactionRuntime;

/// Whether Agent conversation is currently paused by a background compaction.
pub(crate) fn compaction_active(state: &InlineState) -> bool {
    state.control.session().compaction().is_active()
}

/// Whether a background compaction is running, recommended and about to start
/// at the next idle boundary, or finished-but-not-yet-rendered.
///
/// The Agent start gate uses this so a recommended-but-not-yet-running
/// compaction is not starved by internal continuations, user requests arriving
/// in that window are queued rather than started ahead of it, and a finished
/// compaction's completion notice / suppression marker / FIFO resume are all
/// applied at a safe boundary before any new model request runs.
pub(crate) fn compaction_pending_or_active(state: &InlineState) -> bool {
    let compaction = state.control.session().compaction();
    compaction.is_active() || compaction.has_pending_auto() || compaction.has_pending_completion()
}

/// Records a `compaction_recommended_v1` status payload from cosh-core.
///
/// The payload is everything after the `compaction_recommended_v1:` prefix:
/// `<session-id>:<generation>:<revision>:<history>:<usable>`. Malformed
/// payloads are ignored (fail closed) so a corrupt status can never trigger a
/// compaction against the wrong session.
pub(crate) fn note_compaction_recommendation(state: &mut InlineState, payload: &str) {
    state
        .control
        .session_mut()
        .compaction_mut()
        .note_recommendation(payload);
}

/// Renders the actionable "Agent paused during compaction" notice.
pub(crate) fn render_compaction_paused_notice<W: Write>(
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    render_notice_panel(
        output,
        state.i18n().t(MessageId::SessionCompactAgentPausedTitle),
        state
            .i18n()
            .t(MessageId::SessionCompactAgentPausedBody)
            .lines()
            .map(ToOwned::to_owned)
            .collect(),
        None,
    )
}

/// Renders the "pending-request queue is full" notice.
///
/// The compaction-paused title is only used when a compaction is actually
/// pausing the Agent; a queue that filled behind an ordinary busy Agent uses
/// the generic queued title so the user is not falsely told a compaction is
/// running.
pub(crate) fn render_agent_queue_full_notice<W: Write>(
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let title = if compaction_pending_or_active(state) {
        state.i18n().t(MessageId::SessionCompactAgentPausedTitle)
    } else {
        state.i18n().t(MessageId::AgentQueueFullTitle)
    };
    render_notice_panel(
        output,
        title,
        vec![state
            .i18n()
            .t(MessageId::SessionCompactQueueFullBody)
            .to_string()],
        None,
    )
}

/// Renders the notice shown when a control response cannot be admitted.
///
/// Shown *before* any question/approval card state is consumed: the card
/// stays pending, so unlike the generic queue-full notice this explicitly
/// tells the user their answer can simply be retried.
pub(crate) fn render_control_queue_full_notice<W: Write>(
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    render_notice_panel(
        output,
        state.i18n().t(MessageId::AgentQueueFullTitle),
        vec![state
            .i18n()
            .t(MessageId::AgentControlQueueFullBody)
            .to_string()],
        None,
    )
}

/// Dispatches `/session compact [status|cancel]`.
pub(crate) fn render_session_compact_command<W: Write>(
    sub: Option<&str>,
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    match sub {
        None => start_compaction(blocks, adapter, state, output),
        Some("status") => render_compaction_status(state, output),
        Some("cancel") => cancel_compaction(state, output),
        Some(_) => render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionErrorTitle),
            vec![state.i18n().t(MessageId::SessionUsageBody).to_string()],
            None,
        ),
    }
}

/// Resolves the compactable session and its owning workspace scope.
///
/// The workspace is paired with the session that owns it so a user `cd` into
/// another workspace cannot query a stale session ID in the wrong scope.
fn compact_target(core: &CoshCoreAdapter) -> Option<(String, Option<String>)> {
    let session = core
        .session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(session_id) = session.active_session_id() {
        let workspace = session.active_workspace_scope().map(str::to_string);
        return Some((session_id.to_string(), workspace));
    }
    let recovery = &session.recovery;
    if matches!(
        recovery.state,
        crate::adapter::SessionRecoveryState::Selected
    ) {
        if let Some(session_id) = recovery.selected_session_id.clone() {
            return Some((session_id, recovery.selected_workspace_scope.clone()));
        }
    }
    None
}

fn start_compaction<W: Write>(
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(core) = core_adapter(adapter) else {
        return render_unavailable(state, output);
    };
    // A running compactor, a finished-but-unrendered completion, and a
    // recommended automatic attempt all make a fresh manual start a duplicate
    // of work that is already in flight.
    if compaction_pending_or_active(state) {
        return render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionCompactTitle),
            vec![state
                .i18n()
                .t(MessageId::SessionCompactDuplicateBody)
                .to_string()],
            None,
        );
    }
    if !session_management_idle(state) {
        return render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionErrorTitle),
            vec![state.i18n().t(MessageId::SessionBusyBody).to_string()],
            None,
        );
    }
    // MVP scope: only the active or explicitly selected resumable
    // cosh-core session may be compacted.
    let Some((session_id, session_workspace)) = compact_target(core) else {
        return render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionCompactTitle),
            vec![state
                .i18n()
                .t(MessageId::SessionCompactNoSessionBody)
                .to_string()],
            None,
        );
    };
    let workspace = session_workspace.unwrap_or_else(|| workspace_scope(blocks));
    // A manual `/session compact` carries no expected revision: it targets the
    // current session state as-is and reports the manual trigger.
    match spawn_compactor(
        &core.program,
        &workspace,
        &session_id,
        CompactionKind::Manual,
    ) {
        Ok(active) => {
            let body = state
                .i18n()
                .format(MessageId::SessionCompactStartedBody, &[("id", &session_id)])
                .lines()
                .map(ToOwned::to_owned)
                .collect();
            state.control.session_mut().compaction_mut().active = Some(active);
            render_notice_panel(
                output,
                state.i18n().t(MessageId::SessionCompactTitle),
                body,
                Some(state.i18n().t(MessageId::SessionCompactFooter)),
            )
        }
        Err(error) => render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionErrorTitle),
            vec![state.i18n().format(
                MessageId::SessionCompactSpawnFailedBody,
                &[("error", &bounded(&error.to_string()))],
            )],
            None,
        ),
    }
}

fn render_compaction_status<W: Write>(state: &InlineState, output: &mut W) -> std::io::Result<()> {
    let compaction = state.control.session().compaction();
    let body = match compaction.active.as_ref() {
        Some(active) => {
            let run_state = format!(
                "{} ({})",
                if active.cancel_requested() {
                    "cancelling"
                } else {
                    "running"
                },
                active.origin.label()
            );
            vec![
                state.i18n().format(
                    MessageId::SessionCompactStatusSessionLine,
                    &[("id", &active.session_id)],
                ),
                state.i18n().format(
                    MessageId::SessionCompactStatusRunningLine,
                    &[
                        ("state", run_state.as_str()),
                        (
                            "elapsed",
                            &active.started_at.elapsed().as_secs().to_string(),
                        ),
                    ],
                ),
                state.i18n().format(
                    MessageId::SessionWorkspaceLine,
                    &[("workspace", &active.workspace_scope)],
                ),
                state
                    .i18n()
                    .t(MessageId::SessionCompactAgentPausedBody)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            ]
        }
        // A harvested-but-unrendered completion still pauses the Agent; the
        // status must not claim idle before the result was shown.
        None if compaction.has_pending_completion() => vec![state
            .i18n()
            .t(MessageId::SessionCompactStatusPendingRenderBody)
            .to_string()],
        None if compaction.has_pending_auto() => vec![state
            .i18n()
            .t(MessageId::SessionCompactStatusRecommendedBody)
            .to_string()],
        None => vec![state
            .i18n()
            .t(MessageId::SessionCompactStatusIdleBody)
            .to_string()],
    };
    render_notice_panel(
        output,
        state.i18n().t(MessageId::SessionCompactStatusTitle),
        body,
        Some(state.i18n().t(MessageId::SessionCompactFooter)),
    )
}

fn cancel_compaction<W: Write>(state: &mut InlineState, output: &mut W) -> std::io::Result<()> {
    let compaction = state.control.session_mut().compaction_mut();
    // A recommendation that has not started yet is cancelled in the same
    // pass: taking it atomically records its suppression marker and releases
    // the Agent compaction gate, so the same session + generation + revision
    // cannot retrigger while a new revision still can. Any user requests
    // queued behind the gate resume at the next safe-boundary poll.
    let pending_cancelled = compaction.cancel_pending_auto();
    if let Some(active) = compaction.active.as_mut() {
        // Full cancellation path: SIGTERM the process group now; the
        // completion poll escalates to SIGKILL after the grace period,
        // reaps the child, and renders the cancelled completion.
        active.request_termination(TerminationReason::UserCancel);
        return render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionCompactTitle),
            vec![state
                .i18n()
                .t(MessageId::SessionCompactCancelRequestedBody)
                .to_string()],
            None,
        );
    }
    if pending_cancelled {
        // No compactor process exists yet, so nothing is terminated — the
        // notice must say the recommendation was withdrawn, not that a
        // process is being killed.
        return render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionCompactTitle),
            vec![state
                .i18n()
                .t(MessageId::SessionCompactPendingCancelledBody)
                .to_string()],
            None,
        );
    }
    render_notice_panel(
        output,
        state.i18n().t(MessageId::SessionCompactTitle),
        vec![state
            .i18n()
            .t(MessageId::SessionCompactNotRunningBody)
            .to_string()],
        None,
    )
}

/// Harvests compactor results and drives pending automatic attempts.
///
/// While a foreground command is producing output (`shell_busy`) or an Agent
/// run is rendering, completions stay queued and automatic attempts stay
/// pending, so background text never interleaves with foreground output. When
/// idle, a finished compaction is rendered, a recommended one is started, and
/// any explicit user request held back for a compaction is resumed.
pub(crate) fn poll_background_compaction<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    adapter: &AdapterInstance,
    shell_busy: bool,
) -> std::io::Result<()> {
    state.control.session_mut().compaction_mut().poll();
    if shell_busy || state.agent_run.active.is_some() {
        return Ok(());
    }
    if let Some(completion) = state
        .control
        .session_mut()
        .compaction_mut()
        .take_completion()
    {
        // Suppress retriggering for the same context revision after an
        // automatic attempt fails or is cancelled. This *adds* the revision to
        // the suppressed set rather than replacing it: a `/session compact
        // cancel` that suppressed a newer pending recommendation before
        // terminating this compactor must keep that suppression intact, so the
        // cancelled pending revision cannot re-arm the gate when re-emitted.
        if completion.origin == CompactionOrigin::Auto
            && (completion.cancelled
                || matches!(completion.outcome, CompactionOutcome::Failed { .. }))
        {
            if let Some(marker) = completion.revision_marker.clone() {
                state
                    .control
                    .session_mut()
                    .compaction_mut()
                    .suppress_auto_marker(marker);
            }
        }
        render_completion(&completion, state, output)?;
        crate::slash::prompt::write_shell_prompt(state, output)?;
        output.flush()?;
    }
    runtime::maybe_start_pending_auto(state, adapter, output)?;
    runtime::resume_queued_user_request_after_compaction(state, adapter, output)?;
    crate::agent::turn_extension::activate_pending_turn_extension(state, output).map(|_| ())
}

fn render_completion<W: Write>(
    completion: &CompactionCompletion,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    match &completion.outcome {
        CompactionOutcome::Committed {
            tokens_before,
            tokens_after,
            after_source,
        } => render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionCompactCompletedTitle),
            vec![
                state.i18n().format(
                    MessageId::SessionCompactCompletedBody,
                    &[
                        ("before", &tokens_before.to_string()),
                        ("after", &tokens_after.to_string()),
                        ("source", after_source),
                    ],
                ),
                state
                    .i18n()
                    .t(MessageId::SessionCompactCompletedRetainedBody)
                    .to_string(),
            ],
            None,
        ),
        CompactionOutcome::Failed { .. } if completion.cancelled => render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionCompactCancelledTitle),
            vec![state
                .i18n()
                .t(MessageId::SessionCompactCancelledBody)
                .to_string()],
            None,
        ),
        CompactionOutcome::Failed { code, message } => render_notice_panel(
            output,
            state.i18n().t(MessageId::SessionCompactFailedTitle),
            vec![
                state.i18n().format(
                    MessageId::SessionCompactFailedBody,
                    &[("code", code.as_str()), ("message", &bounded(message))],
                ),
                state
                    .i18n()
                    .t(MessageId::SessionCompactFailedTranscriptBody)
                    .to_string(),
            ],
            None,
        ),
    }
}
