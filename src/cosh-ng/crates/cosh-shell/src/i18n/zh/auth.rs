use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    match id {
        MessageId::AuthSelectProviderQuestion => Some("\u{1f511} 需要认证 \u{2014} 选择 AI 服务："),
        MessageId::AuthProviderManagementQuestion => {
            Some("\u{1f511} 服务管理 \u{2014} 选择 AI 服务：")
        }
        MessageId::GuideAiManagementKeys => {
            Some("Enter 打开  Esc 返回  ↑↓\nF2/h 首页  Ctrl-D 回 AI  q 退出")
        }
        MessageId::GuideAuthDeleteKeys => {
            Some("↑↓ 选择  Enter 确认  Esc 返回  Ctrl-D 完成并返回向导")
        }
        MessageId::GuideAuthFieldKeys => {
            Some("输入  Enter 提交  Esc 返回\nF2 首页  Ctrl-D 回 AI 服务")
        }
        MessageId::GuideAuthPending => Some("正在确认结果…"),
        MessageId::GuideAuthTemplateTitle => Some("选择要添加的 AI 服务"),
        MessageId::GuideAuthDiscardTitle => Some("放弃未保存的修改？"),
        MessageId::GuideAuthDiscardBody => Some("当前表单有未保存的内容。要放弃修改并返回{destination}吗？"),
        MessageId::GuideAuthDiscardKeys => Some("Enter {action}  Esc 返回\n↑↓ 选择"),
        MessageId::GuideAuthDiscardAction => Some("放弃修改"),
        MessageId::GuideAuthContinueEditing => Some("继续编辑"),
        MessageId::GuideAuthContinueKey => Some("继续编辑"),
        MessageId::GuideAuthDestinationHome => Some("指南首页"),
        MessageId::GuideAuthDestinationOverview => Some("AI 服务概览"),
        MessageId::GuideAuthDestinationService => Some("当前服务的操作页"),
        MessageId::GuideAuthDestinationManagement => Some("AI 服务管理"),
        MessageId::GuideAuthDestinationTemplates => Some("服务模板列表"),
        MessageId::GuideAuthActivateTitle => Some("切换 AI 服务"),
        MessageId::GuideAuthActivateBody => Some("将 {service} 设为活动 AI 服务？Cosh 会核对结果再报告切换状态。"),
        MessageId::GuideAuthActivateAction => Some("切换服务"),
        MessageId::GuideAuthDeleteTitle => Some("删除 AI 服务"),
        MessageId::GuideAuthDeleteBody => Some("删除 {service}？这会移除 Cosh 中保存的服务和凭据，无法撤销，不会清理服务方记录。"),
        MessageId::GuideAuthDeleteAction => Some("删除服务"),
        MessageId::GuideAuthEnterField => Some("{service} · 输入{field}"),
        MessageId::GuideAuthEditField => Some("{service} · 编辑{field}"),
        MessageId::GuideAuthFieldHint => Some("提示：{value}"),
        MessageId::GuideAuthFieldError => Some("错误：{value}"),
        MessageId::GuideAuthKeepField => Some("按 Enter 保留当前值。"),
        MessageId::GuideAuthBaseUrl => Some(" Base URL"),
        MessageId::GuideAuthApiKey => Some(" API Key"),
        MessageId::GuideAuthModel => Some("模型 ID"),
        MessageId::GuideAuthBaseUrlHint => Some("输入服务的 OpenAI 兼容 API 地址，包含服务要求的 /v1 等路径。"),
        MessageId::GuideAuthApiKeyHint => Some("输入该服务的 API Key，屏幕上会遮掩输入。"),
        MessageId::GuideAuthModelHint => Some("输入服务提供的完整模型 ID。"),
        MessageId::GuideAuthValidationChecks => Some("保存前校验服务地址、凭据和模型。"),
        MessageId::GuideAuthValidationUsage => Some("如果只读校验无法确认接入，Cosh 可能向所选服务发出一次最小聊天请求，消耗少量额度或产生费用。"),
        MessageId::GuideWizardLoginPrivilegeDenied => {
            Some("管理员授权未完成，登录 Shell 未更改、未写入任何内容。可以重试；在终端里运行该登录命令可看到 sudo 的完整输出。")
        }
        MessageId::AuthEcsChecking => Some("正在检查 ECS RAM Role..."),
        MessageId::AuthEcsWaiting => Some("正在等待 ECS RAM Role 授权，配置完成后将自动继续。"),
        MessageId::AuthEcsRefreshing => Some("正在等待 ECS 凭据刷新，刷新后将自动继续配置。"),
        MessageId::AuthEcsRetry => Some("重新检查"),
        MessageId::AuthEcsCancelling => Some("正在停止 ECS 检查并回收资源..."),
        MessageId::AuthEcsCleanupFailed => Some("ECS 资源尚未回收完成，暂不能发起新的检查。"),
        MessageId::AuthEcsTimedOut => Some("等待 ECS 凭据已超时，自动检查已停止。"),
        MessageId::AuthEcsFailed => Some("ECS 认证检查失败。"),
        MessageId::AuthEcsSaving => Some("正在验证并保存 ECS 配置，此次提交不可取消。"),
        MessageId::AuthEcsUnknown => {
            Some("保存结果未确认，请先返回服务管理核实配置，不要重复提交。")
        }
        MessageId::AuthEcsReturn => Some("返回服务管理"),
        MessageId::AuthEcsCancelHint => Some("按 Esc 或 Ctrl+C 取消。"),
        _ => None,
    }
}
