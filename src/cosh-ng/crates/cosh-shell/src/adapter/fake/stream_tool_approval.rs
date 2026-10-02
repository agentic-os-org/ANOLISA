use std::thread;
use std::time::Duration;

use crate::types::{AgentEvent, AgentRequest};

use super::AdapterError;

pub(super) fn emit_fake_tool_approval_stream(
    input: &str,
    request: &AgentRequest,
    sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
) -> Result<bool, AdapterError> {
    let run_id = request.id.clone();
    if input.contains("stream batch tool approval") {
        // Multi-request turn for turn-scope batch consent (issue #1773):
        // three distinct diagnostic commands queued in one run.
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "streaming".to_string(),
            message: "streaming fake batch approval requests".to_string(),
        })?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a batch of streamed tool requests before finishing.".to_string(),
        })?;
        // Emit the three requests back-to-back so they land in the same
        // governed-event batch: the first card already sees a multi-request
        // turn and offers "Allow all this turn". Portable medium-risk
        // commands keep the scripted-PTY case green on macOS and Linux.
        thread::sleep(Duration::from_millis(100));
        for command in [
            "ps aux | head",
            "ps aux | head -20",
            "printf 'alpha\\nbeta\\n' | awk '\n{ print $0 }\n'",
        ] {
            sink(AgentEvent::ToolCall {
                run_id: run_id.clone(),
                tool_id: None,
                name: "Bash".to_string(),
                input: command.to_string(),
            })?;
        }
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "batch stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream pwd tool approval") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "streaming".to_string(),
            message: "streaming fake pwd approval request".to_string(),
        })?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a streamed pwd request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "pwd")?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "stream pwd approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream stale tool approval") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "streaming".to_string(),
            message: "streaming fake stale approval request".to_string(),
        })?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a command before approval.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "pwd")?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "STALE APPROVAL TEXT SHOULD NOT RENDER".to_string(),
        })?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "stream stale approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream delayed tool approval") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "streaming".to_string(),
            message: "streaming fake delayed approval request".to_string(),
        })?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a delayed streamed tool request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "sleep 1; echo a; sleep 1; echo b")?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "delayed stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream stderr tool approval") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "streaming".to_string(),
            message: "streaming fake stderr approval request".to_string(),
        })?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a stderr streamed tool request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "printf 'out\\n'; printf 'err\\n' >&2")?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "stderr stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream sudo tool approval") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "streaming".to_string(),
            message: "streaming fake sudo approval request".to_string(),
        })?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a sudo streamed tool request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "sudo printf approved-sudo")?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "sudo stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream ssh tool approval") {
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing an ssh streamed tool request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "ssh fake-host")?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "ssh stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream pager tool approval") {
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a pager streamed tool request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "fake-pager")?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "pager stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    // Issue #1988: forensics command whose Git configuration asks for a pager.
    // The global `-c`/`--paginate` flags keep it out of the readonly pipeline,
    // so it travels the approval + foreground handoff path under test.
    if input.contains("stream git pager tool approval") {
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a git log request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(
            sink,
            &run_id,
            "git -c core.pager=fake-pager --paginate log -1 --format=%h",
        )?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "git pager stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream less tool approval") {
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a less request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "less Cargo.toml")?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "less stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream repl tool approval") {
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a REPL streamed tool request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "fake-repl")?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "REPL stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream multiline tool approval") {
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a multiline streamed tool request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "printf one\nprintf two")?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "multiline stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream quoted multiline pipeline approval") {
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a quoted multiline pipeline request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(
            sink,
            &run_id,
            "printf 'alpha\\nbeta\\n' | awk '\n{ print $0 }\n'",
        )?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "quoted multiline pipeline fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream tool approval") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "streaming".to_string(),
            message: "streaming fake approval request".to_string(),
        })?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a streamed tool request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "git status --short")?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream long tool approval") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "streaming".to_string(),
            message: "streaming fake long approval request".to_string(),
        })?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a long-running streamed tool request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "sleep 4; printf done")?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "long stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("stream piped tool approval")
        || input.contains("stream blocked tool approval")
    {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "streaming".to_string(),
            message: "streaming fake piped approval request".to_string(),
        })?;
        sink(AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text: "Preparing a piped streamed tool request before finishing.".to_string(),
        })?;
        emit_bash_tool_after_short_delay(sink, &run_id, "ps aux | head")?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "piped stream approval fake analysis completed".to_string(),
        })?;
        return Ok(true);
    }

    Ok(false)
}

fn emit_bash_tool_after_short_delay(
    sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
    run_id: &str,
    input: &str,
) -> Result<(), AdapterError> {
    thread::sleep(Duration::from_millis(100));
    sink(AgentEvent::ToolCall {
        run_id: run_id.to_string(),
        tool_id: None,
        name: "Bash".to_string(),
        input: input.to_string(),
    })?;
    thread::sleep(Duration::from_millis(800));
    Ok(())
}
