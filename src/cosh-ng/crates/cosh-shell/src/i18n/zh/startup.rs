use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::StartupTitle => "Cosh AI Shell",
        MessageId::StartupAdapterLine => {
            "Shell: {shell} · 审批: {approval}"
        }
        MessageId::StartupCwdLine => "cwd: {cwd}",
        MessageId::StartupCommandsLine => "可执行命令，也可直接提问。",
        MessageId::StartupAiDisabledLine => "AI 已关闭，仍可执行 Shell 命令。",
        MessageId::StartupHelpLabel => "帮助：",
        MessageId::StartupAskOnlyLine => "仅本次 --ask 使用 AI；后续输入仍保持 AI 关闭。",
        MessageId::StartupHooksRustProjectFinding => {
            "检测到 `Cargo.toml` Rust 项目；`/skill` 可查看面向项目的 Agent 能力。"
        }
        MessageId::StartupAuthHintLine => {
            "\u{1f4a1} ECS 上可直接提问体验 AI Shell；也可用 /auth 配置自选模型。"
        }
        MessageId::StartupUpgradeHintLine => {
            "\u{1f4e6} 发现 cosh-ng 更新：{current} → {latest}，运行 `{command}` 升级，完成后重新登录"
        }
        MessageId::StartupUpgradeNoticeTitle => "cosh-ng 有可用更新",
        _ => return None,
    })
}
