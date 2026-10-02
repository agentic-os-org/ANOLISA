use crate::question::runtime::{QuestionAnswerRun, RuntimeUserQuestion};
use crate::runtime::prelude::*;

pub(crate) enum QuestionAnswerResolution {
    Accepted(Box<QuestionAnswerRun>),
    EmptyAnswer,
    SelectionRequired,
    InvalidAnswer,
    RequestBuildFailed,
    NoPending,
    Ignored,
}

pub(crate) fn resolve_pending_question_answer(
    event: &ShellEvent,
    sequence: usize,
    state: &mut InlineState,
) -> QuestionAnswerResolution {
    let Some(question_id) = state.questions.pending_id.clone() else {
        return QuestionAnswerResolution::NoPending;
    };
    let Some(question_index) = state
        .questions
        .items
        .iter()
        .position(|question| question.id == question_id && question.answer.is_none())
    else {
        return QuestionAnswerResolution::NoPending;
    };
    if event.message.as_deref() == Some("question_submit_empty")
        && event.input.as_deref().map(str::trim) != Some(question_id.as_str())
    {
        return QuestionAnswerResolution::Ignored;
    }
    let Some(raw_answer) = question_answer_text_from_event(event) else {
        return QuestionAnswerResolution::InvalidAnswer;
    };
    let question = &state.questions.items[question_index];
    let answer = match resolve_question_answer(question, &raw_answer) {
        Ok(answer) => answer,
        Err(rejection) => return rejection,
    };
    let Some(mut request) = agent_request_from_intercepted_input(event, sequence, true) else {
        return QuestionAnswerResolution::RequestBuildFailed;
    };

    let question_text = question.question.clone();
    let origin = question.origin;
    let user_input =
        format!("Answer to pending Agent question: {question_text}\nUser answer: {answer}");
    request.id = format!("agent-answer-{question_id}-{sequence}");
    request.command_block.id = format!("answer-{question_id}-{sequence}");
    request.command_block.command = user_input.clone();
    request.user_input = Some(user_input);

    let provider_request_id = question.provider_request_id.clone();
    let provider_owner_request_id = question.provider_owner_request_id.clone();
    state.questions.items[question_index].answer = Some(answer.clone());
    state.questions.pending_id = None;

    QuestionAnswerResolution::Accepted(Box::new(QuestionAnswerRun {
        question_id,
        question: question_text,
        answer,
        provider_request_id,
        provider_owner_request_id,
        request,
        origin,
    }))
}

fn question_answer_text_from_event(event: &ShellEvent) -> Option<String> {
    if event.component.as_deref() != Some("card") {
        return None;
    }
    match event.message.as_deref() {
        Some("answer") => Some(event.input.as_deref().unwrap_or_default().to_string()),
        Some("question_submit_empty") => Some(String::new()),
        _ => None,
    }
}

fn resolve_question_answer(
    question: &RuntimeUserQuestion,
    raw_answer: &str,
) -> Result<String, QuestionAnswerResolution> {
    if question.selection_mode == QuestionSelectionMode::Multiple {
        return resolve_multi_question_answer(question, raw_answer);
    }

    let answer = raw_answer.trim();
    if answer.is_empty() {
        return Err(QuestionAnswerResolution::EmptyAnswer);
    }
    if let Ok(index) = answer.parse::<usize>() {
        return question
            .options
            .get(index.saturating_sub(1))
            .cloned()
            .ok_or(QuestionAnswerResolution::InvalidAnswer);
    }
    if let Some(option) = question
        .options
        .iter()
        .find(|option| option.eq_ignore_ascii_case(answer))
    {
        return Ok(option.clone());
    }
    if question.allow_free_text {
        Ok(answer.to_string())
    } else {
        Err(QuestionAnswerResolution::InvalidAnswer)
    }
}

fn resolve_multi_question_answer(
    question: &RuntimeUserQuestion,
    raw_answer: &str,
) -> Result<String, QuestionAnswerResolution> {
    let (indices_text, custom_answer) = raw_answer
        .split_once('\n')
        .map(|(indices, custom)| (indices.trim(), custom.trim()))
        .unwrap_or((raw_answer.trim(), ""));
    if indices_text.is_empty() && custom_answer.is_empty() {
        return Err(QuestionAnswerResolution::SelectionRequired);
    }
    let indices = indices_text
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::parse::<usize>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| QuestionAnswerResolution::InvalidAnswer)?;
    let mut answers = Vec::new();
    for index in indices {
        let Some(option) = question.options.get(index.saturating_sub(1)) else {
            return Err(QuestionAnswerResolution::InvalidAnswer);
        };
        answers.push(option.clone());
    }
    if !custom_answer.is_empty() {
        if !question.allow_free_text {
            return Err(QuestionAnswerResolution::InvalidAnswer);
        }
        answers.push(custom_answer.to_string());
    }
    if answers.is_empty() {
        Err(QuestionAnswerResolution::SelectionRequired)
    } else {
        Ok(answers.join(", "))
    }
}
