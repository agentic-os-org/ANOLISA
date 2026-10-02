use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::SessionTitle => "Agent sessions",
        MessageId::SessionUnavailableBody => "Session recovery requires the cosh-core backend.",
        MessageId::SessionBusyBody => {
            "Finish the active Agent run or open panel before managing sessions."
        }
        MessageId::SessionEmptyBody => "No persisted sessions exist for this workspace.",
        MessageId::SessionListFooter => {
            "Use /session to open the picker or /session resume <id>."
        }
        MessageId::SessionStatusTitle => "Session recovery status",
        MessageId::SessionShellIdLine => "shell session: {id}",
        MessageId::SessionProviderIdLine => {
            "active provider session: {active}\nselected provider session: {selected}"
        }
        MessageId::SessionWorkspaceLine => "workspace: {workspace}",
        MessageId::SessionRecoveryLine => "recovery state: {state}",
        MessageId::SessionErrorLine => "last recovery error: [{code}] {error}",
        MessageId::SessionEvidenceNotRestoredBody => {
            "Historical terminal evidence was not restored; only model conversation context resumes."
        }
        MessageId::SessionPickerFooter => {
            "Up/Down or j/k move · Enter resume · Space toggle clear mark · d review clear · Esc cancel"
        }
        MessageId::SessionPickerMarkedCount => "{count} marked",
        MessageId::SessionPickerMarkedFooter => {
            "Up/Down or j/k move · Enter review clear · Space toggle clear mark · d review clear · Esc cancel"
        }
        MessageId::SessionClearConfirmTitle => "Confirm session clear",
        MessageId::SessionClearConfirmCountLine => {
            "The following {count} persisted session(s) will be deleted:"
        }
        MessageId::SessionClearConfirmFooter => "Enter or y confirms · Esc, Ctrl+C, or n cancels",
        MessageId::SessionSelectedTitle => "Session selected",
        MessageId::SessionSelectedBody => {
            "Provider session {id} will resume on the next Agent request."
        }
        MessageId::SessionErrorTitle => "Session recovery",
        MessageId::SessionClearedTitle => "Sessions cleared",
        MessageId::SessionClearedBody => "Deleted {count} persisted session(s).",
        MessageId::SessionSkippedBody => "Skipped {count} protected or unavailable session(s).",
        MessageId::SessionClearInterruptedBody => {
            "Clear stopped [{code}]: {unknown} session(s) have unknown status; {unattempted} were not attempted."
        }
        MessageId::SessionCancelledTitle => "Session manager closed",
        MessageId::SessionCancelledBody => "No provider session or persisted file changed.",
        MessageId::SessionUsageBody => {
            "Usage: /session [new|status|list [--all]|resume <id>|clear <id>...|clear --all|compact [status|cancel]]"
        }
        MessageId::SessionNewTitle => "Fresh session",
        MessageId::SessionNewDetachedBody => {
            "Detached from provider session {id}.\nThe next Agent request starts a fresh conversation.\nShell cwd, history, and settings are unchanged."
        }
        MessageId::SessionNewAlreadyFreshBody => {
            "No provider session was attached.\nThe next Agent request starts a fresh conversation.\nShell cwd, history, and settings are unchanged."
        }
        MessageId::SessionNotReadyBody => {
            "Session {id} is {health} and cannot be resumed. It may still be cleared."
        }
        MessageId::SessionProtectedBody => {
            "Active or selected provider sessions are protected and were not cleared."
        }
        MessageId::SessionCompactTitle => "Session compaction",
        MessageId::SessionCompactStartedBody => {
            "Compaction is running in the background for session {id}.\nThe shell remains available; Agent requests are temporarily paused."
        }
        MessageId::SessionCompactFooter => {
            "/session compact status shows progress · /session compact cancel stops it"
        }
        MessageId::SessionCompactStatusTitle => "Session compaction status",
        MessageId::SessionCompactStatusSessionLine => "session: {id}",
        MessageId::SessionCompactStatusRunningLine => "state: {state} · elapsed {elapsed}s",
        MessageId::SessionCompactStatusIdleBody => "No background compaction is running.",
        MessageId::SessionCompactStatusRecommendedBody => {
            "Automatic compaction is recommended and will start at the next idle boundary."
        }
        MessageId::SessionCompactStatusPendingRenderBody => {
            "Compaction finished; the result will be shown at the next safe prompt boundary."
        }
        MessageId::SessionCompactNoSessionBody => {
            "No active resumable cosh-core session. Run an Agent request or /session resume <id> first."
        }
        MessageId::SessionCompactDuplicateBody => {
            "A compaction is already running; use /session compact status or /session compact cancel."
        }
        MessageId::SessionCompactNotRunningBody => "No background compaction is running.",
        MessageId::SessionCompactCancelRequestedBody => {
            "Cancellation requested; the background compactor is being terminated."
        }
        MessageId::SessionCompactPendingCancelledBody => {
            "Cancelled the recommended automatic compaction before it started; no compactor process was running.\nThis context revision will not retrigger automatically; further context growth still can."
        }
        MessageId::SessionCompactCompletedTitle => "Context compacted in the background",
        MessageId::SessionCompactCompletedBody => {
            "{before} → approximately {after} tokens ({source})"
        }
        MessageId::SessionCompactCompletedRetainedBody => {
            "Complete session history was retained. Agent conversation is available again."
        }
        MessageId::SessionCompactFailedTitle => "Session compaction failed",
        MessageId::SessionCompactFailedBody => "[{code}] {message}",
        MessageId::SessionCompactCancelledTitle => "Session compaction cancelled",
        MessageId::SessionCompactCancelledBody => {
            "The complete transcript is unchanged; the projection may have committed just before cancellation, so the latest valid version will be used."
        }
        MessageId::SessionCompactAgentPausedTitle => "Agent paused during compaction",
        MessageId::SessionCompactAgentPausedBody => {
            "Session compaction is running; Agent requests are temporarily paused.\nShell commands still work. Use /session compact status or /session compact cancel."
        }
        MessageId::SessionCompactAutoStartedBody => {
            "Context is at {percent}% of the usable window; compaction is running in the background for session {id}.\nThe shell remains available; Agent requests are temporarily paused."
        }
        MessageId::SessionCompactSpawnFailedBody => {
            "Failed to start the background compactor: {error}"
        }
        MessageId::SessionCompactFailedTranscriptBody => {
            "The complete session transcript is unchanged; Agent conversation is available again."
        }
        MessageId::SessionCompactQueueFullBody => {
            "Too many Agent requests are already queued; this one was not added. Wait for the current work to finish, then send it again."
        }
        _ => return None,
    })
}
