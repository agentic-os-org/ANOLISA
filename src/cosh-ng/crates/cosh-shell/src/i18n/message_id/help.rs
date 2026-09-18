macro_rules! help_core_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            HelpTitle,
            HelpFooter,
            HelpGroupConfig,
            HelpGroupHealth,
            HelpGroupModes,
            HelpGroupHooks,
            HelpSummaryHelp,
            HelpSummaryAuth,
            HelpSummaryConfig,
            HelpSummaryRecommendations,
            HelpSummaryModeApproval,
            HelpSummaryModeAnalysis,
            HelpSummaryAgent,
            HelpSummaryExplain,
            HelpSummaryCancel,
            HelpSummaryDetails,
            HelpSummaryAudit,
            HelpSummaryHooks,
            HelpSummaryHealth,
            HelpSummarySelect,
            HelpSummaryCopy,
            HelpSummaryDebug,
            HelpSummaryClear,
            HelpSummaryShell,
            HelpSummaryApprovalModeRemoved,
            SlashHintTitle,
            SlashHintPrefix,
            SlashHintCurrentMode,
            SlashHintFooter,
            SlashUnknownTitle,
            SlashUnknownBody,
            SlashUnknownSuggestionBody,
            SlashUnknownFooter,
            SlashInfoAuditTitle,
            SlashInfoAuditApprovalsBody,
            SlashInfoAuditActivityBody,
            SlashInfoAuditFooter,
            SlashInfoConfigTitle,
            SlashInfoConfigLanguageLine,
            SlashInfoConfigLanguageEffectiveLine,
            SlashInfoConfigPathLine,
            SlashInfoConfigDebugActivityLine,
            SlashInfoConfigAnalysisStrategyLine,
            SlashInfoConfigRenderFallbackLine,
            SlashInfoConfigFooter,
        );
    };
}

macro_rules! help_session_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            HelpGroupSessions,
            HelpSummarySession,
        );
    };
}

macro_rules! help_registry_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            HelpGroupRegistry,
            HelpSummaryExtensions,
            HelpSummarySkills,
            SlashExtensionsTitle,
            SlashSkillsTitle,
            SlashRegistryUnavailable,
            SlashHooksShellSection,
            SlashHooksAgentSection,
            SlashHooksAgentUnavailable,
            SlashExtensionsEmptyBody,
            SlashSkillsEmptyBody,
        );
    };
}

// Trailing segment (issue #1747): appended after all existing segments so
// every pre-existing MessageId discriminant stays stable, per the
// stable-runtime-api trailing-segment contract established in #1721.
macro_rules! mcp_registry_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            HelpSummaryMcp,
            SlashMcpTitle,
        );
    };
}

macro_rules! slash_parse_error_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            SlashInvalidArgumentsTitle,
            SlashQuotedArgumentsUnsupported,
        );
    };
}

macro_rules! status_query_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            HelpGroupStatus,
            HelpSummaryStatus,
            HelpSummaryStats,
            SlashValueUnavailable,
            SlashValueNotStarted,
            SlashValueIdle,
            SlashValueActive,
            SlashStatusTitle,
            SlashStatusVersionLine,
            SlashStatusBackendLine,
            SlashStatusProviderLine,
            SlashStatusModelLine,
            SlashStatusSessionLine,
            SlashStatusOsLine,
            SlashStatusModesLine,
            SlashStatusProviderUnavailableLine,
            SlashStatusFooter,
            SlashStatsTitle,
            SlashStatsModelTitle,
            SlashStatsToolsTitle,
            SlashStatsModelLine,
            SlashStatsBackendLine,
            SlashStatsRunStateLine,
            SlashStatsToolTotalsLine,
            SlashStatsNoToolCalls,
            SlashStatsToolRow,
            SlashStatsTelemetryUnavailable,
            SlashStatsUsageLine,
            SlashStatsFooter,
        );
    };
}

macro_rules! prompt_soft_newline_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            PromptSoftNewlineTip,
            PromptDraftTitle,
            PromptDraftFooterEditing,
            PromptDraftFooterSubmitted,
            PromptDraftFooterCancelled,
        );
    };
}

// #1932 additions live in a trailing segment so the existing MessageId
// discriminants (a registered stable runtime interface) never shift.
macro_rules! multiline_entry_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            HelpGroupPrompt,
            HelpSummaryDraft,
            PromptMultilineEntryHint,
        );
    };
}

macro_rules! agent_composer_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            AgentComposerTitle,
            PromptDraftRuntimeLabel,
            AgentComposerFooterEditing,
            AgentComposerRejectedTitle,
            AgentComposerRejectedInvalidPathLine,
            AgentComposerRejectedUnavailablePathLine,
            AgentComposerRejectedOutsideWorkspaceLine,
            AgentComposerRejectedWorkspaceUnavailableLine,
            AgentComposerRejectedLimitLine,
            AgentComposerRejectedFooter,
        );
    };
}

// Managed Task entrypoint additions remain trailing so existing stable
// MessageId discriminants do not shift.
macro_rules! managed_task_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            HelpSummaryTask,
        );
    };
}

// Append submission status separately to preserve existing MessageId values.
macro_rules! composer_control_submit_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!($remaining, $($ids,)* PromptDraftFooterControlSubmitted,);
    };
}
