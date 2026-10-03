macro_rules! startup_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            StartupTitle,
            StartupAdapterLine,
            StartupCwdLine,
            StartupCommandsLine,
            StartupHooksNoneSummary,
            StartupHooksCompletedSummary,
            StartupHooksFindingsHeading,
            StartupHooksRustProjectFinding,
            StartupHooksNoFindings,
            StartupHooksReadOnlyNote,
            StartupSwitchHint,
        );
    };
}

// Registered as a trailing segment so all existing MessageId
// discriminants remain stable.
macro_rules! startup_auth_hint_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            StartupAuthHintLine,
        );
    };
}
