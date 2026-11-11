macro_rules! startup_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            StartupTitle,
            StartupAdapterLine,
            StartupCwdLine,
            StartupCommandsLine,
            StartupHooksRustProjectFinding,
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

// Registered as a trailing segment so all existing MessageId
// discriminants remain stable.
macro_rules! startup_upgrade_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            StartupUpgradeHintLine,
            StartupUpgradeNoticeTitle,
        );
    };
}

macro_rules! startup_guidance_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!($remaining, $($ids,)* StartupAiDisabledLine,);
    };
}

macro_rules! startup_welcome_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!($remaining, $($ids,)* StartupHelpLabel, StartupAskOnlyLine,);
    };
}
