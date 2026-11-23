use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::StartupTitle => "Cosh AI Shell",
        MessageId::StartupAdapterLine => {
            "Shell: {shell} · approval: {approval}"
        }
        MessageId::StartupCwdLine => "cwd: {cwd}",
        MessageId::StartupCommandsLine => "Run commands or ask AI questions.",
        MessageId::StartupAiDisabledLine => "AI is disabled; Shell commands remain available.",
        MessageId::StartupHelpLabel => "Help:",
        MessageId::StartupAskOnlyLine => "Only this --ask request uses AI; AI remains off for subsequent input.",
        MessageId::StartupHooksRustProjectFinding => {
            "Rust project detected from `Cargo.toml`; `/skill` can show project-oriented Agent capabilities."
        }
        MessageId::StartupAuthHintLine => {
            "Ask a question and follow the AI setup prompts · /auth to configure a model"
        }
        MessageId::StartupEcsAuthHintLine => "Ask directly to start an AI conversation",
        MessageId::StartupConfigureAuthHintLine => "Configure a model with /auth to start an AI conversation",
        MessageId::StartupUpgradeHintLine => {
            "\u{1f4e6} Update available: cosh-ng {current} → {latest} — run `{command}` to upgrade, then log in again"
        }
        MessageId::StartupUpgradeNoticeTitle => "cosh-ng update available",
        _ => return None,
    })
}
