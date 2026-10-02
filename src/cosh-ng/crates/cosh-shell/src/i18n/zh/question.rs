use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::QuestionTitle => "Agent 问题",
        MessageId::QuestionDefaultPrompt => "Agent 需要你的输入",
        MessageId::QuestionAnswerLabel => "回答",
        MessageId::QuestionSelectOneLabel => "选择一项:",
        MessageId::QuestionSelectMultipleLabel => "选择一项或多项:",
        MessageId::QuestionOtherEmptyLabel => "其他...",
        MessageId::QuestionKeysPrefix => "按键: ",
        MessageId::QuestionInstructionMoveTypeSend => "左/右移动 | 输入回答 | Enter 发送",
        MessageId::QuestionInstructionMoveToggleSend => "左/右移动 | Space 切换 | Enter 发送",
        MessageId::QuestionInstructionMoveSend => "左/右移动 | Enter 发送",
        MessageId::QuestionInstructionTypeSend => "输入回答 | Enter 发送",
        MessageId::QuestionInstructionNoAnswer => "没有可选择的回答。",
        MessageId::QuestionNoPendingTitle => "没有待回答问题",
        MessageId::QuestionNoPendingBody => "当前没有等待回答的 Agent 问题。",
        MessageId::QuestionDefaultGhost => "请输入回答...",
        MessageId::QuestionRequiredGhost => "请先输入回答",
        MessageId::QuestionInvalidGhost => "请选择有效回答",
        MessageId::QuestionSelectionRequired => "请至少选择一项",
        MessageId::QuestionSelectionRequiredWithText => "请至少选择一项或输入回答",
        MessageId::QuestionAnswerNotSentTitle => "回答未发送",
        MessageId::QuestionAnswerNotSentBody => "问题仍在等待回答，请重试或按 Ctrl+C 取消。",
        _ => return None,
    })
}
