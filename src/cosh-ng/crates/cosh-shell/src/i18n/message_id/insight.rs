macro_rules! hook_insight_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            HookAutoAnalyzedTitle,
            HookAutoAnalyzedBody,
            HookAutoAnalyzedFooter,
            InsightLabel,
            InsightCommandTypoSummary,
            InsightPermissionDeniedSummary,
            InsightBuildOrTestFailureSummary,
            InsightRuntimeExceptionSummary,
            InsightAbnormalSignalSummary,
            InsightMemoryPressureSummary,
            InsightHighMemoryProcessSummary,
            InsightHighMemoryProcessGenericSummary,
            InsightMemoryRootCauseSummary,
            InsightMemoryRootCauseGenericSummary,
            InsightPermissionDeniedPrompt,
            InsightBuildOrTestFailurePrompt,
            InsightRuntimeExceptionPrompt,
            InsightAbnormalSignalPrompt,
            InsightMemoryPressurePrompt,
            InsightHighMemoryProcessPrompt,
            InsightHighMemoryProcessGenericPrompt,
            InsightMemoryRootCausePrompt,
            InsightMemoryRootCauseGenericPrompt,
            InsightShellRewriteFirstUseHint,
            InsightAgentPromptFirstUseHint,
        );
    };
}

macro_rules! routing_insight_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            InsightCommandNotFoundSummary,
            InsightCommandNotFoundPrompt,
        );
    };
}
