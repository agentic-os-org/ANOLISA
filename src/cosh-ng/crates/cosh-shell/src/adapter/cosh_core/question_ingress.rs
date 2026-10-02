use serde_json::Value;

use crate::types::{AgentEvent, QuestionSelectionMode};

use super::super::AdapterError;

const ERROR_PREFIX: &str = "cosh-core-question-protocol:";
const KNOWN_GENERIC_FALLBACK: &str = "Agent needs your input";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreQuestionProtocolReason {
    MissingRequestId,
    MissingQuestion,
    InvalidControlShape,
    KnownGenericFallback,
    NoAnswerPath,
    DuplicateConflict,
    ConcurrentQuestion,
    PrematureCompletion,
    AnswerWriteFailed,
}

impl CoreQuestionProtocolReason {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::MissingRequestId => "missing-request-id",
            Self::MissingQuestion => "missing-question",
            Self::InvalidControlShape => "invalid-control-shape",
            Self::KnownGenericFallback => "known-generic-fallback",
            Self::NoAnswerPath => "no-answer-path",
            Self::DuplicateConflict => "duplicate-conflict",
            Self::ConcurrentQuestion => "concurrent-question",
            Self::PrematureCompletion => "premature-completion",
            Self::AnswerWriteFailed => "answer-write-failed",
        }
    }
}

pub(crate) fn protocol_error(reason: CoreQuestionProtocolReason) -> AdapterError {
    AdapterError {
        message: format!("{ERROR_PREFIX}{}", reason.code()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedAskUser {
    pub(crate) request_id: String,
    pub(crate) question: String,
    pub(crate) options: Vec<String>,
    pub(crate) allow_free_text: bool,
    pub(crate) selection_mode: QuestionSelectionMode,
}

impl NormalizedAskUser {
    fn signature(&self) -> QuestionSignature {
        QuestionSignature {
            question: self.question.clone(),
            options: self.options.clone(),
            allow_free_text: self.allow_free_text,
            selection_mode: self.selection_mode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoshCoreOutputClass {
    PassThrough,
    ValidAskUser(NormalizedAskUser),
}

pub(crate) fn classify_output_line(
    line: &str,
) -> Result<CoshCoreOutputClass, CoreQuestionProtocolReason> {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
        return Ok(CoshCoreOutputClass::PassThrough);
    };
    if value.get("type").and_then(Value::as_str) != Some("control_request") {
        return Ok(CoshCoreOutputClass::PassThrough);
    }
    let Some(request) = value.get("request") else {
        return Ok(CoshCoreOutputClass::PassThrough);
    };
    if request.get("subtype").and_then(Value::as_str) != Some("ask_user") {
        return Ok(CoshCoreOutputClass::PassThrough);
    }

    let request_id = value
        .get("request_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(CoreQuestionProtocolReason::MissingRequestId)?
        .to_string();
    let question = request
        .get("question")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(CoreQuestionProtocolReason::MissingQuestion)?
        .to_string();
    if question == KNOWN_GENERIC_FALLBACK {
        return Err(CoreQuestionProtocolReason::KnownGenericFallback);
    }
    let allow_free_text = request
        .get("allow_free_text")
        .map(|value| {
            value
                .as_bool()
                .ok_or(CoreQuestionProtocolReason::InvalidControlShape)
        })
        .transpose()?
        .unwrap_or(true);
    let multi_select = request
        .get("multi_select")
        .map(|value| {
            value
                .as_bool()
                .ok_or(CoreQuestionProtocolReason::InvalidControlShape)
        })
        .transpose()?
        .unwrap_or(false);
    let options = normalize_options(request.get("options"))?;
    if !allow_free_text && options.is_empty() {
        return Err(CoreQuestionProtocolReason::NoAnswerPath);
    }

    Ok(CoshCoreOutputClass::ValidAskUser(NormalizedAskUser {
        request_id,
        question,
        options,
        allow_free_text,
        selection_mode: if multi_select {
            QuestionSelectionMode::Multiple
        } else {
            QuestionSelectionMode::Single
        },
    }))
}

fn normalize_options(options: Option<&Value>) -> Result<Vec<String>, CoreQuestionProtocolReason> {
    let Some(options) = options else {
        return Ok(Vec::new());
    };
    let items = options
        .as_array()
        .ok_or(CoreQuestionProtocolReason::InvalidControlShape)?;
    let mut normalized = Vec::new();
    for item in items {
        let label = if let Some(label) = item.as_str() {
            label
        } else if let Some(label) = item.get("label").and_then(Value::as_str) {
            label
        } else {
            return Err(CoreQuestionProtocolReason::InvalidControlShape);
        };
        let label = label.trim();
        if !label.is_empty() {
            normalized.push(label.to_string());
        }
    }
    Ok(normalized)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QuestionSignature {
    question: String,
    options: Vec<String>,
    allow_free_text: bool,
    selection_mode: QuestionSelectionMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InFlightQuestion {
    request_id: String,
    signature: QuestionSignature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuestionGateDecision {
    Accept,
    Duplicate,
}

#[derive(Debug, Default)]
pub(crate) struct CoshCoreQuestionGate {
    in_flight: Option<InFlightQuestion>,
    answered: Vec<InFlightQuestion>,
}

impl CoshCoreQuestionGate {
    pub(crate) fn accept(
        &mut self,
        question: &NormalizedAskUser,
    ) -> Result<QuestionGateDecision, CoreQuestionProtocolReason> {
        if let Some(answered) = self
            .answered
            .iter()
            .find(|answered| answered.request_id == question.request_id)
        {
            return if answered.signature == question.signature() {
                Ok(QuestionGateDecision::Duplicate)
            } else {
                Err(CoreQuestionProtocolReason::DuplicateConflict)
            };
        }
        let Some(current) = self.in_flight.as_ref() else {
            self.in_flight = Some(InFlightQuestion {
                request_id: question.request_id.clone(),
                signature: question.signature(),
            });
            return Ok(QuestionGateDecision::Accept);
        };
        if current.request_id == question.request_id {
            if current.signature == question.signature() {
                Ok(QuestionGateDecision::Duplicate)
            } else {
                Err(CoreQuestionProtocolReason::DuplicateConflict)
            }
        } else {
            Err(CoreQuestionProtocolReason::ConcurrentQuestion)
        }
    }

    pub(crate) fn answer_written(&mut self, request_id: &str) {
        if self
            .in_flight
            .as_ref()
            .is_some_and(|question| question.request_id == request_id)
        {
            if let Some(answered) = self.in_flight.take() {
                self.answered.push(answered);
            }
        }
    }

    pub(crate) fn observe_terminal(
        &mut self,
        event: &AgentEvent,
    ) -> Result<(), CoreQuestionProtocolReason> {
        match event {
            AgentEvent::AgentCompleted { .. } if self.in_flight.is_some() => {
                Err(CoreQuestionProtocolReason::PrematureCompletion)
            }
            AgentEvent::AgentFailed { .. } | AgentEvent::AgentCancelled { .. } => {
                self.in_flight = None;
                self.answered.clear();
                Ok(())
            }
            AgentEvent::AgentCompleted { .. } => {
                self.answered.clear();
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_question(id: &str, question: &str) -> NormalizedAskUser {
        NormalizedAskUser {
            request_id: id.to_string(),
            question: question.to_string(),
            options: vec!["One".to_string()],
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
        }
    }

    #[test]
    fn classifier_is_strict_only_for_ask_user() {
        assert_eq!(
            classify_output_line(r#"{"type":"assistant","message":"hi"}"#),
            Ok(CoshCoreOutputClass::PassThrough)
        );
        assert_eq!(
            classify_output_line(
                r#"{"type":"control_request","request_id":"x","request":{"subtype":"unknown"}}"#
            ),
            Ok(CoshCoreOutputClass::PassThrough)
        );
        assert_eq!(
            classify_output_line("not-json"),
            Ok(CoshCoreOutputClass::PassThrough)
        );
    }

    #[test]
    fn classifier_rejects_incomplete_and_unanswerable_questions() {
        let missing_id =
            r#"{"type":"control_request","request":{"subtype":"ask_user","question":"Choose"}}"#;
        assert_eq!(
            classify_output_line(missing_id),
            Err(CoreQuestionProtocolReason::MissingRequestId)
        );
        let missing_question =
            r#"{"type":"control_request","request_id":"q","request":{"subtype":"ask_user"}}"#;
        assert_eq!(
            classify_output_line(missing_question),
            Err(CoreQuestionProtocolReason::MissingQuestion)
        );
        let no_answer = r#"{"type":"control_request","request_id":"q","request":{"subtype":"ask_user","question":"Choose","allow_free_text":false,"options":[{"label":"  "}]}}"#;
        assert_eq!(
            classify_output_line(no_answer),
            Err(CoreQuestionProtocolReason::NoAnswerPath)
        );
        let fallback = r#"{"type":"control_request","request_id":"q","request":{"subtype":"ask_user","question":"Agent needs your input"}}"#;
        assert_eq!(
            classify_output_line(fallback),
            Err(CoreQuestionProtocolReason::KnownGenericFallback)
        );
        for invalid in [
            r#"{"type":"control_request","request_id":7,"request":{"subtype":"ask_user","question":"Choose"}}"#,
            r#"{"type":"control_request","request_id":"q","request":{"subtype":"ask_user","question":7}}"#,
            r#"{"type":"control_request","request_id":"q","request":{"subtype":"ask_user","question":"Choose","allow_free_text":"yes"}}"#,
            r#"{"type":"control_request","request_id":"q","request":{"subtype":"ask_user","question":"Choose","multi_select":1}}"#,
            r#"{"type":"control_request","request_id":"q","request":{"subtype":"ask_user","question":"Choose","options":{}}}"#,
            r#"{"type":"control_request","request_id":"q","request":{"subtype":"ask_user","question":"Choose","options":[{"label":7}]}}"#,
        ] {
            assert!(classify_output_line(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn classifier_normalizes_labels_without_nested_reconstruction() {
        let line = r#"{"type":"control_request","request_id":" q ","request":{"subtype":"ask_user","question":" Choose ","allow_free_text":false,"options":[" One ",{"label":" Two "},{"label":"  "}]}}"#;
        let CoshCoreOutputClass::ValidAskUser(question) =
            classify_output_line(line).expect("valid question")
        else {
            panic!("expected question");
        };
        assert_eq!(question.request_id, "q");
        assert_eq!(question.question, "Choose");
        assert_eq!(question.options, vec!["One", "Two"]);

        let nested = r#"{"type":"control_request","request_id":"q","request":{"subtype":"ask_user","questions":[{"question":"Nested"}]}}"#;
        assert_eq!(
            classify_output_line(nested),
            Err(CoreQuestionProtocolReason::MissingQuestion)
        );
    }

    #[test]
    fn gate_deduplicates_and_rejects_conflicts() {
        let mut gate = CoshCoreQuestionGate::default();
        let q1 = valid_question("q1", "Choose");
        assert_eq!(gate.accept(&q1), Ok(QuestionGateDecision::Accept));
        assert_eq!(gate.accept(&q1), Ok(QuestionGateDecision::Duplicate));
        assert_eq!(
            gate.accept(&valid_question("q1", "Changed")),
            Err(CoreQuestionProtocolReason::DuplicateConflict)
        );
        assert_eq!(
            gate.accept(&valid_question("q2", "Second")),
            Err(CoreQuestionProtocolReason::ConcurrentQuestion)
        );
        assert_eq!(
            gate.observe_terminal(&AgentEvent::AgentCompleted {
                run_id: "run".to_string(),
                summary: "done".to_string(),
            }),
            Err(CoreQuestionProtocolReason::PrematureCompletion)
        );
        gate.answer_written("q1");
        assert_eq!(gate.accept(&q1), Ok(QuestionGateDecision::Duplicate));
        assert_eq!(
            gate.accept(&valid_question("q1", "Changed after answer")),
            Err(CoreQuestionProtocolReason::DuplicateConflict)
        );
        assert_eq!(
            gate.accept(&valid_question("q2", "Second")),
            Ok(QuestionGateDecision::Accept)
        );
        assert!(gate
            .observe_terminal(&AgentEvent::AgentFailed {
                run_id: "run".to_string(),
                error: "failed".to_string(),
                error_code: None,
                max_turns: None,
            })
            .is_ok());
        assert_eq!(
            gate.accept(&valid_question("q3", "Third")),
            Ok(QuestionGateDecision::Accept)
        );
        assert!(gate
            .observe_terminal(&AgentEvent::AgentCancelled {
                run_id: "run".to_string(),
                reason: "cancelled".to_string(),
            })
            .is_ok());
        assert!(gate
            .observe_terminal(&AgentEvent::AgentCompleted {
                run_id: "run".to_string(),
                summary: "done".to_string(),
            })
            .is_ok());
    }
}
