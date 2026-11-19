use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    match id {
        MessageId::AuthSelectProviderQuestion => Some("\u{1f511} 需要认证 \u{2014} 选择 AI 服务："),
        MessageId::AuthProviderManagementQuestion => {
            Some("\u{1f511} 服务管理 \u{2014} 选择 AI 服务：")
        }
        MessageId::GuideAiManagementKeys => {
            Some("↑↓ 选择  Enter 打开  Esc 返回  Ctrl-D 完成并返回向导")
        }
        MessageId::GuideAuthDeleteKeys => {
            Some("↑↓ 选择  Enter 确认  Esc 返回  Ctrl-D 完成并返回向导")
        }
        MessageId::GuideAuthFieldKeys => {
            Some("输入内容  Enter 提交  Esc 返回  Ctrl-D 完成并返回向导")
        }
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
