use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::SlashHooksRegisteredTitle => "Hook 状态",
        MessageId::SlashHooksNoHooksBody => "未注册 Hook。",
        MessageId::SlashHooksStatusCountLine => {
            "已注册: {total}; 已启用: {enabled}; 已禁用: {disabled}。"
        }
        MessageId::SlashHooksStatusSourcesLine => {
            "来源: builtin={builtin}; user={user}; project={project}。"
        }
        MessageId::SlashHooksStatusProjectTrustLine => {
            "项目信任: trusted={trusted}; untrusted={untrusted}。"
        }
        MessageId::SlashHooksFooterCount => "已注册 {count} 个 Hook。",
        MessageId::SlashHooksFooterMutedTargets => {
            "已注册 {count} 个 Hook。已静音目标: {targets}。"
        }
        MessageId::SlashHooksTargetMutedTitle => "Hook 目标已静音",
        MessageId::SlashHooksTargetMutedBody => "本会话已静音 Hook 目标 '{target}'。",
        MessageId::SlashHooksTargetMutedFooter => "已静音 finding 仍会记录在 /hooks history。",
        MessageId::SlashHooksTargetUnmutedTitle => "Hook 目标已取消静音",
        MessageId::SlashHooksTargetUnmutedBody => "已取消静音 Hook 目标 '{target}'。",
        MessageId::SlashHooksTargetNotMutedBody => "Hook 目标 '{target}' 未处于静音状态。",
        MessageId::SlashHooksEnabledTitle => "Hook 已启用",
        MessageId::SlashHooksEnabledBody => "Hook '{id}' 已启用。",
        MessageId::SlashHooksDisabledTitle => "Hook 已禁用",
        MessageId::SlashHooksDisabledBody => "Hook '{id}' 已禁用。",
        MessageId::SlashHooksHistoryTitle => "Hook 历史",
        MessageId::SlashHooksHistoryEmptyBody => "本会话未记录 Hook finding。",
        MessageId::SlashHooksHistoryFooter => "最近 finding 只读；Analyze 仍需要用户确认。",
        MessageId::SlashHooksEventsTitle => "Hook 显示事件",
        MessageId::SlashHooksEventsEmptyBody => "本会话未记录 Hook 显示事件。",
        MessageId::SlashHooksEventsFooter => "事件仅属于当前会话，包含策略元数据，不包含命令输出。",
        MessageId::SlashHooksUsageTitle => "用法",
        MessageId::SlashHooksUsageListLine => "/hooks                - 显示 Hook 状态",
        MessageId::SlashHooksUsageHistoryLine => "/hooks history        - 显示最近 Hook finding",
        MessageId::SlashHooksUsageEventsLine => "/hooks events         - 显示最近 Hook 展示事件",
        MessageId::SlashHooksUsageAnalyzeLine => "/hooks analyze <id>   - 分析提示 finding",
        MessageId::SlashHooksUsageIgnoreLine => "/hooks ignore <id>    - 忽略提示 finding",
        MessageId::SlashHooksUsageDetailsLine => "/hooks details <id>   - 显示 Hook finding 详情",
        MessageId::SlashHooksUsageFeedbackLine => "/hooks feedback noisy|useful <id> - 记录反馈",
        MessageId::SlashHooksUsageClearFeedbackLine => "/hooks clear-feedback - 清除 Hook 反馈偏好",
        MessageId::SlashHooksUsageMuteLine => "/hooks mute <target>  - 静音 topic 或 Hook id",
        MessageId::SlashHooksUsageUnmuteLine => "/hooks unmute <target>- 取消静音 topic 或 Hook id",
        MessageId::SlashHooksUsageTrustProjectLine => "/hooks trust-project  - 信任本会话项目 Hook",
        MessageId::SlashHooksUsageUntrustProjectLine => {
            "/hooks untrust-project- 取消信任本会话项目 Hook"
        }
        MessageId::SlashHooksUsageClearProjectTrustLine => {
            "/hooks clear-project-trust - 清除项目 Hook 信任存储"
        }
        MessageId::SlashHooksUsageEnableLine => "/hooks enable <id>    - 启用 Hook",
        MessageId::SlashHooksUsageDisableLine => "/hooks disable <id>   - 禁用 Hook",
        MessageId::SlashHooksProjectTrustedTitle => "项目 Hook 已信任",
        MessageId::SlashHooksProjectUntrustedTitle => "项目 Hook 已取消信任",
        MessageId::SlashHooksProjectTrustNoHooksBody => "本会话未注册项目 Hook。",
        MessageId::SlashHooksProjectTrustedBody => "已将 {count} 个项目 Hook 标记为 trusted。",
        MessageId::SlashHooksProjectUntrustedBody => "已将 {count} 个项目 Hook 标记为 untrusted。",
        MessageId::SlashHooksProjectTrustNoChangeFooter => "信任状态未变更。",
        MessageId::SlashHooksProjectTrustPersistedFooter => "信任已持久化；已禁用 Hook 保持禁用。",
        MessageId::SlashHooksProjectTrustRemovedFooter => {
            "信任已从持久化存储移除；已禁用 Hook 保持禁用。"
        }
        MessageId::SlashHooksProjectTrustPersistenceFailedFooter => {
            "会话状态已变更，但持久化失败: {failures}"
        }
        MessageId::SlashHooksProjectTrustClearedTitle => "项目 Hook 信任已清除",
        MessageId::SlashHooksProjectTrustClearedBody => {
            "已将 {count} 个项目 Hook 标记为 untrusted。"
        }
        MessageId::SlashHooksProjectTrustClearedFooter => {
            "项目 Hook 信任存储已清除；当前会话项目 Hook 已取消信任。"
        }
        MessageId::SlashHooksProjectTrustClearFailedFooter => {
            "当前会话项目 Hook 已标记为 untrusted，但清除持久化信任存储失败: {error}"
        }
        MessageId::SlashHooksFeedbackUsageBody => "/hooks feedback noisy|useful <finding_id>",
        MessageId::SlashHooksFeedbackTitle => "Hook 反馈",
        MessageId::SlashHooksFeedbackFindingNotFoundBody => "本会话未找到 finding '{finding_id}'。",
        MessageId::SlashHooksFeedbackFindingNotFoundFooter => {
            "使用 /hooks history 复制最近的 finding id。"
        }
        MessageId::SlashHooksFeedbackRecordedTitle => "Hook 反馈已记录",
        MessageId::SlashHooksFeedbackRecordedBody => {
            "已为 finding '{finding_id}' 记录反馈 '{feedback}'。"
        }
        MessageId::SlashHooksFeedbackHookLine => "Hook: {hook_id}。",
        MessageId::SlashHooksFeedbackPolicyKeyLine => "策略 key: {key}。",
        MessageId::SlashHooksFeedbackPersistedFooter => "反馈已持久化，仅影响展示策略。",
        MessageId::SlashHooksFeedbackPersistenceFailedFooter => {
            "会话反馈已记录，但持久化失败: {error}"
        }
        MessageId::SlashHooksFeedbackClearedTitle => "Hook 反馈已清除",
        MessageId::SlashHooksFeedbackClearedBody => "已从本会话清除 {count} 条反馈偏好。",
        MessageId::SlashHooksFeedbackClearedFooter => "Hook 反馈偏好已清除。",
        MessageId::SlashHooksFeedbackClearFailedFooter => {
            "会话反馈已清除，但持久化存储清除失败: {error}"
        }
        _ => return None,
    })
}
