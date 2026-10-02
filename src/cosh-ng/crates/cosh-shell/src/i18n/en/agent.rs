use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::AgentThinking => "Thinking...",
        MessageId::AgentThinkingElapsed => "Thinking... {elapsed}s · {detail}",
        MessageId::AgentRecoveryTitle => "Agent recovery",
        MessageId::AgentRecoveryFreshTurnBody => {
            "Using a fresh provider turn for shell evidence recovery."
        }
        MessageId::AgentRecoveryContinuityBody => "Provider session continuity may be degraded.",
        MessageId::AgentRecoveryTriggerLine => "Recovery trigger: {reason}",
        MessageId::AgentRecoverySameSessionRetryLine => {
            "Provider turn stalled; retrying with session history..."
        }
        MessageId::AgentStatusTitle => "Agent",
        MessageId::AgentStillWorking => "Still working... {elapsed}s · {detail}",
        MessageId::AgentStatusFooter => "Ctrl+C cancels · [Cancel]",
        MessageId::AgentStatusStarting => "starting",
        MessageId::AgentStatusWaitingBackend => "waiting for Agent backend",
        MessageId::AgentStatusThinking => "thinking",
        MessageId::AgentStatusPreparingModelSession => "preparing model session",
        MessageId::AgentStatusStartingModelBackend => "starting model backend",
        MessageId::AgentStatusModelInitialized => "model initialized {model}",
        MessageId::AgentStatusModelStatus => "model status: {status}",
        MessageId::AgentStatusAnalysisCompleted => "analysis completed",
        MessageId::AgentStatusAnalysisReturnedError => "analysis returned an error",
        MessageId::AgentStatusStreaming => "streaming",
        MessageId::AgentStatusReceivingResponse => "receiving Agent response",
        MessageId::AgentStatusApproval => "approval",
        MessageId::AgentStatusWaitingApprovalTool => "waiting for approval: tool {tool}",
        MessageId::AgentStatusQuestion => "question",
        MessageId::AgentStatusWaitingUserAnswer => "waiting for user answer: {question}",
        MessageId::AgentStatusWaitingApprovalCommand => "waiting for approval: {command}",
        MessageId::AgentStatusTool => "tool",
        MessageId::AgentStatusToolArguments => "tool arguments",
        MessageId::AgentStatusGeneratingToolArguments => "generating {tool} arguments...",
        MessageId::AgentStatusCapturingToolOutput => "capturing output from {tool_id}",
        MessageId::AgentStatusToolCompleted => "{tool_id} completed with status {status}",
        MessageId::AgentStatusCompleted => "completed",
        MessageId::AgentStatusFailed => "failed",
        MessageId::AgentStatusCancelled => "cancelled",
        MessageId::AgentQuestionUnavailableTitle => "Agent question unavailable",
        MessageId::AgentQuestionUnavailableBody => {
            "The Agent returned an incomplete question. Please retry."
        }
        MessageId::AgentAnswerDeliveryUnknownTitle => "Agent answer delivery uncertain",
        MessageId::AgentAnswerDeliveryUnknownBody => {
            "The Agent connection closed while sending your answer. Delivery could not be confirmed."
        }
        MessageId::AgentStatusRunningApprovedProviderTool => "running approved provider tool",
        MessageId::AgentProviderTimeoutDroppedQueuedBody => {
            "{dropped} queued requests skipped after provider timeout"
        }
        MessageId::AgentCancellationRequestedTitle => "Agent cancellation requested",
        MessageId::AgentCancellationRequestedBody => "Stopping active Agent run...",
        MessageId::AgentCancelledReasonLabel => "Reason:",
        MessageId::AgentCancelledUserRequestedReason => "user requested cancellation",
        MessageId::AgentResponseTitle => "Agent",
        MessageId::AgentGovernanceTitle => "Governance",
        MessageId::AgentGovernanceStatusLine => "Status: {phase}",
        MessageId::AgentGovernanceReasonLine => "Reason: {reason}",
        MessageId::AgentGovernanceSummaryLine => "Summary: {summary}",
        MessageId::AgentGovernanceErrorLine => "Error: {error}",
        MessageId::AgentGovernanceToolOutputLine => "Tool output: {tool_id} {stream}",
        MessageId::AgentGovernanceToolCompletedLine => "Tool completed: {tool_id}",
        MessageId::AgentGovernanceApprovalRequiredLine => "Approval required: {subject}",
        MessageId::AgentGovernanceShellCommandSubject => "Shell command",
        MessageId::AgentGovernanceBashCommandSubject => "Bash command",
        MessageId::AgentGovernanceToolSubject => "{tool} tool",
        MessageId::AgentGovernanceBlockedUserApprovalLine => "Blocked: user approval required",
        MessageId::AgentGovernanceQuestionLine => "Question: {question}",
        MessageId::AgentGovernanceHookNotification => {
            "Hook: {hook}\nMessage: {message}\nDecision: {decision}"
        }
        MessageId::AgentGovernanceHookUnknown => "unknown hook",
        MessageId::AgentGovernanceHookNoMessage => "no message provided",
        MessageId::AgentGovernanceHookDecisionUnspecified => "unspecified",
        MessageId::AgentRecommendedCommandsLabel => "recommended commands:",
        MessageId::InterceptNoticeTitle => "AI request",
        MessageId::InterceptNoticeBody => "Agent input: {input}",
        MessageId::InterceptNoticeFooter => "The input was intercepted before the shell ran it.",
        MessageId::FailedAnalysisCancelledTitle => "Agent cancelled",
        MessageId::FailedAnalysisCancelledBody => "cancelled pending analysis for `{command}`",
        MessageId::FailedAnalysisCancelNoActiveBody => {
            "no active Agent run is currently waiting for cancellation"
        }
        MessageId::FailedAnalysisCancelledFooter => "Shell remains active.",
        MessageId::AnalysisSkippedTitle => "Analysis skipped",
        MessageId::AnalysisSkippedBody => "skipped repeated failure analysis for `{command}`",
        MessageId::AnalysisSkippedFooter => {
            "Too many consecutive failures for this command. Wait before retrying."
        }
        MessageId::AgentQueuedTitle => "Agent queued",
        MessageId::AgentQueuedBodyCommand => "Captured failed command: {command}",
        MessageId::AgentQueuedBodyActive => "Current Agent run is still streaming.",
        MessageId::AgentQueuedFooter => {
            "This failure will be analyzed after the current Agent run finishes."
        }
        MessageId::AgentQueueFullTitle => "Agent queue full",
        MessageId::AgentControlQueueFullBody => {
            "Too many queued requests to accept this response right now. \
             The card is still pending — answer it again in a moment."
        }
        _ => return None,
    })
}
