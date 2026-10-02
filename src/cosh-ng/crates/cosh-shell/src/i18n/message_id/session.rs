macro_rules! session_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            SessionTitle,
            SessionUnavailableBody,
            SessionBusyBody,
            SessionEmptyBody,
            SessionListFooter,
            SessionStatusTitle,
            SessionShellIdLine,
            SessionProviderIdLine,
            SessionWorkspaceLine,
            SessionRecoveryLine,
            SessionErrorLine,
            SessionEvidenceNotRestoredBody,
            SessionPickerFooter,
            SessionClearConfirmTitle,
            SessionClearConfirmCountLine,
            SessionClearConfirmFooter,
            SessionSelectedTitle,
            SessionSelectedBody,
            SessionErrorTitle,
            SessionClearedTitle,
            SessionClearedBody,
            SessionSkippedBody,
            SessionClearInterruptedBody,
            SessionCancelledTitle,
            SessionCancelledBody,
            SessionUsageBody,
            SessionNotReadyBody,
            SessionProtectedBody,
        );
    };
}

// Keep new compaction IDs in a trailing segment so existing MessageId
// discriminants remain stable across the upstream i18n module split.
macro_rules! session_compaction_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            SessionCompactTitle,
            SessionCompactStartedBody,
            SessionCompactFooter,
            SessionCompactStatusTitle,
            SessionCompactStatusSessionLine,
            SessionCompactStatusRunningLine,
            SessionCompactStatusIdleBody,
            SessionCompactStatusRecommendedBody,
            SessionCompactStatusPendingRenderBody,
            SessionCompactNoSessionBody,
            SessionCompactDuplicateBody,
            SessionCompactNotRunningBody,
            SessionCompactCancelRequestedBody,
            SessionCompactPendingCancelledBody,
            SessionCompactCompletedTitle,
            SessionCompactCompletedBody,
            SessionCompactCompletedRetainedBody,
            SessionCompactFailedTitle,
            SessionCompactFailedBody,
            SessionCompactCancelledTitle,
            SessionCompactCancelledBody,
            SessionCompactAgentPausedTitle,
            SessionCompactAgentPausedBody,
            SessionCompactAutoStartedBody,
            SessionCompactSpawnFailedBody,
            SessionCompactFailedTranscriptBody,
            SessionCompactQueueFullBody,
        );
    };
}

// Registered as the final segment in message_id.rs so the picker-count ID
// does not shift the discriminants pinned by the fieldless-enum test.
macro_rules! session_picker_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            SessionPickerMarkedCount,
        );
    };
}

// Registered after the picker segment so all existing MessageId
// discriminants remain stable.
macro_rules! session_fresh_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            SessionNewTitle,
            SessionNewDetachedBody,
            SessionNewAlreadyFreshBody,
        );
    };
}

// Keep this new picker footer message in its own trailing segment so every
// existing MessageId discriminant remains stable.
macro_rules! session_picker_footer_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            SessionPickerMarkedFooter,
        );
    };
}
