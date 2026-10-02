macro_rules! debug_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            DebugSessionTitle,
            DebugAdapterLine,
            DebugProviderInvocationLine,
            DebugProviderCommittedSessionLine,
            DebugActiveRunLine,
            DebugQueuedRunsLine,
            DebugProviderPendingSessionLine,
            DebugProviderInitializeSeenLine,
            DebugHostExecutedShellResultLine,
            DebugSelectedShellExecutionPathLine,
            DebugLatestProviderRequestLine,
            DebugLatestToolUseLine,
            DebugLatestRecoveryStatusLine,
            DebugLatestRecoveryReasonLine,
            DebugEvidenceAccessLine,
            DebugEvidenceToolRegisteredLine,
            DebugEvidenceNamespaceLine,
            DebugEvidenceLedgerCountLine,
            DebugLatestShellOutputReadLine,
            DebugUnknownTargetBody,
            DebugUnknownTargetFooter,
            CommandRemovedTitle,
        );
    };
}

macro_rules! removed_command_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            RemovedDecisionCommandBody,
            RemovedApprovalDecisionFooter,
            RemovedQuestionAnswerFooter,
        );
    };
}
