//! Emergency context preflight: the fail-closed guard executed before every
//! provider request while the emergency threshold is crossed.

use std::io::Write;

use crate::config::CoreConfig;
use crate::protocol::OutputMessage;
use crate::provider::ContentGenerator;
use crate::provider::Message;

use super::boundary::{has_active_run, PreservationPolicy};
use super::budget::{estimate_messages_tokens, ContextBudget, ModelCapability};
use super::engine::{compact_in_memory, CompactionError};
use super::runtime::CompactionRuntime;

/// Stable prefix identifying typed context-limit turn failures.
pub(crate) const CONTEXT_LIMIT_ERROR_PREFIX: &str = "context_limit:";

/// Maximum compaction attempts one preflight may issue before yielding control.
///
/// Large transcripts can require several bounded summary inputs to reclaim a
/// safe prefix. Capping the work keeps one provider request from turning into
/// an unbounded sequence of summarizer calls; a retry resumes from the last
/// committed projection.
const MAX_EMERGENCY_COMPACTION_ATTEMPTS: usize = 16;

/// Why the emergency ladder could not bring the effective context back under
/// the emergency threshold.
///
/// Kept distinct from [`CompactionError`] so the final `context_limit:` message
/// names the real cause: a summarizer failure must never be reported as a
/// missing safe split.
#[derive(Debug)]
enum EmergencyFailure {
    /// No preservation policy produced a safe Agent-run split and nothing
    /// outside the projection is large enough to explain the overflow: the
    /// summary itself is what exceeds the threshold.
    NoSafeSplit,
    /// Every completed run is summarized and the remaining active, incomplete
    /// run alone exceeds the threshold. Only reported when the uncompactable
    /// tail was *measured* over the threshold, never inferred from the mere
    /// presence of an active run.
    ActiveRunTooLarge,
    /// Everything compactable is summarized, and what is left — the summary
    /// plus the still-verbatim tail — is over the threshold together, without
    /// either part exceeding it alone.
    IrreducibleRemainder,
    /// Defensive fallback when the ladder ends without a terminal or
    /// cut-specific failure while the reduced context is still over threshold.
    StillOverEmergency,
    /// Compaction kept making progress but reached the per-preflight attempt
    /// limit before the effective context crossed back under the threshold.
    AttemptLimitReached,
    /// One attempt failed for a reason unrelated to boundary selection:
    /// summarizer failure, empty summary, oversized input, or an exhausted
    /// revision clock.
    Attempt(CompactionError),
}

impl EmergencyFailure {
    /// Stable machine-readable code appended to the emergency status line.
    fn code(&self) -> &str {
        match self {
            Self::NoSafeSplit => "no_safe_split",
            Self::ActiveRunTooLarge => "active_run_too_large",
            Self::IrreducibleRemainder => "irreducible_remainder",
            Self::StillOverEmergency => "still_over_emergency",
            Self::AttemptLimitReached => "attempt_limit",
            Self::Attempt(error) => error.code(),
        }
    }

    /// Operator-facing explanation embedded in the `context_limit:` message.
    fn detail(&self) -> String {
        match self {
            Self::NoSafeSplit => "no safe Agent-run split was available, even after allowing the \
                 latest completed run to be summarized"
                .to_string(),
            Self::ActiveRunTooLarge => "every completed Agent run is already summarized and the \
                 active run alone exceeds the threshold"
                .to_string(),
            Self::IrreducibleRemainder => "every compactable Agent run is already summarized and \
                 the summary plus the remaining verbatim messages still exceed the threshold"
                .to_string(),
            Self::StillOverEmergency => {
                "compaction succeeded but the reduced context is still over the threshold"
                    .to_string()
            }
            Self::AttemptLimitReached => format!(
                "compaction made progress but reached the per-request limit of \
                 {MAX_EMERGENCY_COMPACTION_ATTEMPTS} compaction attempts"
            ),
            Self::Attempt(error) => error.to_string(),
        }
    }

    /// Recovery advice matched to the cause.
    ///
    /// The deepest ladder rung already applies manual-compaction semantics — the
    /// same preservation policy `/session compact` uses — so "compact manually"
    /// is only actionable for a *transient* attempt failure. A deterministic
    /// failure would repeat identically on a manual retry, and an exhausted
    /// ladder has no compactable history left, so both point at a new session.
    /// The attempt-limit case is different: it committed safe progress, so a
    /// retry resumes from a strictly later cut.
    fn recovery_hint(&self) -> &'static str {
        const DETERMINISTIC: &str =
            "start a new session, or raise the model context window if this model supports one";
        match self {
            Self::NoSafeSplit
            | Self::ActiveRunTooLarge
            | Self::IrreducibleRemainder
            | Self::StillOverEmergency => DETERMINISTIC,
            Self::AttemptLimitReached => {
                "retry to continue compaction from the last safe projection, or start a new session"
            }
            Self::Attempt(error) if attempt_is_transient(error) => {
                "retry, compact manually, or start a new session"
            }
            Self::Attempt(_) => DETERMINISTIC,
        }
    }
}

/// Whether re-running the same compaction could plausibly succeed.
///
/// Transient failures come from outside the transcript's shape — a provider or
/// store hiccup, a cancelled stream, or a concurrent session change — so a
/// retry or an explicit `/session compact` is worth suggesting. Everything else
/// is a deterministic property of this transcript and window: the deepest rung
/// already used manual semantics, so repeating it reproduces the same failure.
fn attempt_is_transient(error: &CompactionError) -> bool {
    match error {
        // Provider-side or store-side; a retry may behave differently.
        CompactionError::Provider(_)
        | CompactionError::EmptySummary
        | CompactionError::OversizedSummary
        | CompactionError::Cancelled
        | CompactionError::Conflict
        | CompactionError::DigestMismatch
        | CompactionError::Session(_) => true,
        // Deterministic for this transcript, window, or configuration: the
        // summary cannot pay for itself, the prefix cannot fit the summarizer
        // input, the revision clock is spent, the transcript is malformed, or
        // compaction is switched off. Manual compaction repeats the failure.
        CompactionError::NotReducing
        | CompactionError::OversizedInput
        | CompactionError::RevisionExhausted
        | CompactionError::Boundary(_)
        | CompactionError::NothingToCompact
        | CompactionError::Disabled => false,
    }
}

/// Ordered emergency fallbacks, from quality-first to last-resort.
///
/// The configured recent-run protection is tried first so an emergency that can
/// be resolved cheaply keeps the same verbatim history a normal automatic
/// compaction would. Only when that reclaims nothing does the ladder drop to one
/// protected run and finally to the manual-compaction semantics, which may
/// summarize the latest *completed* run. The active run is never a candidate at
/// any step.
///
/// Adjacent duplicates are removed so a `preserve_recent_runs` of 0 or 1 does
/// not spend two identical attempts — and two summarizer calls — on the same cut.
fn emergency_ladder(preserve_recent_runs: usize) -> Vec<PreservationPolicy> {
    let mut ladder = vec![
        PreservationPolicy::RecentRuns(preserve_recent_runs.max(1)),
        PreservationPolicy::RecentRuns(1),
        PreservationPolicy::ThroughLatestCompletedRun,
    ];
    ladder.dedup();
    ladder
}

/// Emergency context preflight executed before every provider request.
///
/// When the next request would cross the emergency threshold, compacts
/// synchronously at this complete exchange boundary and commits the result into
/// `runtime`. Walks [`emergency_ladder`], re-pricing the effective context after
/// every committed projection, and only returns a typed `context_limit:` error
/// once every safe degradation has been tried.
///
/// # Errors
///
/// Returns a `context_limit:`-prefixed message classifying why the effective
/// context still exceeds the emergency threshold. The prefix is a stable
/// contract consumed by the shell's session-recovery path.
pub(crate) async fn run_context_preflight<W: Write>(
    runtime: &mut CompactionRuntime,
    messages: &[Message],
    provider: &dyn ContentGenerator,
    model: &str,
    config: &CoreConfig,
    prefix_tokens: u64,
    writer: &mut W,
) -> Result<(), String> {
    let policy = &config.session.compaction;
    if !policy.enabled {
        return Ok(());
    }
    let capability = ModelCapability::resolve(policy, config.agent.session_token_limit, model);
    let budget = ContextBudget::compute(capability, prefix_tokens, policy);
    if !budget.over_emergency(runtime.effective_history_tokens(messages, prefix_tokens)) {
        return Ok(());
    }
    emit_status(writer, "compaction_emergency_started");

    // `terminal` short-circuits the ladder; `unresolved_cut` records the
    // cut-specific reason the last evaluated step produced no projection.
    let mut terminal: Option<EmergencyFailure> = None;
    let mut unresolved_cut: Option<CompactionError> = None;
    let mut attempts = 0;
    'ladder: for preservation in emergency_ladder(policy.preserve_recent_runs) {
        loop {
            if attempts >= MAX_EMERGENCY_COMPACTION_ATTEMPTS {
                terminal = Some(EmergencyFailure::AttemptLimitReached);
                break 'ladder;
            }
            attempts += 1;
            match compact_in_memory(
                messages,
                runtime.state(),
                runtime.revision(),
                provider,
                model,
                config,
                budget.target_tokens,
                preservation,
            )
            .await
            {
                Ok(candidate) => {
                    runtime.commit_state(candidate);
                    unresolved_cut = None;
                    // A bounded summary input may cover only part of the cut
                    // selected for this policy. Re-price the committed
                    // projection, then repeat the same rung while it can still
                    // advance before giving up any more recent-run protection.
                    if !budget
                        .over_emergency(runtime.effective_history_tokens(messages, prefix_tokens))
                    {
                        emit_status(writer, "compaction_emergency_completed");
                        return Ok(());
                    }
                }
                // Cut-specific outcomes: this policy protected too much
                // history for the attempt to help, but a more aggressive rung
                // summarizes a strictly longer prefix and may still succeed.
                //
                // `NothingToCompact` means no safe split existed under this
                // policy. `NotReducing` means the chosen prefix was too small
                // to pay for its own summary — e.g. summarizing a 1 KiB first
                // run costs 2 KiB — while the next rung can absorb a much
                // larger prefix and win.
                Err(error @ (CompactionError::NothingToCompact | CompactionError::NotReducing)) => {
                    unresolved_cut = Some(error);
                    break;
                }
                // Anything else is not about boundary selection: degrading
                // the policy cannot fix it and would only spend more summarizer
                // calls, so fail closed reporting the actual cause.
                // `OversizedInput` in particular only worsens with a longer
                // prefix.
                Err(error) => {
                    terminal = Some(EmergencyFailure::Attempt(error));
                    break 'ladder;
                }
            }
        }
    }

    let failure = terminal.unwrap_or_else(|| match unresolved_cut {
        // The deepest rung still could not shrink the context: report the real
        // cause rather than a boundary story.
        Some(CompactionError::NotReducing) => {
            EmergencyFailure::Attempt(CompactionError::NotReducing)
        }
        // The most permissive policy found no cut, so everything compactable is
        // already summarized. Classify by what is *measurably* left over
        // instead of assuming the active run is to blame.
        Some(_) => classify_uncompactable_remainder(runtime, messages, &budget),
        // Defensive fallback: every normal ladder exit records either a
        // terminal failure or a cut-specific outcome.
        None => EmergencyFailure::StillOverEmergency,
    });

    let history_tokens = runtime.effective_history_tokens(messages, prefix_tokens);
    emit_status(
        writer,
        &format!("compaction_emergency_failed:{}", failure.code()),
    );
    Err(format!(
        "{CONTEXT_LIMIT_ERROR_PREFIX} effective context (~{history_tokens} tokens) exceeds the \
         emergency threshold ({} of {} usable history tokens) and emergency compaction could not \
         reclaim enough context ({}); {}",
        budget.emergency_tokens,
        budget.usable_history,
        failure.detail(),
        failure.recovery_hint()
    ))
}

/// Classifies an exhausted ladder by the size of what could not be compacted.
///
/// Everything up to the projection's `compacted_through` is already represented
/// by the summary, so the verbatim tail after it — normally the active,
/// incomplete run — is the only history a further rung could have taken. The
/// tail is measured against the emergency threshold instead of blaming the
/// active run merely because one exists: a large persisted summary plus a modest
/// active run exhausts the ladder just as well, and reporting that as
/// "active run alone exceeds the threshold" would misdirect the operator.
fn classify_uncompactable_remainder(
    runtime: &CompactionRuntime,
    messages: &[Message],
    budget: &ContextBudget,
) -> EmergencyFailure {
    let compacted_through = runtime
        .state()
        .map(|state| state.compacted_through)
        .unwrap_or(0)
        .min(messages.len());
    let tail = &messages[compacted_through..];
    if tail.is_empty() {
        // The projection already reaches the transcript end, so the snapshot
        // itself is the whole effective context and what exceeds the threshold.
        return EmergencyFailure::NoSafeSplit;
    }
    // History-only, matching `effective_history_tokens`, which excludes `P`.
    let tail_tokens = estimate_messages_tokens(tail);
    if has_active_run(messages) && budget.over_emergency(tail_tokens) {
        // The still-verbatim tail on its own crosses the threshold, so no
        // summary of the completed history could have saved this request.
        return EmergencyFailure::ActiveRunTooLarge;
    }
    if compacted_through == 0 {
        // Nothing was ever summarized and no safe split exists: the transcript
        // shape itself, not an oversized summary, is the blocker.
        return EmergencyFailure::NoSafeSplit;
    }
    // A summary exists and neither part alone is over the threshold; together
    // they are.
    EmergencyFailure::IrreducibleRemainder
}

/// Writes one system-status line to the protocol stream, mirroring
/// `CoshCore::emit`'s best-effort serialization.
fn emit_status<W: Write>(writer: &mut W, status: &str) {
    if let Ok(json) = serde_json::to_string(&OutputMessage::system_status(status)) {
        let _ = writeln!(writer, "{json}");
        let _ = writer.flush();
    }
}

#[cfg(test)]
mod tests;
