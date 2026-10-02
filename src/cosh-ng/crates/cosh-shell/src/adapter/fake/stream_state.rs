use std::thread;
use std::time::Duration;

use crate::types::{AgentEvent, AgentRequest, QuestionSelectionMode};

use super::AdapterError;

pub(super) fn emit_fake_stale_question_stream(
    input: &str,
    request: &AgentRequest,
    sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
) -> Result<bool, AdapterError> {
    if !input.contains("stream stale question") {
        return Ok(false);
    }

    let run_id = request.id.clone();
    sink(AgentEvent::StatusChanged {
        run_id: run_id.clone(),
        phase: "question".to_string(),
        message: "streaming fake question request".to_string(),
    })?;
    sink(AgentEvent::UserQuestion {
        run_id: run_id.clone(),
        provider_request_id: None,
        question: "Choose a color for the next step".to_string(),
        options: vec!["Green".to_string(), "Blue".to_string()],
        allow_free_text: true,
        selection_mode: QuestionSelectionMode::Single,
    })?;
    thread::sleep(Duration::from_millis(800));
    sink(AgentEvent::TextDelta {
        run_id: run_id.clone(),
        text: "STALE QUESTION TEXT SHOULD NOT RENDER".to_string(),
    })?;
    sink(AgentEvent::AgentCompleted {
        run_id,
        summary: "stale question fake analysis completed".to_string(),
    })?;
    Ok(true)
}

pub(super) fn emit_fake_late_card_or_artifact_stream(
    input: &str,
    request: &AgentRequest,
    sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
) -> Result<bool, AdapterError> {
    if !input.contains("late card after cancel") && !input.contains("late artifact after cancel") {
        return Ok(false);
    }

    let run_id = request.id.clone();
    sink(AgentEvent::StatusChanged {
        run_id: run_id.clone(),
        phase: "thinking".to_string(),
        message: "waiting before fake late event".to_string(),
    })?;
    thread::sleep(Duration::from_millis(900));

    if input.contains("late card after cancel") {
        sink(AgentEvent::UserQuestion {
            run_id: run_id.clone(),
            provider_request_id: None,
            question: "LATE QUESTION SHOULD NOT RENDER".to_string(),
            options: vec!["Yes".to_string(), "No".to_string()],
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
        })?;
    } else {
        sink(AgentEvent::ToolOutputDelta {
            run_id: run_id.clone(),
            tool_id: "late-tool".to_string(),
            stream: "stderr".to_string(),
            text: "LATE TOOL ARTIFACT SHOULD NOT RENDER".to_string(),
        })?;
        sink(AgentEvent::ToolCompleted {
            run_id: run_id.clone(),
            tool_id: "late-tool".to_string(),
            status: "error".to_string(),
        })?;
    }

    sink(AgentEvent::AgentCompleted {
        run_id,
        summary: "late fake event completed".to_string(),
    })?;
    Ok(true)
}

pub(super) fn emit_fake_slow_stream(
    input: &str,
    request: &AgentRequest,
    sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
) -> Result<(), AdapterError> {
    let run_id = request.id.clone();
    if input.contains("text then wait") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "thinking".to_string(),
            message: "simulating text before completion".to_string(),
        })?;
        thread::sleep(Duration::from_millis(100));
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: format!("Slow fake response for: {input}"),
        })?;
        thread::sleep(Duration::from_millis(7_000));
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "slow text wait fake analysis completed".to_string(),
        })?;
        return Ok(());
    }
    if input.contains("unclosed request then wait") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "thinking".to_string(),
            message: "simulating unclosed request before cancellation".to_string(),
        })?;
        thread::sleep(Duration::from_millis(100));
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "before hidden request\n```cosh-request\nhistory\n".to_string(),
        })?;
        thread::sleep(Duration::from_millis(7_000));
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "unclosed request wait fake analysis completed".to_string(),
        })?;
        return Ok(());
    }

    sink(AgentEvent::StatusChanged {
        run_id: run_id.clone(),
        phase: "thinking".to_string(),
        message: "simulating a slow fake Agent run".to_string(),
    })?;
    let delay = if input.contains("hold test") {
        Duration::from_millis(1800)
    } else if input.contains("very slow") {
        Duration::from_millis(1500)
    } else {
        Duration::from_millis(500)
    };
    thread::sleep(delay);
    sink(AgentEvent::TextDelta {
        run_id: run_id.clone(),
        text: format!("Slow fake response for: {input}"),
    })?;
    sink(AgentEvent::AgentCompleted {
        run_id,
        summary: "slow fake analysis completed".to_string(),
    })?;
    Ok(())
}
