use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::DebugSessionTitle => "会话调试",
        MessageId::DebugAdapterLine => "适配器: {value}",
        MessageId::DebugProviderInvocationLine => "provider 调用路径: {value}",
        MessageId::DebugProviderCommittedSessionLine => "provider 已提交会话: {value}",
        MessageId::DebugActiveRunLine => "活跃运行: {value}",
        MessageId::DebugQueuedRunsLine => "排队运行: {value}",
        MessageId::DebugProviderPendingSessionLine => "provider 待提交会话: {value}",
        MessageId::DebugProviderInitializeSeenLine => "provider initialize 已收到: {value}",
        MessageId::DebugHostExecutedShellResultLine => "host-executed shell 结果: {value}",
        MessageId::DebugSelectedShellExecutionPathLine => "已选择 shell 执行路径: {value}",
        MessageId::DebugLatestProviderRequestLine => "最近 provider request: {value}",
        MessageId::DebugLatestToolUseLine => "最近 tool use id: {value}",
        MessageId::DebugLatestRecoveryStatusLine => "最近恢复状态: {value}",
        MessageId::DebugLatestRecoveryReasonLine => "最近恢复原因: {value}",
        MessageId::DebugEvidenceAccessLine => "evidence 访问方式: {value}",
        MessageId::DebugEvidenceToolRegisteredLine => "evidence tool 已注册: {value}",
        MessageId::DebugEvidenceNamespaceLine => "当前 evidence namespace: {value}",
        MessageId::DebugEvidenceLedgerCountLine => "evidence ledger 命令数: {value}",
        MessageId::DebugLatestShellOutputReadLine => "最近 shell evidence action: {value}",
        MessageId::DebugUnknownTargetBody => "未知 debug 目标: {target}",
        MessageId::DebugUnknownTargetFooter => "使用 /debug session。",
        MessageId::CommandRemovedTitle => "命令已移除",
        MessageId::RemovedDecisionCommandBody => "{command} 不再作为输入命令支持。",
        MessageId::RemovedApprovalDecisionFooter => {
            "请使用审批卡片按钮；本次输入没有发送到 shell。"
        }
        MessageId::RemovedQuestionAnswerFooter => "请在问题卡片中回答；本次输入没有发送到 shell。",
        _ => return None,
    })
}
