macro_rules! agent_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            AgentThinking,
            AgentThinkingElapsed,
            AgentRecoveryTitle,
            AgentRecoveryFreshTurnBody,
            AgentRecoveryContinuityBody,
            AgentStatusTitle,
            AgentStillWorking,
            AgentStatusFooter,
            AgentStatusStarting,
            AgentStatusWaitingBackend,
            AgentStatusThinking,
            AgentStatusPreparingModelSession,
            AgentStatusStartingModelBackend,
            AgentStatusModelInitialized,
            AgentStatusModelStatus,
            AgentStatusAnalysisCompleted,
            AgentStatusAnalysisReturnedError,
            AgentStatusStreaming,
            AgentStatusReceivingResponse,
            AgentStatusApproval,
            AgentStatusWaitingApprovalTool,
            AgentStatusQuestion,
            AgentStatusWaitingUserAnswer,
            AgentStatusWaitingApprovalCommand,
            AgentStatusTool,
            AgentStatusCapturingToolOutput,
            AgentStatusToolCompleted,
            AgentStatusCompleted,
            AgentStatusFailed,
            AgentStatusCancelled,
            AgentStatusRunningApprovedProviderTool,
            AgentProviderTimeoutDroppedQueuedBody,
            AgentCancellationRequestedTitle,
            AgentCancellationRequestedBody,
            AgentCancelledReasonLabel,
            AgentCancelledUserRequestedReason,
            AgentResponseTitle,
            AgentGovernanceTitle,
            AgentGovernanceStatusLine,
            AgentGovernanceReasonLine,
            AgentGovernanceSummaryLine,
            AgentGovernanceErrorLine,
            AgentGovernanceToolOutputLine,
            AgentGovernanceToolCompletedLine,
            AgentGovernanceApprovalRequiredLine,
            AgentGovernanceShellCommandSubject,
            AgentGovernanceBashCommandSubject,
            AgentGovernanceToolSubject,
            AgentGovernanceBlockedUserApprovalLine,
            AgentGovernanceQuestionLine,
            AgentRecommendedCommandsLabel,
            InterceptNoticeTitle,
            InterceptNoticeBody,
            InterceptNoticeFooter,
            FailedAnalysisCancelledTitle,
            FailedAnalysisCancelledBody,
            FailedAnalysisCancelNoActiveBody,
            FailedAnalysisCancelledFooter,
            AnalysisSkippedTitle,
            AnalysisSkippedBody,
            AnalysisSkippedFooter,
        );
    };
}

// Keep feature-specific additions in a trailing segment so existing public
// MessageId discriminants remain stable.
macro_rules! tool_argument_status_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            AgentStatusToolArguments,
            AgentStatusGeneratingToolArguments,
        );
    };
}

macro_rules! agent_queue_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            AgentQueuedTitle,
            AgentQueuedBodyCommand,
            AgentQueuedBodyActive,
            AgentQueuedFooter,
        );
    };
}

macro_rules! compaction_queue_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            AgentQueueFullTitle,
            AgentControlQueueFullBody,
        );
    };
}

macro_rules! question_hardening_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            AgentQuestionUnavailableTitle,
            AgentQuestionUnavailableBody,
            AgentAnswerDeliveryUnknownTitle,
            AgentAnswerDeliveryUnknownBody,
        );
    };
}

macro_rules! agent_recovery_reason_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            AgentRecoveryTriggerLine,
        );
    };
}

// #2031 trailing segment: appended after every earlier segment so
// pre-existing discriminants never shift.
macro_rules! agent_recovery_retry_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            AgentRecoverySameSessionRetryLine,
        );
    };
}

macro_rules! hook_notification_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            AgentGovernanceHookNotification,
            AgentGovernanceHookUnknown,
            AgentGovernanceHookNoMessage,
            AgentGovernanceHookDecisionUnspecified,
        );
    };
}
