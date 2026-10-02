use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::DebugSessionTitle => "Session debug",
        MessageId::DebugAdapterLine => "adapter: {value}",
        MessageId::DebugProviderInvocationLine => "provider invocation: {value}",
        MessageId::DebugProviderCommittedSessionLine => "provider committed session: {value}",
        MessageId::DebugActiveRunLine => "active run: {value}",
        MessageId::DebugQueuedRunsLine => "queued runs: {value}",
        MessageId::DebugProviderPendingSessionLine => "provider pending session: {value}",
        MessageId::DebugProviderInitializeSeenLine => "provider initialize seen: {value}",
        MessageId::DebugHostExecutedShellResultLine => "host-executed shell result: {value}",
        MessageId::DebugSelectedShellExecutionPathLine => "selected shell execution path: {value}",
        MessageId::DebugLatestProviderRequestLine => "latest provider request: {value}",
        MessageId::DebugLatestToolUseLine => "latest tool use id: {value}",
        MessageId::DebugLatestRecoveryStatusLine => "latest recovery status: {value}",
        MessageId::DebugLatestRecoveryReasonLine => "latest recovery reason: {value}",
        MessageId::DebugEvidenceAccessLine => "evidence access: {value}",
        MessageId::DebugEvidenceToolRegisteredLine => "evidence tool registered: {value}",
        MessageId::DebugEvidenceNamespaceLine => "current evidence namespace: {value}",
        MessageId::DebugEvidenceLedgerCountLine => "evidence ledger commands: {value}",
        MessageId::DebugLatestShellOutputReadLine => "latest shell evidence action: {value}",
        MessageId::DebugUnknownTargetBody => "Unknown debug target: {target}",
        MessageId::DebugUnknownTargetFooter => "Use /debug session.",
        MessageId::CommandRemovedTitle => "Command removed",
        MessageId::RemovedDecisionCommandBody => {
            "{command} is no longer a supported input command."
        }
        MessageId::RemovedApprovalDecisionFooter => {
            "Use the approval card buttons instead; nothing was sent to the shell."
        }
        MessageId::RemovedQuestionAnswerFooter => {
            "Answer from the question card instead; nothing was sent to the shell."
        }
        _ => return None,
    })
}
