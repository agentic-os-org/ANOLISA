use crate::types::{AgentEvent, AgentRequest};

use super::{fake_stream_pacing_sleep, AdapterError};

pub(super) fn emit_fake_markdown_stream(
    input: &str,
    request: &AgentRequest,
    sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
) -> Result<bool, AdapterError> {
    let run_id = request.id.clone();
    if input.contains("stream markdown table") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "streaming".to_string(),
            message: "streaming markdown table fake response".to_string(),
        })?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "# Streaming table\n\n".to_string(),
        })?;
        fake_stream_pacing_sleep();
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "| 排名 | 进程 | RSS |\n".to_string(),
        })?;
        fake_stream_pacing_sleep();
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "| --- | --- | --- |\n| 1 | ps aux \\| grep cosh | ~42 MB |\n\nDone.".to_string(),
        })?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "stream markdown table fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream markdown paragraph") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "streaming".to_string(),
            message: "streaming markdown paragraph fake response".to_string(),
        })?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "# Streaming paragraph\n\nThis Agent answer starts\n".to_string(),
        })?;
        fake_stream_pacing_sleep();
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "and continues on another source line with 中文内容.\n\nDone.".to_string(),
        })?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "stream markdown paragraph fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream markdown") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "streaming".to_string(),
            message: "streaming markdown fake response".to_string(),
        })?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "# Streaming check\n\n".to_string(),
        })?;
        fake_stream_pacing_sleep();
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "- First item\n- Second item\n\n".to_string(),
        })?;
        fake_stream_pacing_sleep();
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "```bash\ncargo test --package cosh-shell\n```\n\nDone.".to_string(),
        })?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "stream markdown fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    Ok(false)
}
