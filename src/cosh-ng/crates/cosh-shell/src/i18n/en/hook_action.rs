use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    match id {
        MessageId::SlashHooksActionCancelledTitle => Some("Hook action"),
        MessageId::SlashHooksActionCancelledBody => Some("Action cancelled."),
        MessageId::SlashHooksActionVerbEnable => Some("Enable"),
        MessageId::SlashHooksActionVerbDisable => Some("Disable"),
        MessageId::SlashHooksActionQuestion => {
            Some("Hook id '{id}' exists in both layers. {verb} which?")
        }
        MessageId::SlashHooksActionOptionShell => Some("Shell hook (session-level)"),
        MessageId::SlashHooksActionOptionAgent => Some("Agent hook (persistent)"),
        MessageId::SlashHooksActionOptionBoth => Some("Both"),
        MessageId::SlashHooksActionAgentEnabledBody => {
            Some("  Agent hook '{id}' enabled (persisted).")
        }
        MessageId::SlashHooksActionAgentDisabledBody => {
            Some("  Agent hook '{id}' disabled (persisted).")
        }
        MessageId::SlashHooksActionAgentErrorBody => Some("Agent hook '{id}' error: {error}"),
        _ => None,
    }
}
