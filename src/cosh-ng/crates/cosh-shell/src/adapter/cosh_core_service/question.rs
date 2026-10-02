//! User-question validation and lifecycle gates for persistent turns.

use std::sync::{mpsc, Arc, Mutex};

use crate::types::AgentEvent;

use super::super::claude::send_agent_event;
use super::super::cosh_core::question_ingress::{
    classify_output_line, protocol_error, CoreQuestionProtocolReason, CoshCoreOutputClass,
    CoshCoreQuestionGate, QuestionGateDecision,
};
use super::super::AdapterError;

pub(super) enum QuestionLineOutcome {
    Handled,
    PassThrough,
}

pub(super) fn handle_line(
    line: &str,
    gate: &Arc<Mutex<CoshCoreQuestionGate>>,
    run_id: &str,
    event_tx: &mpsc::Sender<Result<AgentEvent, AdapterError>>,
) -> Result<QuestionLineOutcome, AdapterError> {
    let question = match classify_output_line(line).map_err(protocol_error)? {
        CoshCoreOutputClass::ValidAskUser(question) => question,
        CoshCoreOutputClass::PassThrough => return Ok(QuestionLineOutcome::PassThrough),
    };
    let decision = gate
        .lock()
        .map_err(|_| protocol_error(CoreQuestionProtocolReason::InvalidControlShape))?
        .accept(&question)
        .map_err(protocol_error)?;
    if decision == QuestionGateDecision::Accept {
        send_agent_event(
            event_tx,
            AgentEvent::UserQuestion {
                run_id: run_id.to_string(),
                provider_request_id: Some(question.request_id),
                question: question.question,
                options: question.options,
                allow_free_text: question.allow_free_text,
                selection_mode: question.selection_mode,
            },
        );
    }
    Ok(QuestionLineOutcome::Handled)
}

pub(super) fn observe_event(
    gate: &Arc<Mutex<CoshCoreQuestionGate>>,
    event: &AgentEvent,
) -> Result<(), AdapterError> {
    gate.lock()
        .map_err(|_| protocol_error(CoreQuestionProtocolReason::InvalidControlShape))?
        .observe_terminal(event)
        .map_err(protocol_error)
}
