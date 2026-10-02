use crate::runtime::state::InlineState;
use crate::types::QuestionSelectionMode;

use super::runtime::RuntimeUserQuestion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreQuestionStoreDecision {
    Accept,
    Duplicate,
    Reject(&'static str),
}

pub(crate) struct IncomingQuestion<'a> {
    pub(crate) provider_request_id: Option<&'a str>,
    pub(crate) question: &'a str,
    pub(crate) options: &'a [String],
    pub(crate) allow_free_text: bool,
    pub(crate) selection_mode: QuestionSelectionMode,
}

pub(crate) fn core_question_store_decision(
    state: &InlineState,
    incoming: IncomingQuestion<'_>,
) -> CoreQuestionStoreDecision {
    let strict_core = state
        .agent_run
        .active
        .as_ref()
        .is_some_and(|run| run.provider_name == "cosh-core")
        && incoming.provider_request_id.is_some();
    if !strict_core {
        return CoreQuestionStoreDecision::Accept;
    }
    let Some(pending_id) = state.questions.pending_id.as_ref() else {
        return CoreQuestionStoreDecision::Accept;
    };
    let Some(existing) = state
        .questions
        .items
        .iter()
        .find(|question| question.id == *pending_id && question.answer.is_none())
    else {
        return CoreQuestionStoreDecision::Accept;
    };
    compare_existing(existing, &incoming)
}

pub(crate) fn reject_core_question_store(state: &mut InlineState) -> bool {
    if state.questions.question_protocol_failure_reported {
        return false;
    }
    state.questions.question_protocol_failure_reported = true;
    let Some(active_run) = state.agent_run.active.as_mut() else {
        return false;
    };
    active_run.handle.cancel();
    true
}

fn compare_existing(
    existing: &RuntimeUserQuestion,
    incoming: &IncomingQuestion<'_>,
) -> CoreQuestionStoreDecision {
    if existing.provider_request_id.as_deref() == incoming.provider_request_id {
        if same_signature(existing, incoming) {
            CoreQuestionStoreDecision::Duplicate
        } else {
            CoreQuestionStoreDecision::Reject("duplicate-conflict")
        }
    } else {
        CoreQuestionStoreDecision::Reject("concurrent-question")
    }
}

fn same_signature(existing: &RuntimeUserQuestion, incoming: &IncomingQuestion<'_>) -> bool {
    existing.question == incoming.question
        && existing.options == incoming.options
        && existing.allow_free_text == incoming.allow_free_text
        && existing.selection_mode == incoming.selection_mode
}

#[cfg(test)]
mod tests {
    use crate::runtime::prelude::AgentRunOrigin;
    use crate::ui::QuestionInputFeedback;

    use super::*;

    fn existing() -> RuntimeUserQuestion {
        RuntimeUserQuestion {
            id: "q-1".to_string(),
            question: "Choose".to_string(),
            options: vec!["One".to_string()],
            selected_option: 0,
            selected_options: Vec::new(),
            custom_answer: String::new(),
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::None,
            provider_request_id: Some("provider-q".to_string()),
            provider_owner_request_id: Some("owner".to_string()),
            origin: AgentRunOrigin::Standard,
            answer: None,
        }
    }

    #[test]
    fn store_comparison_deduplicates_without_overwriting_pending() {
        let options = vec!["One".to_string()];
        let incoming = IncomingQuestion {
            provider_request_id: Some("provider-q"),
            question: "Choose",
            options: &options,
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
        };
        assert_eq!(
            compare_existing(&existing(), &incoming),
            CoreQuestionStoreDecision::Duplicate
        );

        let changed = IncomingQuestion {
            question: "Changed",
            ..incoming
        };
        assert_eq!(
            compare_existing(&existing(), &changed),
            CoreQuestionStoreDecision::Reject("duplicate-conflict")
        );
        let concurrent = IncomingQuestion {
            provider_request_id: Some("provider-q2"),
            ..changed
        };
        assert_eq!(
            compare_existing(&existing(), &concurrent),
            CoreQuestionStoreDecision::Reject("concurrent-question")
        );
    }
}
