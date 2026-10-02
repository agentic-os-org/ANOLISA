use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::RecommendationTitle => "Recommendations",
        MessageId::RecommendationNextStepTitle => "Suggested next step",
        MessageId::AnalysisResultTitle => "Analysis result",
        MessageId::RecommendationEmptyBody => "No command recommendations",
        MessageId::RecommendationFooter => "display-only: no command was executed",
        MessageId::RecommendationNoSelectableTitle => "No selectable recommendation",
        MessageId::RecommendationNoSelectableBody => {
            "No selectable recommendation is available yet"
        }
        MessageId::RecommendationUnavailableTitle => "Recommendation unavailable",
        MessageId::RecommendationUnavailableBody => {
            "Recommendation {index} is not available; choose 1..{total}"
        }
        MessageId::RecommendationSelectedTitle => "Recommendation selected",
        MessageId::RecommendationSelectedBody => "Selected recommendation {index}",
        MessageId::RecommendationCopiedTitle => "Recommendation copy",
        MessageId::RecommendationCopiedBody => "Copy recommendation {index}",
        MessageId::RecommendationInsertTitle => "Recommendation insert",
        MessageId::RecommendationInsertBody => "Prepared recommendation {index} for manual input",
        MessageId::RecommendationDetailsTitle => "Recommendation details",
        MessageId::RecommendationDetailsBody => "Details for recommendation {index}",
        MessageId::RecommendationDisplayOnlyBody => {
            "Display-only: command was not executed; copy or re-enter it to run"
        }
        MessageId::RecommendationCopyOnlyBody => {
            "Copy-only: command was shown for copying; it was not executed."
        }
        MessageId::RecommendationInsertOnlyBody => {
            "Insert is pending editable input only; nothing was submitted or written to the child shell."
        }
        MessageId::RecommendationDetailsOnlyBody => {
            "Details-only: inspect the command before deciding whether to type or copy it."
        }
        _ => return None,
    })
}
