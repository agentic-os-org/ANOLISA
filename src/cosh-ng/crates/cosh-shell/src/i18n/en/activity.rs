use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::RuntimeDetailsUnavailableTitle => "Details unavailable",
        MessageId::RuntimeDetailsUnavailableBody => {
            "{id} is not available; use a Details action with an approval or activity id"
        }
        MessageId::ActivityTitle => "Activity",
        MessageId::ActivityDetailsTitle => "Activity details",
        MessageId::ActivityRunLabel => "Run",
        MessageId::ActivityDetailLabel => "Detail",
        MessageId::ActivitySkillLabel => "Skill",
        MessageId::ActivitySkillUpdatedStatus => "updated",
        MessageId::ActivityToolLabel => "Tool",
        MessageId::ActivityToolOutputLabel => "Tool output",
        MessageId::ActivityShellLabel => "Shell",
        MessageId::ActivityStatusLoading => "loading",
        MessageId::ActivityStatusLoaded => "loaded",
        MessageId::ActivityStatusFailed => "failed",
        MessageId::ActivityStatusCalled => "called",
        MessageId::ActivityStatusRequested => "requested",
        MessageId::ActivityStatusCaptured => "captured",
        MessageId::ActivityStatusCompleted => "completed",
        MessageId::ActivityStatusCompletedUntracked => "completed (untracked)",
        MessageId::ActivityStatusError => "error",
        MessageId::ActivityStatusInterrupted => "interrupted",
        MessageId::ActivityToolCalledSummary => "{tool} called: {preview}",
        MessageId::ActivityToolRequestedSummary => "{tool} requested: {preview}",
        MessageId::ActivityToolOutputCapturedSummary => "{stream} captured",
        MessageId::ActivityProviderNativeShellBypassSummary => {
            "{tool} auto-approved by provider: {preview}"
        }
        MessageId::ActivityToolNeedsForegroundShellSummary => {
            "may require foreground shell; [Send to shell] {handoff}"
        }
        MessageId::ActivityShellHandoffSentSummary => "{approval} sent to shell",
        MessageId::ToolCardReadFileLabel => "Read",
        MessageId::ToolCardWriteFileLabel => "Write",
        MessageId::ToolCardEditFileLabel => "Edit",
        MessageId::ToolCardSearchFilesLabel => "Search",
        MessageId::ToolCardFindFilesLabel => "Find files",
        MessageId::ToolCardListDirectoryLabel => "List directory",
        MessageId::ToolCardShellLabel => "Shell",
        MessageId::ToolCardWebFetchLabel => "Web fetch",
        MessageId::ToolCardWebSearchLabel => "Web search",
        MessageId::ToolCardSkillLabel => "Skill",
        MessageId::ToolCardAgentLabel => "Agent",
        MessageId::ToolCardMemoryLabel => "Memory",
        MessageId::ToolCardEvidenceLabel => "Shell evidence",
        MessageId::ToolCardCustomToolLabel => "Custom tool",
        MessageId::ToolCardCalledStatus => "called",
        MessageId::ToolCardRequestedStatus => "requested",
        MessageId::ToolCardAutoApprovedStatus => "auto-approved",
        MessageId::ToolCardCapturedStatus => "captured",
        MessageId::ToolCardCompletedStatus => "completed",
        MessageId::ToolCardFailedStatus => "failed",
        MessageId::ToolCardDuplicateStatus => "duplicate request",
        MessageId::ToolCardInterruptedStatus => "interrupted",
        MessageId::ToolCardReadOnlyIntent => "read-only operation",
        MessageId::ToolCardWriteIntent => "will modify workspace state",
        MessageId::ToolCardExecuteIntent => "will run command",
        MessageId::ToolCardNetworkIntent => "will access network",
        MessageId::ToolCardContextIntent => "will update context",
        MessageId::ToolCardCustomIntent => "custom tool invocation",
        MessageId::ToolCardApprovalRequiredAction => "approval required",
        MessageId::ToolCardWriteCompletedResult => "write completed",
        MessageId::ToolCardEditCompletedResult => "edit completed",
        MessageId::ToolCardSkillAvailableResult => "instructions available",
        MessageId::ToolCardShellEvidenceDeliveredResult => "body not repeated",
        MessageId::ToolCardShellEvidenceListResult => "command history delivered to Agent",
        MessageId::ToolCardShellEvidenceReadResult => "shell output excerpt delivered to Agent",
        MessageId::ToolCardShellEvidenceAlreadyDeliveredResult => {
            "recent output already delivered; body not repeated"
        }
        MessageId::ToolCardShellEvidenceFailedResult => "shell evidence unavailable",
        MessageId::ToolCardShellEvidenceDuplicateResult => {
            "provider repeated the same shell evidence request"
        }
        MessageId::ToolCardShellEvidenceMetadataMetric => "evidence metadata: {count} lines",
        MessageId::ToolCardOutputCapturedResult => "output captured",
        MessageId::ToolCardLinesReturnedResult => "{count} lines returned",
        MessageId::ToolCardStdoutMetric => "stdout: {count} lines",
        MessageId::ToolCardStderrMetric => "stderr: {count} lines",
        MessageId::ToolCardTruncatedMetric => "truncated",
        MessageId::MarkdownCodeLabel => "code",
        MessageId::MarkdownCodeWithLanguageLabel => "code: {language}",
        MessageId::MarkdownTableLabel => "table",
        MessageId::ToolOutputStdoutCapturedSummary => "stdout captured",
        MessageId::ToolOutputStderrCapturedSummary => "stderr captured",
        MessageId::ToolSummaryExit => "exit {exit}",
        MessageId::ToolSummaryBlocked => "tool request blocked by shell broker guard",
        MessageId::ToolSummaryTimedOut => "tool request timed out",
        MessageId::ToolSummaryFailed => "tool request failed",
        _ => return None,
    })
}
