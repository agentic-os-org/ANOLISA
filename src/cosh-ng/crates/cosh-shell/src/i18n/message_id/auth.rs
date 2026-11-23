macro_rules! auth_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            AuthSelectProviderQuestion,
        );
    };
}

macro_rules! guide_ai_management_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            AuthProviderManagementQuestion,
            GuideAiManagementKeys,
            GuideAuthDeleteKeys,
            GuideAuthFieldKeys,
            GuideWizardLoginPrivilegeDenied,
        );
    };
}

macro_rules! auth_ecs_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            AuthEcsChecking,
            AuthEcsWaiting,
            AuthEcsRetry,
            AuthEcsCancelling,
            AuthEcsCleanupFailed,
            AuthEcsTimedOut,
            AuthEcsFailed,
            AuthEcsSaving,
            AuthEcsUnknown,
            AuthEcsReturn,
            AuthEcsCancelHint,
            AuthEcsRefreshing,
        );
    };
}
