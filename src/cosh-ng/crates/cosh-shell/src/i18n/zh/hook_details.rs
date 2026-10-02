use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::HookFindingTitle => "Hook 发现",
        MessageId::HookFindingFooter => "使用 /hooks 查看 Hook 发现。",
        MessageId::HookFindingMarkdownTitle => "命令 Hook 发现",
        MessageId::HookFindingMarkdownHookLine => "- Hook: `{hook_id}`.",
        MessageId::HookFindingMarkdownSeverityLine => "- 严重级别: `{severity}`.",
        MessageId::HookFindingMarkdownFindingLine => "- 发现: {finding}.",
        MessageId::HookFindingMarkdownSuggestionLine => "- 建议: {suggestion}.",
        MessageId::HookFindingMarkdownRelatedTitle => "- 相关发现:",
        MessageId::HookFindingMarkdownRelatedLine => "  - `{hook_id}` [{severity}]: {finding}",
        MessageId::HookFindingMarkdownAgentFollowUpLine => {
            "Agent 后续分析必须先使用 cosh-shell 的有界证据，再给出细节判断。"
        }
        MessageId::HookHintTitle => "Hook 提示",
        MessageId::HookHintNotFoundBody => "本会话没有找到 Hook 提示 '{hint_id}'。",
        MessageId::HookHintNotFoundFooter => "使用 /hooks history 复制最近的发现 id。",
        MessageId::HookHintNoFindingBody => "Hook 提示 '{hint_id}' 没有关联发现。",
        MessageId::HookHintBlockUnavailableBody => "命令块 '{block_id}' 已不可用。",
        MessageId::HookHintIgnoredTitle => "Hook 提示已忽略",
        MessageId::HookHintIgnoredBody => "本会话已忽略 Hook 提示 '{hint_id}'。",
        MessageId::HookHintIgnoredFooter => "后续匹配的发现会被策略降级。",
        MessageId::HookHintUsageTitle => "用法",
        MessageId::HookHintUsageBody => "/hooks analyze|ignore|details <hint_id>",
        MessageId::HookFindingDetailsTitle => "Hook 发现详情",
        MessageId::HookConsultationHookLabel => "Hook",
        MessageId::HookConsultationConfidenceReasonLine => "置信度: {confidence}; 原因: {reason}",
        MessageId::HookConsultationFindingLine => "发现: {finding}",
        MessageId::HookConsultationSuggestionLine => "建议动作: {suggestion}",
        MessageId::HookConsultationAnalyzeAction => "分析",
        MessageId::HookConsultationIgnoreAction => "忽略",
        MessageId::HookDetailsConfidenceLine => "置信度: {confidence}; 策略原因: {reason}",
        MessageId::HookDetailsUserInterestLine => "用户关注原因: {code}: {description}",
        MessageId::HookDetailsReasonLookupIntent => {
            "命令指向特定进程或搜索目标，因此该发现保持低打扰。"
        }
        MessageId::HookDetailsReasonPipelineIntent => {
            "命令管道可能已经转换输出，因此缺失或不确定的结构不会被视为高置信度。"
        }
        MessageId::HookDetailsReasonScriptIntent => {
            "脚本或批处理输出可能不代表用户当前关注点，因此会降低打扰。"
        }
        MessageId::HookDetailsReasonWrapperLowConfidence => {
            "包装器、远程或容器上下文会让目标视图变得不明确，因此需要先验证。"
        }
        MessageId::HookDetailsReasonInteractiveIntent => {
            "交互式输出不是稳定的诊断快照，因此只显示采样提示。"
        }
        MessageId::HookDetailsReasonActiveRunDeferred => {
            "已有另一个 Agent 运行中，因此这个成功命令发现会等待并在显示前重新检查。"
        }
        MessageId::HookDetailsReasonUserContinuedInput => {
            "用户已经继续输入其他内容，因此这个成功命令发现不会打断。"
        }
        MessageId::HookDetailsReasonNonDiagnosticSuccessCommand => {
            "该命令不像明确的诊断快照，因此会降低打扰。"
        }
        MessageId::HookDetailsReasonFeedbackNoisy => {
            "之前的用户反馈表明类似发现噪声较高，因此会降低打扰。"
        }
        MessageId::HookDetailsReasonIgnoredSameFinding => "用户之前在本会话中忽略过匹配的发现。",
        MessageId::HookDetailsReasonSameCardAlreadyRendered => {
            "这个发现键已经展示过同等或更高严重级别的卡片。"
        }
        MessageId::HookDetailsReasonInterruptionBudget => {
            "最近类似卡片已经使用了本会话的打扰预算。"
        }
        MessageId::HookDetailsReasonLowConfidence => "证据不完整，需要先做只读验证再给出更强判断。",
        MessageId::HookDetailsReasonDiagnosticIntent => "明确的诊断命令且证据充分。",
        MessageId::HookDetailsReasonOtherIntent => "没有识别到明确的诊断意图。",
        MessageId::HookDetailsTopicLine => "主题: {topic}; 实体: {entity}",
        MessageId::HookDetailsOriginLine => "命令来源: {origin}",
        MessageId::HookDetailsSuppressionKeyLine => "抑制键: {key}",
        MessageId::HookDetailsOutputRefLine => "输出捕获: {ref}",
        MessageId::HookDetailsCreatedAtLine => "创建时间: {created_at}",
        MessageId::HookDetailsPromptHintLine => "提示词线索: {hint}",
        MessageId::HookDetailsRecommendedSkillLine => "推荐 skill: {skill}",
        MessageId::HookDetailsReadOnlyCliHintLine => "只读 CLI 提示: {hint}",
        MessageId::HookDetailsFooter => "分析仍需要确认。",
        _ => return None,
    })
}
