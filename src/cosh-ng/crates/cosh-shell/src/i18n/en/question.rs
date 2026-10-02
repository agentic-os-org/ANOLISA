use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::QuestionTitle => "Agent question",
        MessageId::QuestionDefaultPrompt => "Agent needs your input",
        MessageId::QuestionAnswerLabel => "Answer",
        MessageId::QuestionSelectOneLabel => "Select one:",
        MessageId::QuestionSelectMultipleLabel => "Select one or more:",
        MessageId::QuestionOtherEmptyLabel => "Other...",
        MessageId::QuestionKeysPrefix => "Keys: ",
        MessageId::QuestionInstructionMoveTypeSend => "Left/Right move | type answer | Enter send",
        MessageId::QuestionInstructionMoveToggleSend => {
            "Left/Right move | Space toggle | Enter send"
        }
        MessageId::QuestionInstructionMoveSend => "Left/Right move | Enter send",
        MessageId::QuestionInstructionTypeSend => "Type answer | Enter send",
        MessageId::QuestionInstructionNoAnswer => "No selectable answer is available.",
        MessageId::QuestionNoPendingTitle => "No pending question",
        MessageId::QuestionNoPendingBody => "There is no Agent question waiting for an answer.",
        MessageId::QuestionDefaultGhost => "Type your answer...",
        MessageId::QuestionRequiredGhost => "Please enter an answer",
        MessageId::QuestionInvalidGhost => "Choose a valid answer",
        MessageId::QuestionSelectionRequired => "Select at least one option",
        MessageId::QuestionSelectionRequiredWithText => {
            "Select at least one option or enter an answer"
        }
        MessageId::QuestionAnswerNotSentTitle => "Answer not sent",
        MessageId::QuestionAnswerNotSentBody => {
            "The question is still pending. Retry or press Ctrl+C to cancel."
        }
        // Registry slash commands
        _ => return None,
    })
}
