use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    match id {
        MessageId::SlashHooksActionCancelledTitle => Some("Hook 操作"),
        MessageId::SlashHooksActionCancelledBody => Some("操作已取消。"),
        MessageId::SlashHooksActionVerbEnable => Some("启用"),
        MessageId::SlashHooksActionVerbDisable => Some("禁用"),
        MessageId::SlashHooksActionQuestion => {
            Some("Hook id '{id}' 同时存在于两层。{verb} 哪一层？")
        }
        MessageId::SlashHooksActionOptionShell => Some("Shell hook（会话级）"),
        MessageId::SlashHooksActionOptionAgent => Some("Agent hook（持久化）"),
        MessageId::SlashHooksActionOptionBoth => Some("两者"),
        MessageId::SlashHooksActionAgentEnabledBody => {
            Some("  Agent hook '{id}' 已启用（已持久化）。")
        }
        MessageId::SlashHooksActionAgentDisabledBody => {
            Some("  Agent hook '{id}' 已禁用（已持久化）。")
        }
        MessageId::SlashHooksActionAgentErrorBody => Some("Agent hook '{id}' 出错：{error}"),
        _ => None,
    }
}
