use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::StartupTitle => "cosh-shell",
        MessageId::StartupAdapterLine => {
            "后端: {adapter} · Shell: {shell} · 审批: {approval} · 分析: {analysis}"
        }
        MessageId::StartupCwdLine => "cwd: {cwd}",
        MessageId::StartupCommandsLine => "/help · /mode · /hooks",
        MessageId::StartupHooksNoneSummary => "启动 hooks: 未配置。",
        MessageId::StartupHooksCompletedSummary => "启动 hooks: 内置只读检查已完成。",
        MessageId::StartupHooksFindingsHeading => "启动检查结果",
        MessageId::StartupHooksRustProjectFinding => {
            "检测到 `Cargo.toml` Rust 项目；`/skill` 可查看面向项目的 Agent 能力。"
        }
        MessageId::StartupHooksNoFindings => "内置只读检查未发现启动项。",
        MessageId::StartupHooksReadOnlyNote => "`cosh-shell` 只检查了轻量启动上下文。",
        MessageId::StartupSwitchHint => {
            "\u{1f4a1} 运行 \"cosh-switch\" 可在 cosh-ng 与 copilot-shell 之间切换"
        }
        MessageId::StartupAuthHintLine => {
            "\u{1f4a1} 尚未配置 AI：运行 /auth 即可解锁自然语言提问、失败分析与个性化推荐"
        }
        _ => return None,
    })
}
