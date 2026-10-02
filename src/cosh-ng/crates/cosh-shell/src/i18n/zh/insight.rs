use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::HookAutoAnalyzedTitle => "Hook 自动分析",
        MessageId::HookAutoAnalyzedBody => "`{command}` 退出码为 {exit_code}",
        MessageId::HookAutoAnalyzedFooter => "Agent 分析正在启动。",
        MessageId::InsightLabel => "洞察：",
        MessageId::InsightCommandTypoSummary => "发现可能的命令拼写错误",
        MessageId::InsightCommandNotFoundSummary => "刚才的输入未成功执行",
        MessageId::InsightCommandNotFoundPrompt => "分析并处理刚才未成功执行的输入",
        MessageId::InsightPermissionDeniedSummary => "命令因权限不足被拒绝",
        MessageId::InsightBuildOrTestFailureSummary => "构建或测试失败",
        MessageId::InsightRuntimeExceptionSummary => "程序发生未捕获异常",
        MessageId::InsightAbnormalSignalSummary => "命令因异常信号而终止",
        MessageId::InsightMemoryPressureSummary => "当前内存压力需要关注",
        MessageId::InsightHighMemoryProcessSummary => "{process} 的内存占用异常偏高",
        MessageId::InsightHighMemoryProcessGenericSummary => "有进程的内存占用异常偏高",
        MessageId::InsightMemoryRootCauseSummary => "内存压力可能与 {process} 有关",
        MessageId::InsightMemoryRootCauseGenericSummary => "内存压力可能与高内存进程有关",
        MessageId::InsightPermissionDeniedPrompt => {
            "分析这次权限失败，先判断限制来源并给出最小权限建议"
        }
        MessageId::InsightBuildOrTestFailurePrompt => "分析这次构建或测试失败，定位首个可行动错误",
        MessageId::InsightRuntimeExceptionPrompt => {
            "分析这次未捕获异常，确认直接原因及是否需要修复"
        }
        MessageId::InsightAbnormalSignalPrompt => {
            "分析命令异常终止，确认信号事实并给出一个安全检查"
        }
        MessageId::InsightMemoryPressurePrompt => "基于当前输出分析内存压力，必要时定位主要进程",
        MessageId::InsightHighMemoryProcessPrompt => {
            "基于当前输出判断 {process} 是否为主要内存来源"
        }
        MessageId::InsightHighMemoryProcessGenericPrompt => "基于当前输出判断主要内存进程",
        MessageId::InsightMemoryRootCausePrompt => {
            "基于当前输出确认 {process} 是否为内存压力主要来源"
        }
        MessageId::InsightMemoryRootCauseGenericPrompt => "基于当前输出确认内存压力的主要进程来源",
        MessageId::InsightShellRewriteFirstUseHint => "Tab 填入后按 Enter 执行；继续输入可忽略",
        MessageId::InsightAgentPromptFirstUseHint => "Tab 填入后按 Enter 提交；继续输入可忽略",
        _ => return None,
    })
}
