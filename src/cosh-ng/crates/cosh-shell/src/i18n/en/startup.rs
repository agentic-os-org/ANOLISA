use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::StartupTitle => "cosh-shell",
        MessageId::StartupAdapterLine => {
            "Adapter: {adapter} · Shell: {shell} · Approval: {approval} · Analysis: {analysis}"
        }
        MessageId::StartupCwdLine => "cwd: {cwd}",
        MessageId::StartupCommandsLine => "/help · /mode · /hooks",
        MessageId::StartupHooksNoneSummary => "Startup hooks: none configured.",
        MessageId::StartupHooksCompletedSummary => {
            "Startup hooks: built-in read-only checks completed."
        }
        MessageId::StartupHooksFindingsHeading => "Startup findings",
        MessageId::StartupHooksRustProjectFinding => {
            "Rust project detected from `Cargo.toml`; `/skill` can show project-oriented Agent capabilities."
        }
        MessageId::StartupHooksNoFindings => {
            "No startup findings from built-in read-only checks."
        }
        MessageId::StartupHooksReadOnlyNote => {
            "`cosh-shell` only inspected lightweight startup context."
        }
        MessageId::StartupSwitchHint => {
            "\u{1f4a1} Run \"cosh-switch\" to switch between cosh-ng and copilot-shell"
        }
        MessageId::StartupAuthHintLine => {
            "\u{1f4a1} AI not configured — run /auth to unlock AI help, failure analysis, tips"
        }
        _ => return None,
    })
}
