use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::HelpTitle => "Slash 命令",
        MessageId::HelpFooter => {
            "模式: {mode}. 策略: {strategy}. Shift+Enter / Alt+Enter 在 prompt 内插入换行；以 ?? 开头可多行组稿。"
        }
        MessageId::PromptSoftNewlineTip => {
            "提示：以 ?? 开头可组稿多行 prompt（Shift+Enter 换行）。"
        }
        MessageId::PromptDraftTitle => "Prompt 草稿",
        MessageId::PromptDraftFooterEditing => {
            "Enter 发送 · Shift+Enter 换行 · Esc 取消"
        }
        MessageId::PromptDraftFooterSubmitted => "已发送给 Agent",
        MessageId::PromptDraftFooterControlSubmitted => "命令已提交",
        MessageId::PromptDraftFooterCancelled => "草稿已取消",
        MessageId::AgentComposerTitle => "Agent Composer",
        MessageId::PromptDraftRuntimeLabel => "Runtime",
        MessageId::AgentComposerFooterEditing => {
            "/ 命令 · ↑↓ 选择 · Tab 补全 · Enter 提交 · Shift+Enter 换行 · Esc 取消"
        }
        MessageId::AgentComposerRejectedTitle => "已跳过引用",
        MessageId::AgentComposerRejectedInvalidPathLine => {
            "{path}：不是有效的工作空间相对路径"
        }
        MessageId::AgentComposerRejectedUnavailablePathLine => {
            "{path}：路径不存在，或不是文件/目录"
        }
        MessageId::AgentComposerRejectedOutsideWorkspaceLine => {
            "{path}：解析后的路径超出工作空间"
        }
        MessageId::AgentComposerRejectedWorkspaceUnavailableLine => {
            "{path}：Shell 工作空间不可用"
        }
        MessageId::AgentComposerRejectedLimitLine => {
            "{path}：已达到 16 个引用的上限"
        }
        MessageId::AgentComposerRejectedFooter => {
            "已跳过的引用不会作为 Agent 上下文发送。"
        }
        MessageId::HelpGroupPrompt => "Prompt",
        MessageId::HelpSummaryDraft => "打开多行 Prompt 草稿卡（?? 回车等效）",
        MessageId::PromptMultilineEntryHint => {
            "多行提问可输入 ?? 后回车；一次性 cosh-core 请求可使用 /agent"
        }
        MessageId::HelpGroupConfig => "配置",
        MessageId::HelpGroupHealth => "健康",
        MessageId::HelpGroupModes => "模式",
        MessageId::HelpGroupHooks => "Hooks",
        MessageId::HelpSummaryHelp => "显示命令参考",
        MessageId::HelpSummaryAuth => "配置 AI 服务商凭证",
        MessageId::HelpSummaryConfig => "配置界面语言",
        MessageId::HelpSummaryRecommendations => {
            "仅管理个性化提示词推荐（失败命令 Insight 由 /mode analysis 控制）；分析会向服务商发送有界活动，本地 clear 不控制服务商侧保留"
        }
        MessageId::HelpSummaryModeApproval => "切换审批模式",
        MessageId::HelpSummaryModeAnalysis => {
            "选择建议模式、自动分析或关闭主动介入；控制被动建议与失败命令 Insight"
        }
        MessageId::HelpSummaryModeRouting => "选择未知自然语言输入是否可以路由给 Agent",
        MessageId::HelpSummaryAgent => "组稿一次性 Agent 请求",
        MessageId::HelpSummaryExplain => "分析上一个失败命令",
        MessageId::HelpSummaryCancel => "取消正在运行的 Agent 工作",
        MessageId::HelpSummaryDetails => "查看审批或活动详情",
        MessageId::HelpSummaryAudit => "显示审计入口",
        MessageId::HelpSummaryHooks => "显示 Hook 状态",
        MessageId::HelpSummaryHealth => "按需运行健康检查",
        MessageId::HelpSummarySelect => "展示一条推荐",
        MessageId::HelpSummaryCopy => "复制一条推荐",
        MessageId::HelpSummaryDebug => "显示会话调试详情",
        MessageId::HelpSummaryClear => "清理本地 shell 状态",
        MessageId::HelpSummaryShell => "返回 shell 输入",
        MessageId::HelpSummaryApprovalModeRemoved => "已移除的 approval-mode 别名",
        MessageId::SlashHintTitle => "Slash 命令提示",
        MessageId::SlashHintPrefix => "前缀: {prefix}",
        MessageId::SlashHintCurrentMode => "当前模式: {mode}",
        MessageId::SlashHintFooter => "输入完整命令并回车；/tmp/foo 这类路径仍进入 shell。",
        MessageId::SlashUnknownTitle => "Slash 命令",
        MessageId::SlashUnknownBody => "未知 slash 命令: {command}",
        MessageId::SlashUnknownSuggestionBody => "你是不是想用 {command}？",
        MessageId::SlashUnknownFooter => "使用 /help 查看可用命令。",
        MessageId::SlashInvalidArgumentsTitle => "Slash 参数错误",
        MessageId::SlashQuotedArgumentsUnsupported => {
            "不支持带引号的参数。本例请改用 /mode approval trust confirm。"
        }
        MessageId::SlashInfoAuditTitle => "审计",
        MessageId::SlashInfoAuditApprovalsBody => "审批决策可通过 Details 操作查看。",
        MessageId::SlashInfoAuditActivityBody => "活动 output ref 可通过 Details 操作查看。",
        MessageId::SlashInfoAuditFooter => "审计视图是只读的；不会运行 shell 命令。",
        MessageId::SlashInfoConfigTitle => "配置",
        MessageId::SlashInfoConfigLanguageLine => "语言: {effective} 来源: {source}",
        MessageId::SlashInfoConfigLanguageEffectiveLine => {
            "语言: {effective} 生效，设置: {setting}，来源: {source}"
        }
        MessageId::SlashInfoConfigPathLine => "配置文件: {path}",
        MessageId::SlashInfoConfigDebugActivityLine => {
            "调试活动: {state} (ui.debug 或 COSH_SHELL_DEBUG=1)"
        }
        MessageId::SlashInfoConfigAnalysisStrategyLine => {
            "分析策略: /mode analysis smart|auto|manual"
        }
        MessageId::SlashInfoConfigRenderFallbackLine => {
            "渲染降级: 启动 cosh-shell 前设置 COSH_SHELL_RENDER=plain。"
        }
        MessageId::SlashInfoConfigFooter => {
            "使用 /config language [auto|en-US|zh-CN]。设置立即生效；Agent 回复跟随你的提问语言。"
        }
        MessageId::HelpGroupSessions => "会话",
        MessageId::HelpSummarySession => "查找、恢复和清理智能体会话",
        MessageId::HelpSummaryTask => "委托并查看本机持久 Agent Task",
        MessageId::HelpGroupRegistry => "Registry",
        MessageId::HelpSummaryExtensions => "列出/管理 cosh-core 扩展",
        MessageId::HelpSummarySkills => "列出/查看 cosh-core 技能",
        MessageId::HelpSummaryMcp => "管理 MCP 服务器",
        MessageId::HelpGroupStatus => "状态",
        MessageId::HelpSummaryStatus => "显示版本、服务商、模型和运行状态",
        MessageId::HelpSummaryStats => "显示模型和工具的会话统计",
        MessageId::SlashValueUnavailable => "不可用",
        MessageId::SlashValueNotStarted => "未启动",
        MessageId::SlashValueIdle => "空闲",
        MessageId::SlashValueActive => "运行中",
        MessageId::SlashStatusTitle => "状态",
        MessageId::SlashStatusVersionLine => "cosh-shell: {version}",
        MessageId::SlashStatusBackendLine => "后端: {backend}",
        MessageId::SlashStatusProviderLine => "服务商: {provider}",
        MessageId::SlashStatusModelLine => "模型: {model}",
        MessageId::SlashStatusSessionLine => "会话: {session}",
        MessageId::SlashStatusOsLine => "操作系统: {os}",
        MessageId::SlashStatusModesLine => "模式: 审批={approval}，分析={analysis}",
        MessageId::SlashStatusProviderUnavailableLine => {
            "服务商详情: 当前后端未提供"
        }
        MessageId::SlashStatusFooter => {
            "/about 是 /status 的别名。使用 /stats [model|tools] 查看会话统计。"
        }
        MessageId::SlashStatsTitle => "会话统计",
        MessageId::SlashStatsModelTitle => "模型统计",
        MessageId::SlashStatsToolsTitle => "工具统计",
        MessageId::SlashStatsModelLine => "模型: {model}",
        MessageId::SlashStatsBackendLine => "后端: {backend}",
        MessageId::SlashStatsRunStateLine => "Agent 运行状态: {state}",
        MessageId::SlashStatsToolTotalsLine => {
            "工具: 调用 {calls} 次，成功 {successful} 次，失败 {failed} 次，待定 {pending} 次"
        }
        MessageId::SlashStatsNoToolCalls => "当前会话尚未记录工具调用。",
        MessageId::SlashStatsToolRow => {
            "{name}: 调用 {calls} 次，成功 {successful} 次，失败 {failed} 次，待定 {pending} 次"
        }
        MessageId::SlashStatsTelemetryUnavailable => {
            "当前后端协议未提供 Token 数、API 错误和延迟数据。"
        }
        MessageId::SlashStatsUsageLine => "用法: /stats [model|tools]",
        MessageId::SlashStatsFooter => {
            "统计信息只读，覆盖当前 cosh-shell 进程已观测到的数据。"
        }
        MessageId::SlashExtensionsTitle => "扩展",
        MessageId::SlashSkillsTitle => "技能",
        MessageId::SlashMcpTitle => "MCP 服务器",
        MessageId::SlashRegistryUnavailable => "此功能需要 cosh-core 后端支持。",
        MessageId::SlashHooksShellSection => "Shell Hooks",
        MessageId::SlashHooksAgentSection => "Agent Hooks",
        MessageId::SlashHooksAgentUnavailable => "(cosh-core 后端不可用)",
        MessageId::SlashExtensionsEmptyBody => "未安装扩展。",
        MessageId::SlashSkillsEmptyBody => "未发现技能。",
        _ => return None,
    })
}
