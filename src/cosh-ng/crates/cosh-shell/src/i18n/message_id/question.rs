macro_rules! question_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            QuestionTitle,
            QuestionDefaultPrompt,
            QuestionAnswerLabel,
            QuestionSelectOneLabel,
            QuestionSelectMultipleLabel,
            QuestionOtherEmptyLabel,
            QuestionKeysPrefix,
            QuestionInstructionMoveTypeSend,
            QuestionInstructionMoveToggleSend,
            QuestionInstructionMoveSend,
            QuestionInstructionTypeSend,
            QuestionInstructionNoAnswer,
            QuestionNoPendingTitle,
            QuestionNoPendingBody,
        );
    };
}

macro_rules! question_interaction_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            QuestionDefaultGhost,
            QuestionRequiredGhost,
            QuestionInvalidGhost,
            QuestionSelectionRequired,
            QuestionSelectionRequiredWithText,
            QuestionAnswerNotSentTitle,
            QuestionAnswerNotSentBody,
        );
    };
}
