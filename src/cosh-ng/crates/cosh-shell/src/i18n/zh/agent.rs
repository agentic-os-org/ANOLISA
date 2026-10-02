use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::AgentThinking => "正在思考...",
        MessageId::AgentThinkingElapsed => "正在思考... {elapsed}s · {detail}",
        MessageId::AgentRecoveryTitle => "Agent 恢复",
        MessageId::AgentRecoveryFreshTurnBody => "正在使用新的 provider 轮次恢复 shell evidence。",
        MessageId::AgentRecoveryContinuityBody => "Provider 会话连续性可能降低。",
        MessageId::AgentRecoveryTriggerLine => "恢复触发原因：{reason}",
        MessageId::AgentRecoverySameSessionRetryLine => "Provider 轮次无响应，正带会话历史重试...",
        MessageId::AgentStatusTitle => "Agent",
        MessageId::AgentStillWorking => "仍在处理... {elapsed}s · {detail}",
        MessageId::AgentStatusFooter => "Ctrl+C 取消 · [Cancel]",
        MessageId::AgentStatusStarting => "正在启动",
        MessageId::AgentStatusWaitingBackend => "正在等待 Agent 后端",
        MessageId::AgentStatusThinking => "正在思考",
        MessageId::AgentStatusPreparingModelSession => "正在准备模型会话",
        MessageId::AgentStatusStartingModelBackend => "正在启动模型后端",
        MessageId::AgentStatusModelInitialized => "模型已初始化 {model}",
        MessageId::AgentStatusModelStatus => "模型状态: {status}",
        MessageId::AgentStatusAnalysisCompleted => "分析完成",
        MessageId::AgentStatusAnalysisReturnedError => "分析返回错误",
        MessageId::AgentStatusStreaming => "正在流式输出",
        MessageId::AgentStatusReceivingResponse => "正在接收 Agent 响应",
        MessageId::AgentStatusApproval => "审批",
        MessageId::AgentStatusWaitingApprovalTool => "正在等待审批: tool {tool}",
        MessageId::AgentStatusQuestion => "问题",
        MessageId::AgentStatusWaitingUserAnswer => "正在等待用户回答: {question}",
        MessageId::AgentStatusWaitingApprovalCommand => "正在等待审批: {command}",
        MessageId::AgentStatusTool => "tool",
        MessageId::AgentStatusToolArguments => "工具参数",
        MessageId::AgentStatusGeneratingToolArguments => "正在生成 {tool} 参数...",
        MessageId::AgentStatusCapturingToolOutput => "正在捕获 {tool_id} 的输出",
        MessageId::AgentStatusToolCompleted => "{tool_id} 已完成，状态 {status}",
        MessageId::AgentStatusCompleted => "已完成",
        MessageId::AgentStatusFailed => "已失败",
        MessageId::AgentStatusCancelled => "已取消",
        MessageId::AgentQuestionUnavailableTitle => "Agent 问题不可用",
        MessageId::AgentQuestionUnavailableBody => "Agent 返回的问题不完整，请重试。",
        MessageId::AgentAnswerDeliveryUnknownTitle => "Agent 回答送达状态未知",
        MessageId::AgentAnswerDeliveryUnknownBody => {
            "发送回答时 Agent 连接已关闭，无法确认是否送达。"
        }
        MessageId::AgentStatusRunningApprovedProviderTool => "正在运行已批准的 provider tool",
        MessageId::AgentProviderTimeoutDroppedQueuedBody => {
            "provider 超时后已跳过 {dropped} 个排队请求"
        }
        MessageId::AgentCancellationRequestedTitle => "Agent 取消请求已发送",
        MessageId::AgentCancellationRequestedBody => "正在停止 active Agent 运行...",
        MessageId::AgentCancelledReasonLabel => "原因:",
        MessageId::AgentCancelledUserRequestedReason => "用户请求取消",
        MessageId::AgentResponseTitle => "Agent 回复",
        MessageId::AgentGovernanceTitle => "治理",
        MessageId::AgentGovernanceStatusLine => "状态: {phase}",
        MessageId::AgentGovernanceReasonLine => "原因: {reason}",
        MessageId::AgentGovernanceSummaryLine => "摘要: {summary}",
        MessageId::AgentGovernanceErrorLine => "错误: {error}",
        MessageId::AgentGovernanceToolOutputLine => "Tool 输出: {tool_id} {stream}",
        MessageId::AgentGovernanceToolCompletedLine => "Tool 已完成: {tool_id}",
        MessageId::AgentGovernanceApprovalRequiredLine => "需要审批: {subject}",
        MessageId::AgentGovernanceShellCommandSubject => "Shell 命令",
        MessageId::AgentGovernanceBashCommandSubject => "Bash 命令",
        MessageId::AgentGovernanceToolSubject => "{tool} tool",
        MessageId::AgentGovernanceBlockedUserApprovalLine => "已阻止: 需要用户审批",
        MessageId::AgentGovernanceQuestionLine => "问题: {question}",
        MessageId::AgentGovernanceHookNotification => {
            "Hook: {hook}\n消息: {message}\n决策: {decision}"
        }
        MessageId::AgentGovernanceHookUnknown => "未知 Hook",
        MessageId::AgentGovernanceHookNoMessage => "未提供消息",
        MessageId::AgentGovernanceHookDecisionUnspecified => "未指定",
        MessageId::AgentRecommendedCommandsLabel => "推荐命令:",
        MessageId::InterceptNoticeTitle => "AI 请求",
        MessageId::InterceptNoticeBody => "Agent 输入: {input}",
        MessageId::InterceptNoticeFooter => "该输入已在 Shell 执行前被拦截。",
        MessageId::FailedAnalysisCancelledTitle => "Agent 已取消",
        MessageId::FailedAnalysisCancelledBody => "已取消 `{command}` 的待处理分析",
        MessageId::FailedAnalysisCancelNoActiveBody => "当前没有等待取消的 Agent 运行",
        MessageId::FailedAnalysisCancelledFooter => "Shell 保持可用。",
        MessageId::AnalysisSkippedTitle => "已跳过分析",
        MessageId::AnalysisSkippedBody => "已跳过 `{command}` 的重复失败分析",
        MessageId::AnalysisSkippedFooter => "这个命令连续失败次数过多，请稍后再试。",
        MessageId::AgentQueuedTitle => "Agent 已排队",
        MessageId::AgentQueuedBodyCommand => "已捕获失败命令: {command}",
        MessageId::AgentQueuedBodyActive => "当前 Agent 运行仍在流式输出。",
        MessageId::AgentQueuedFooter => "当前 Agent 完成后会分析这次失败。",
        MessageId::AgentQueueFullTitle => "智能体队列已满",
        MessageId::AgentControlQueueFullBody => {
            "当前排队请求过多，暂时无法接受该响应。卡片仍保持待处理，请稍后重试。"
        }
        _ => return None,
    })
}
