//! Compaction runtime state: pending automatic attempts and completion polling.

use std::time::Instant;

use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;
use crate::slash::prompt::write_shell_prompt;

use super::super::panel::{core_adapter, session_interaction_idle};
use super::process::{
    bounded, spawn_compactor, ActiveCompaction, CompactionKind, CompactionOrigin,
    CompactionOutcome, SuppressionMarker, TerminationReason,
};

/// Upper bound on retained suppression markers (see
/// [`CompactionRuntime::suppressed_auto_markers`]). Only a handful are ever
/// live at once — an active compactor plus a newer pending recommendation —
/// so this is a generous backstop against unbounded growth over a long
/// session, not a functional limit.
const MAX_SUPPRESSED_AUTO_MARKERS: usize = 16;

/// Automatic trigger recommendation reported by cosh-core.
///
/// Bound to the exact session and context revision the recommendation was
/// emitted for; the shell refuses to compact any other session.
pub(super) struct AutoCompactionRequest {
    /// Session the recommendation names (canonical UUID).
    session_id: String,
    /// Session store generation captured at the trigger boundary.
    generation: u64,
    /// Projection revision captured at the trigger boundary.
    projection_revision: u64,
    /// History tokens reported at the trigger boundary.
    history_tokens: u64,
    /// Usable history budget reported at the trigger boundary.
    usable_history: u64,
}

impl AutoCompactionRequest {
    /// Per-revision suppression identity, scoped to this session so a failure
    /// here cannot suppress the same `generation:revision` on another session.
    fn suppression_marker(&self) -> SuppressionMarker {
        SuppressionMarker {
            session_id: self.session_id.clone(),
            generation: self.generation,
            projection_revision: self.projection_revision,
        }
    }
}

pub(super) struct CompactionCompletion {
    pub(super) cancelled: bool,
    pub(super) origin: CompactionOrigin,
    pub(super) revision_marker: Option<SuppressionMarker>,
    pub(super) outcome: CompactionOutcome,
}

/// Slash-owned background compaction runtime.
#[derive(Default)]
pub(crate) struct CompactionRuntime {
    pub(super) active: Option<ActiveCompaction>,
    pending_completion: Option<CompactionCompletion>,
    pending_auto: Option<AutoCompactionRequest>,
    /// Per-session revision markers suppressed after an automatic attempt was
    /// cancelled, failed, or failed to spawn: a re-emitted recommendation for
    /// the same session + generation + revision is dropped so it cannot
    /// retrigger a loop (a different session, generation, or revision still
    /// may).
    ///
    /// This is a set, not a single slot, because an active compactor and a
    /// newer pending recommendation can both need suppression at once: one
    /// `/session compact cancel` suppresses the pending revision immediately,
    /// and the active compactor's later cancelled completion must *add* its own
    /// revision without clobbering the pending one — otherwise the cancelled
    /// pending revision would re-arm the gate the moment it was re-emitted.
    /// Bounded by [`MAX_SUPPRESSED_AUTO_MARKERS`]; the oldest revision is
    /// evicted first.
    pub(super) suppressed_auto_markers: Vec<SuppressionMarker>,
}

impl CompactionRuntime {
    /// Whether a background compactor process is running.
    pub(crate) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Whether an automatic recommendation is waiting to start.
    pub(crate) fn has_pending_auto(&self) -> bool {
        self.pending_auto.is_some()
    }

    /// Whether a finished compaction has been harvested but its completion
    /// notice has not yet been rendered at a safe boundary.
    ///
    /// `poll` may reap a finished compactor while a foreground command is busy,
    /// clearing `active` but leaving the result queued here. The Agent stays
    /// paused across that window so the completion notice, suppression marker,
    /// and FIFO user-queue resume are all applied before any new model request.
    pub(crate) fn has_pending_completion(&self) -> bool {
        self.pending_completion.is_some()
    }

    /// Records `marker` as suppressed so a re-emitted recommendation for the
    /// same session + generation + revision is dropped.
    ///
    /// Idempotent (a repeated marker is not stored twice) and bounded by
    /// [`MAX_SUPPRESSED_AUTO_MARKERS`]: once the cap is reached the oldest
    /// suppressed revision is evicted, so a long-lived session cannot grow the
    /// set without limit.
    pub(super) fn suppress_auto_marker(&mut self, marker: SuppressionMarker) {
        if self.suppressed_auto_markers.contains(&marker) {
            return;
        }
        if self.suppressed_auto_markers.len() >= MAX_SUPPRESSED_AUTO_MARKERS {
            self.suppressed_auto_markers.remove(0);
        }
        self.suppressed_auto_markers.push(marker);
    }

    /// Whether `marker` is currently suppressed.
    pub(super) fn is_auto_marker_suppressed(&self, marker: &SuppressionMarker) -> bool {
        self.suppressed_auto_markers.contains(marker)
    }

    /// Records a strictly validated `compaction_recommended_v1` payload.
    ///
    /// Payload (already stripped of its prefix):
    /// `<session-id>:<generation>:<revision>:<history>:<usable>`. Every field
    /// is validated — exactly five fields, a canonical UUID session id, and
    /// four `u64` values — and anything malformed is dropped (fail closed) so
    /// a corrupt status can never bind a compaction to the wrong session.
    ///
    /// A payload naming the currently suppressed session + generation +
    /// revision is also dropped: a cancelled or failed automatic attempt must
    /// not re-arm the compaction gate for the same context revision.
    pub(crate) fn note_recommendation(&mut self, payload: &str) {
        let fields: Vec<&str> = payload.split(':').collect();
        if fields.len() != 5 {
            return;
        }
        let session_id = fields[0];
        if !is_valid_session_id(session_id) {
            return;
        }
        let (Ok(generation), Ok(projection_revision), Ok(history_tokens), Ok(usable_history)) = (
            fields[1].parse::<u64>(),
            fields[2].parse::<u64>(),
            fields[3].parse::<u64>(),
            fields[4].parse::<u64>(),
        ) else {
            return;
        };
        let request = AutoCompactionRequest {
            session_id: session_id.to_string(),
            generation,
            projection_revision,
            history_tokens,
            usable_history,
        };
        if self.is_auto_marker_suppressed(&request.suppression_marker()) {
            return;
        }
        self.pending_auto = Some(request);
    }

    /// Atomically cancels a recommendation that has not started yet.
    ///
    /// Takes the pending request and records its suppression marker in one
    /// step, so the same session + generation + revision can neither start at
    /// the next idle boundary nor re-arm through a re-emitted status. Returns
    /// `false` when nothing was pending. No process exists at this point, so
    /// there is nothing to signal or reap.
    pub(super) fn cancel_pending_auto(&mut self) -> bool {
        let Some(request) = self.pending_auto.take() else {
            return false;
        };
        self.suppress_auto_marker(request.suppression_marker());
        true
    }

    /// Harvests a finished compactor without rendering anything, and drives
    /// the deadline / cancellation state machine while one is still running.
    ///
    /// The child is reaped here, on the owning thread, while the handle is
    /// still exclusively held; no signal can ever target a recycled PID. A
    /// disconnected result channel (reader thread gone without a value) is
    /// turned into a typed failure and the child is still reaped.
    ///
    /// Liveness: every path converges on exactly one terminal completion —
    /// a normal result, a transport failure, or a `timeout`/`cancelled`
    /// failure once termination was requested. `active` is taken exactly
    /// once, so the completion can never be processed twice, and a compactor
    /// that ignores every signal until SIGKILL still cannot keep the
    /// compaction active past the grace period.
    pub(crate) fn poll(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let outcome = match active.receiver.try_recv() {
            // A structured result that arrives during the termination grace
            // window is still accepted: the compactor did finish, and
            // reporting what it actually did (committed or a typed engine
            // failure) is more truthful than a synthesized timeout. The
            // reader's own transport failure is different: after a SIGTERM it
            // is just the EOF of the process we terminated, so it is
            // reclassified under the active termination reason — otherwise
            // the common "deadline hit, child exited on SIGTERM" path would
            // masquerade as `transport` instead of the promised `timeout`.
            Ok(CompactionOutcome::Failed { code, message }) if code == "transport" => {
                let failure = match termination_failure(active) {
                    Some(reclassified) => reclassified,
                    None => CompactionOutcome::Failed { code, message },
                };
                with_stderr_context(failure, &active.stderr_tail.snapshot())
            }
            Ok(outcome) => outcome,
            // A disconnected reader after SIGTERM is likewise the terminated
            // process going away, not an independent transport fault.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                let failure = match termination_failure(active) {
                    Some(reclassified) => reclassified,
                    None => CompactionOutcome::Failed {
                        code: "transport".to_string(),
                        message: "compactor reader disconnected before reporting a result"
                            .to_string(),
                    },
                };
                with_stderr_context(failure, &active.stderr_tail.snapshot())
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                let now = Instant::now();
                match active.termination {
                    Some(termination) if now >= termination.kill_at => {
                        // Grace expired: escalate to SIGKILL below (the reap
                        // helper kills the whole group) and synthesize the
                        // typed terminal failure for this termination reason.
                        let failure = termination_failure(active)
                            .expect("termination checked above; reason is present");
                        with_stderr_context(failure, &active.stderr_tail.snapshot())
                    }
                    Some(_) => return, // waiting out the grace period
                    None if now >= active.deadline => {
                        // Deadline reached: SIGTERM the group and give it one
                        // grace period to exit (or deliver a late result).
                        active.request_termination(TerminationReason::DeadlineExceeded);
                        return;
                    }
                    None => return,
                }
            }
        };
        let active = self.active.take().expect("checked above");
        active.terminate_and_reap();
        self.pending_completion = Some(CompactionCompletion {
            cancelled: active.cancel_requested(),
            origin: active.origin,
            revision_marker: active.revision_marker.clone(),
            outcome,
        });
    }

    pub(super) fn take_completion(&mut self) -> Option<CompactionCompletion> {
        self.pending_completion.take()
    }
}

/// Builds the typed failure for an in-flight termination, or `None` when no
/// termination was requested.
///
/// Used both when the grace period expires with no result and when the
/// terminated child's exit surfaces as a reader transport failure or channel
/// disconnect — all of those are consequences of the SIGTERM, so they carry
/// the termination reason (`timeout` / `cancelled`), never `transport`.
fn termination_failure(active: &ActiveCompaction) -> Option<CompactionOutcome> {
    let termination = active.termination.as_ref()?;
    let elapsed = active.started_at.elapsed().as_secs();
    Some(match termination.reason {
        TerminationReason::UserCancel => CompactionOutcome::Failed {
            code: "cancelled".to_string(),
            message: "compactor was terminated after the cancellation request".to_string(),
        },
        TerminationReason::DeadlineExceeded => CompactionOutcome::Failed {
            code: "timeout".to_string(),
            message: format!(
                "compactor exceeded its execution deadline after {elapsed}s and was terminated"
            ),
        },
    })
}

/// Appends the bounded, control-character-free stderr tail to a synthesized
/// failure so abnormal exits keep a diagnostic trace.
///
/// Only failures the shell itself synthesizes (transport, timeout, kill
/// escalation) carry the tail; structured failures from the cosh-core
/// envelope already contain their own message. The final panel text is
/// additionally truncated to [`super::process::MAX_REPORTED_ERROR_CHARS`] by
/// `bounded`, so a noisy tail can neither flood the panel nor leak beyond
/// the retained window.
fn with_stderr_context(outcome: CompactionOutcome, stderr_tail: &str) -> CompactionOutcome {
    let trimmed = stderr_tail.trim();
    if trimmed.is_empty() {
        return outcome;
    }
    match outcome {
        CompactionOutcome::Failed { code, message } => CompactionOutcome::Failed {
            code,
            message: format!("{message}; stderr tail: {trimmed}"),
        },
        committed => committed,
    }
}

/// Starts a recommended automatic compaction at a safe idle boundary.
///
/// The recommendation is honored only for the exact session it named: if the
/// active/selected session changed since it was emitted, it is discarded
/// rather than compacting a different conversation. Spawn failures are shown
/// to the user here (a safe prompt boundary), not just logged.
pub(super) fn maybe_start_pending_auto<W: Write>(
    state: &mut InlineState,
    adapter: &AdapterInstance,
    output: &mut W,
) -> std::io::Result<()> {
    if state.control.session().compaction().pending_auto.is_none() {
        return Ok(());
    }
    if state.control.session().compaction().is_active() {
        // Already compacting; the stale recommendation is obsolete.
        state.control.session_mut().compaction_mut().pending_auto = None;
        return Ok(());
    }
    // Auto-start idle gate: this path owns the pending recommendation, so it
    // must not use the full session-mutation gate (which blocks on
    // pending_auto and would deadlock itself). It still refuses to start over
    // a finished-but-unrendered completion or any pending interaction.
    if state
        .control
        .session()
        .compaction()
        .has_pending_completion()
        || !session_interaction_idle(state)
    {
        // Conflicting interaction in progress; retry at the next boundary.
        return Ok(());
    }
    let Some(core) = core_adapter(adapter) else {
        state.control.session_mut().compaction_mut().pending_auto = None;
        return Ok(());
    };
    // Take the recommendation; from here it is either acted on or discarded.
    let Some(request) = state
        .control
        .session_mut()
        .compaction_mut()
        .pending_auto
        .take()
    else {
        return Ok(());
    };
    let marker = request.suppression_marker();
    if state
        .control
        .session()
        .compaction()
        .is_auto_marker_suppressed(&marker)
    {
        return Ok(());
    }
    let Some((session_id, session_workspace)) = super::compact_target(core) else {
        // No compactable session now; the recommendation is stale.
        return Ok(());
    };
    if session_id != request.session_id {
        // The active/selected session changed since the recommendation was
        // emitted; never compact a different session than the one it named.
        return Ok(());
    }
    let Some(workspace) = session_workspace else {
        // Without the recommendation session's own workspace we would have to
        // guess from cwd; discard rather than compact in the wrong scope.
        return Ok(());
    };
    match spawn_compactor(
        &core.program,
        &workspace,
        &session_id,
        CompactionKind::Auto {
            generation: request.generation,
            projection_revision: request.projection_revision,
        },
    ) {
        Ok(active) => {
            let percent = request
                .history_tokens
                .saturating_mul(100)
                .checked_div(request.usable_history)
                .unwrap_or(100);
            state.control.session_mut().compaction_mut().active = Some(active);
            render_notice_panel(
                output,
                state.i18n().t(MessageId::SessionCompactTitle),
                state
                    .i18n()
                    .format(
                        MessageId::SessionCompactAutoStartedBody,
                        &[
                            ("percent", percent.to_string().as_str()),
                            ("id", &session_id),
                        ],
                    )
                    .lines()
                    .map(ToOwned::to_owned)
                    .collect(),
                Some(state.i18n().t(MessageId::SessionCompactFooter)),
            )?;
            write_shell_prompt(state, output)?;
            output.flush()
        }
        Err(error) => {
            // Spawn failures suppress this revision to avoid loops, and are
            // surfaced to the user at this safe prompt boundary.
            state
                .control
                .session_mut()
                .compaction_mut()
                .suppress_auto_marker(marker);
            render_notice_panel(
                output,
                state.i18n().t(MessageId::SessionErrorTitle),
                vec![state.i18n().format(
                    MessageId::SessionCompactSpawnFailedBody,
                    &[("error", &bounded(&error.to_string()))],
                )],
                None,
            )?;
            write_shell_prompt(state, output)?;
            output.flush()?;
            tracing::warn!("automatic compactor spawn failed: {error}");
            Ok(())
        }
    }
}

/// Resumes an explicit user request held back for a background compaction.
///
/// While a compaction is recommended or running, `finish` keeps explicit user
/// requests queued (and drops stale internal ones). Once the context is stable
/// again — nothing active, running, or pending — the oldest held request is
/// started so the user's intent is never silently lost.
pub(super) fn resume_queued_user_request_after_compaction<W: Write>(
    state: &mut InlineState,
    adapter: &AdapterInstance,
    output: &mut W,
) -> std::io::Result<()> {
    if state.control.session().compaction().is_active()
        || state.control.session().compaction().has_pending_auto()
        || state
            .control
            .session()
            .compaction()
            .has_pending_completion()
        || state.agent_run.active.is_some()
    {
        return Ok(());
    }
    let Some(pending) = state.agent_run.queued_requests.pop_front() else {
        return Ok(());
    };
    // Preserve the stored admission class across the resume so a control
    // response re-queued behind a new recommendation is never downgraded.
    crate::agent::run::start_pending_agent_run(pending, adapter, state, output).map(|_| ())
}

/// Validates a canonical lowercase UUID as emitted by cosh-core session ids.
fn is_valid_session_id(value: &str) -> bool {
    if value.len() != 36 {
        return false;
    }
    value.bytes().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => byte == b'-',
        _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
    })
}
