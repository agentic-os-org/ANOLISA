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

macro_rules! guide_ai_management_navigation_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            GuideAuthPending,
            GuideAuthTemplateTitle,
            GuideAuthDiscardTitle,
            GuideAuthDiscardBody,
            GuideAuthDiscardKeys,
            GuideAuthDiscardAction,
            GuideAuthContinueEditing,
            GuideAuthContinueKey,
            GuideAuthDestinationHome,
            GuideAuthDestinationOverview,
            GuideAuthDestinationService,
            GuideAuthDestinationManagement,
            GuideAuthDestinationTemplates,
            GuideAuthActivateTitle,
            GuideAuthActivateBody,
            GuideAuthActivateAction,
            GuideAuthDeleteTitle,
            GuideAuthDeleteBody,
            GuideAuthDeleteAction,
            GuideAuthEnterField,
            GuideAuthEditField,
            GuideAuthFieldHint,
            GuideAuthFieldError,
            GuideAuthKeepField,
            GuideAuthBaseUrl,
            GuideAuthApiKey,
            GuideAuthModel,
            GuideAuthBaseUrlHint,
            GuideAuthApiKeyHint,
            GuideAuthModelHint,
            GuideAuthValidationChecks,
            GuideAuthValidationUsage,
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
