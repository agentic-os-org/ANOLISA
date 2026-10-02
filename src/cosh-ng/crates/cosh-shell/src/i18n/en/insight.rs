use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::HookAutoAnalyzedTitle => "Hook auto-analyzed",
        MessageId::HookAutoAnalyzedBody => "`{command}` exited with code {exit_code}",
        MessageId::HookAutoAnalyzedFooter => "Agent analysis is starting.",
        MessageId::InsightLabel => "Insight: ",
        MessageId::InsightCommandTypoSummary => "A likely command typo was found",
        MessageId::InsightCommandNotFoundSummary => "The previous input did not run successfully",
        MessageId::InsightCommandNotFoundPrompt => {
            "Analyze and handle the previous input that did not run successfully"
        }
        MessageId::InsightPermissionDeniedSummary => "The command was denied by permissions",
        MessageId::InsightBuildOrTestFailureSummary => "The build or test command failed",
        MessageId::InsightRuntimeExceptionSummary => {
            "The program terminated with an unhandled exception"
        }
        MessageId::InsightAbnormalSignalSummary => {
            "The command ended because of an abnormal signal"
        }
        MessageId::InsightMemoryPressureSummary => "Current memory pressure needs attention",
        MessageId::InsightHighMemoryProcessSummary => "{process} is using unusually high memory",
        MessageId::InsightHighMemoryProcessGenericSummary => {
            "A process is using unusually high memory"
        }
        MessageId::InsightMemoryRootCauseSummary => {
            "Memory pressure may be related to {process}"
        }
        MessageId::InsightMemoryRootCauseGenericSummary => {
            "Memory pressure may be related to a high-memory process"
        }
        MessageId::InsightPermissionDeniedPrompt => {
            "Analyze the permission failure, identify the boundary, and suggest a least-privilege next step"
        }
        MessageId::InsightBuildOrTestFailurePrompt => {
            "Analyze the build or test failure and identify the first actionable error"
        }
        MessageId::InsightRuntimeExceptionPrompt => {
            "Analyze the uncaught exception, confirm the direct cause, and decide whether it needs repair"
        }
        MessageId::InsightAbnormalSignalPrompt => {
            "Analyze the abnormal termination, confirm the signal fact, and suggest one safe check"
        }
        MessageId::InsightMemoryPressurePrompt => {
            "Analyze memory pressure from the current output and identify a process only if needed"
        }
        MessageId::InsightHighMemoryProcessPrompt => {
            "Use the current output to decide whether {process} is the main memory source"
        }
        MessageId::InsightHighMemoryProcessGenericPrompt => {
            "Use the current output to identify the main memory process"
        }
        MessageId::InsightMemoryRootCausePrompt => {
            "Use the current output to confirm whether {process} is the main source of memory pressure"
        }
        MessageId::InsightMemoryRootCauseGenericPrompt => {
            "Use the current output to confirm the main process source of memory pressure"
        }
        MessageId::InsightShellRewriteFirstUseHint => {
            "Press Tab to fill, then Enter to run; keep typing to ignore"
        }
        MessageId::InsightAgentPromptFirstUseHint => {
            "Press Tab to fill, then Enter to submit; keep typing to ignore"
        }
        _ => return None,
    })
}
