use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::ApprovalTitle => "审批",
        MessageId::ApprovalRequiredTitle => "需要审批",
        MessageId::ApprovalResolutionApprovedTitle => "已批准",
        MessageId::ApprovalResolutionAutoApprovedTitle => "已自动批准",
        MessageId::ApprovalResolutionTrustedTitle => "已信任",
        MessageId::ApprovalResolutionTurnApprovedTitle => "本轮已批准",
        MessageId::ApprovalResolutionDeniedTitle => "已拒绝",
        MessageId::ApprovalResolutionCancelledTitle => "已取消",
        MessageId::ApprovalResolutionBlockedTitle => "已阻止",
        MessageId::ApprovalResolutionDeferredTitle => "已延后",
        MessageId::ApprovalActionAllowOnce => "允许一次",
        MessageId::ApprovalActionApproveTurn => "本轮全部允许",
        MessageId::ApprovalActionAlwaysTrust => "始终信任此命令",
        MessageId::ApprovalActionDeny => "拒绝",
        MessageId::ApprovalActionDetails => "详情",
        MessageId::ApprovalToolInputLabel => "Tool 输入",
        MessageId::ApprovalCommandLabel => "命令",
        MessageId::ApprovalDetailsTitle => "审批详情",
        MessageId::ApprovalDetailsSourceLabel => "来源",
        MessageId::ApprovalDetailsRunLabel => "运行",
        MessageId::ApprovalDetailsExecutionLabel => "执行",
        MessageId::ApprovalDetailsCommandBlockLabel => "命令块",
        MessageId::ApprovalDetailsRedactionLabel => "脱敏",
        MessageId::ApprovalDetailsProviderRequestLabel => "Provider 请求",
        MessageId::ApprovalDetailsToolUseLabel => "Tool 使用",
        MessageId::ApprovalDetailsDefaultDenyLine => "默认: 拒绝",
        MessageId::ApprovalDetailsRequestLabel => "请求",
        MessageId::ApprovalDetailsInputLabel => "输入",
        MessageId::ApprovalDetailsBashCommandSubject => "Bash 命令",
        MessageId::ApprovalDetailsShellCommandSubject => "Shell 命令",
        MessageId::ApprovalDetailsToolSubject => "{tool} tool",
        MessageId::ApprovalDetailsPendingValue => "<待处理>",
        MessageId::ApprovalDetailsNoneValue => "<无>",
        MessageId::ApprovalDetailsNotApplicableValue => "<不适用>",
        MessageId::ApprovalAssessmentSummaryLine => {
            "评估: 影响 {impact}；决策 {decision}；置信度 {confidence}"
        }
        MessageId::ApprovalAssessmentReasonLine => "原因: {reason}",
        MessageId::ApprovalJournalTitle => "审批记录",
        MessageId::ApprovalJournalDecisionCount => "{count} 条决策",
        MessageId::ApprovalJournalEmptyBody => "本 shell 会话还没有审批决策记录。",
        MessageId::ApprovalJournalActorLabel => "执行者",
        MessageId::ApprovalJournalPreviewHashLabel => "预览哈希",
        MessageId::ApprovalJournalSubjectLabel => "对象",
        MessageId::ApprovalJournalPreviewLabel => "预览",
        MessageId::ApprovalRiskSuffix => "风险 {risk}",
        MessageId::ApprovalQueueCompactLine => "队列: {position}/{total} 待处理",
        MessageId::ApprovalQueueFullLine => "队列: 第 {position}/{total} 个待处理",
        MessageId::ApprovalQueueNextSuffix => "；下一个 {next}",
        MessageId::ApprovalSubjectLabel => "对象: ",
        MessageId::ApprovalNextLabel => "下一个: ",
        MessageId::ApprovalKeysPrefix => "按键: ",
        MessageId::ApprovalKeysText => "左/右选择  Enter 确认  d 详情  Esc 取消",
        MessageId::ApprovalExecutableToolPolicy => "策略: 可执行 tool 请求必须先经过用户审批。",
        MessageId::ApprovalExecutableToolPolicyExtra => {
            "MVP 中只有已审批的只读 Bash/shell tool 请求可以运行。"
        }
        MessageId::ApprovalCommandDefaultPolicy => "默认: 拒绝。批准的命令仍会由只读 broker 复查。",
        MessageId::ApprovalRunShellCommandPrompt => "运行 shell 命令？",
        MessageId::ApprovalRunBashCommandPrompt => "运行 Bash 命令？",
        MessageId::ApprovalNotFoundTitle => "审批未找到",
        MessageId::ApprovalNotFoundBody => "{id} 不可用；审批卡片可能已经处理完成。",
        MessageId::ApprovalShellHandoffNotFoundTitle => "Shell handoff 未找到",
        MessageId::ApprovalShellHandoffNotFoundBody => {
            "{id} 不可用；请先对 provider tool failure 使用 Details 操作"
        }
        MessageId::ApprovalShellHandoffBlockedTitle => "Shell handoff 已阻止",
        MessageId::ApprovalShellHandoffBlockedFooter => "命令没有写入前台 shell。",
        MessageId::ApprovalShellHandoffValidationEmptyCommand => "Shell handoff 命令为空。",
        MessageId::ApprovalShellHandoffValidationMultilineCommand => {
            "Shell handoff 命令包含不受支持的换行；仅允许完整单引号参数内的换行。"
        }
        MessageId::ApprovalShellHandoffValidationControlCharacter => {
            "Shell handoff 命令包含被阻止的控制字符。"
        }
        MessageId::ApprovalShellHandoffValidationEmptyPreview => "Shell handoff 预览为空。",
        MessageId::ApprovalShellHandoffValidationEmptyApprovalId => "Shell handoff 审批 id 为空。",
        MessageId::ApprovalShellHandoffValidationEmptyRunId => "Shell handoff run id 为空。",
        MessageId::ApprovalShellHandoffSendingTitle => "正在发送到 shell",
        MessageId::ApprovalShellHandoffSendingBody => "{id} 将在前台 shell 中运行。",
        MessageId::ApprovalShellHandoffTimeoutTitle => "Shell 恢复",
        MessageId::ApprovalShellHandoffTimeoutExceededBody => {
            "命令超过了配置的 shell handoff 超时时间（{seconds}s）。"
        }
        MessageId::ApprovalShellHandoffTimeoutInterruptBody => {
            "已向前台 PTY 发送中断；正在等待 shell evidence。"
        }
        MessageId::ApprovalShellHandoffInputWaitTimeoutTitle => "命令等待输入超时",
        MessageId::ApprovalShellHandoffInputWaitTimeoutExceededBody => {
            "前台命令等待键盘输入超过 {seconds} 秒无人应答（input_wait_timeout_secs）。"
        }
        MessageId::ApprovalShellHandoffInputWaitTimeoutInterruptBody => {
            "已中断该命令（等同 Ctrl+C）；Agent 将收到结果并可改用非交互方式重试。"
        }
        MessageId::ShellInputWaitHintTitle => "⏳ 命令正在等待输入",
        MessageId::ShellInputWaitHintPasswordBody => "命令正在等待密码/隐藏输入。",
        MessageId::ShellInputWaitHintPagerBody => "输出正被分页器查看，按 q 退出后继续。",
        MessageId::ShellInputWaitHintRawInteractiveBody => "交互式程序正在等待键盘输入。",
        MessageId::ShellInputWaitHintStdinWaitBody => "命令正在等待键盘输入/确认。",
        MessageId::ShellInputWaitHintGuidanceBody => "可直接键入回复，或按 Ctrl+C 中断该命令。",
        MessageId::ShellInputWaitHintTimeoutForecastBody => {
            "等待输入 {seconds} 秒无人应答将自动中断。"
        }
        MessageId::ApprovalReceiptKindToolRequest => "tool 请求",
        MessageId::ApprovalReceiptKindShellCommandRequest => "shell 命令请求",
        MessageId::ApprovalReceiptKindBashTool => "Bash tool",
        MessageId::ApprovalReceiptDecisionPending => "待处理",
        MessageId::ApprovalReceiptDecisionApproved => "已批准",
        MessageId::ApprovalReceiptDecisionSentToShell => "已发送到 shell",
        MessageId::ApprovalReceiptDecisionProviderNativeAllowed => "已允许 provider-native 执行",
        MessageId::ApprovalReceiptDecisionApprovedDisplayOnly => "已批准，仅展示",
        MessageId::ApprovalReceiptDecisionDenied => "已拒绝",
        MessageId::ApprovalReceiptDecisionCancelled => "用户已取消",
        MessageId::ApprovalReceiptDecisionBlocked => "已被 cosh-shell 阻止",
        MessageId::ApprovalReceiptSubjectBashSentToShell => "Bash tool: 已发送到 shell",
        MessageId::ApprovalReceiptSubjectBashProviderNative => "Bash tool: provider-native 执行",
        MessageId::ApprovalReceiptBashSentToShellMessage => "Bash tool 已发送到 shell",
        MessageId::ApprovalReceiptForegroundInteractiveHint => {
            "此命令将在前台交互运行，键盘输入会直接发送给它；若进入分页器，通常按 q 返回。"
        }
        MessageId::ApprovalReceiptProviderNativeAllowedMessage => {
            "已允许 provider-native shell tool 执行"
        }
        MessageId::ApprovalHookHeading => "Hook 审查",
        MessageId::ApprovalRiskDetailLabel => "风险: ",
        MessageId::ApprovalRiskLevelHigh => "高风险",
        MessageId::ApprovalRiskLevelMedium => "中风险",
        MessageId::ApprovalRiskLevelLow => "低风险",
        MessageId::ApprovalQueueMetaSuffix => " · 队列 {position}/{total}",
        MessageId::ApprovalRiskPhrasePrivilegeEscalation => "提权操作",
        MessageId::ApprovalRiskPhraseCredentialAccess => "凭证访问",
        MessageId::ApprovalRiskPhraseFilesystemDelete => "文件删除操作",
        MessageId::ApprovalRiskPhraseFilesystemWrite => "文件系统写入",
        MessageId::ApprovalRiskPhrasePermissionChange => "权限变更",
        MessageId::ApprovalRiskPhraseProcessControl => "进程控制",
        MessageId::ApprovalRiskPhraseSystemControl => "整机重启/关停",
        MessageId::ApprovalIrrecoverableWarningLine => {
            "不可逆操作：将重启/关停整机，SSH 会话断开，未保存工作丢失"
        }
        MessageId::ApprovalRiskPhraseServiceControl => "服务控制",
        MessageId::ApprovalRiskPhraseServiceOrContainerControl => "服务/容器控制",
        MessageId::ApprovalRiskPhrasePackageManagerMutation => "软件包变更",
        MessageId::ApprovalRiskPhraseInteractiveEditor => "交互式编辑器可修改文件",
        MessageId::ApprovalRiskPhraseRemoteCodeExecution => "远程代码执行风险",
        MessageId::ApprovalRiskPhraseSensitivePath => "涉及敏感路径",
        MessageId::ApprovalRiskPhraseSensitiveSearch => "敏感信息搜索",
        MessageId::ApprovalRiskPhraseCommandSubstitution => "命令替换语法",
        MessageId::ApprovalRiskPhraseRedirectionWrite => "重定向写文件",
        MessageId::ApprovalRiskPhraseAwkShellExecution => "awk 执行外部命令",
        MessageId::ApprovalRiskLevelUnknown => "未知风险",
        MessageId::ApprovalTurnExtensionSubject => "Agent 轮次预算",
        MessageId::ApprovalTurnExtensionPreview => {
            "Agent 已用完配置的 {turns} 轮。是否继续同一任务并增加 {turns} 轮？"
        }
        MessageId::ApprovalTurnExtensionLabel => "轮次预算",
        MessageId::ApprovalActionContinue => "继续",
        MessageId::ApprovalActionStop => "停止",
        MessageId::ApprovalResolutionContinuingTitle => "正在继续",
        MessageId::ApprovalResolutionStoppedTitle => "已停止",
        MessageId::ApprovalReceiptKindTurnExtension => "轮次预算扩容",
        MessageId::ApprovalTurnExtensionUnavailableTitle => "无法继续",
        MessageId::ApprovalTurnExtensionUnavailableBody => {
            "批准扩容前，已持久化的 provider 会话发生了变化。"
        }
        MessageId::ApprovalTrustUnknownToolReason => {
            "不在受信任工具目录中；Trust 模式下仍需显式审批"
        }
        _ => return None,
    })
}
