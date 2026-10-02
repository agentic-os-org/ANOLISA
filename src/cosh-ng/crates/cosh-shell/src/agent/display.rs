use crate::i18n::{I18n, MessageId};

pub(super) fn display_agent_error(error: &str, i18n: &I18n) -> String {
    if error == "cosh-core-question-protocol:answer-write-failed" {
        format!(
            "{}\n{}",
            i18n.t(MessageId::AgentAnswerDeliveryUnknownTitle),
            i18n.t(MessageId::AgentAnswerDeliveryUnknownBody)
        )
    } else if error.starts_with("cosh-core-question-protocol:") {
        format!(
            "{}\n{}",
            i18n.t(MessageId::AgentQuestionUnavailableTitle),
            i18n.t(MessageId::AgentQuestionUnavailableBody)
        )
    } else if error == "analysis returned an error" {
        i18n.t(MessageId::AgentStatusAnalysisReturnedError)
            .to_string()
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_question_protocol_errors_do_not_expose_reason_codes() {
        let i18n = I18n::new(crate::Language::EnUs);
        let display = display_agent_error("cosh-core-question-protocol:missing-question", &i18n);
        assert_eq!(
            display,
            "Agent question unavailable\nThe Agent returned an incomplete question. Please retry."
        );
        assert!(!display.contains("missing-question"));

        let delivery =
            display_agent_error("cosh-core-question-protocol:answer-write-failed", &i18n);
        assert!(delivery.contains("Agent answer delivery uncertain"));
        assert!(!delivery.contains("answer-write-failed"));
    }
}
