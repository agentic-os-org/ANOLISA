use crate::evidence::{provider_safe_command_fact_line, terminal_output_id};
use crate::types::{AgentEvent, AgentRequest, QuestionSelectionMode, PROVIDER_TIMEOUT_ERROR_CODE};

use super::{AdapterError, AgentAdapter, AgentBackendCapabilities};
use control_protocol::emit_fake_control_protocol_stream;
use fixtures::{
    extract_fake_approval_result, extract_fake_pending_answer, extract_fake_tool_result,
    fake_long_tool_output,
};
use responses_markdown::fake_markdown_response;
use stream_markdown::emit_fake_markdown_stream;
use stream_state::{
    emit_fake_late_card_or_artifact_stream, emit_fake_slow_stream, emit_fake_stale_question_stream,
};
use stream_tool_approval::emit_fake_tool_approval_stream;

mod control_protocol;
mod fixtures;
mod responses_markdown;
mod stream_markdown;
mod stream_state;
mod stream_tool_approval;

/// Pacing delay between streamed fake events. Defaults to zero so the fake
/// provider settles as fast as the runtime consumes events; set
/// `COSH_FAKE_STREAM_PACING_MS` to restore a human-visible streaming rhythm
/// (for demos or manual TTY runs). Values are clamped to one second so a
/// stray setting cannot silently stall every streaming test. Semantic delays
/// that create deliberate cancellation/approval race windows do NOT go
/// through this helper.
fn fake_stream_pacing() -> std::time::Duration {
    use std::sync::OnceLock;
    const MAX_PACING_MS: u64 = 1_000;
    static PACING: OnceLock<std::time::Duration> = OnceLock::new();
    *PACING.get_or_init(|| {
        let millis = std::env::var("COSH_FAKE_STREAM_PACING_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        std::time::Duration::from_millis(millis.min(MAX_PACING_MS))
    })
}

fn fake_stream_pacing_sleep() {
    let pacing = fake_stream_pacing();
    if !pacing.is_zero() {
        std::thread::sleep(pacing);
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeAgentAdapter;

impl AgentAdapter for FakeAgentAdapter {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn capabilities(&self) -> AgentBackendCapabilities {
        AgentBackendCapabilities {
            text_stream: true,
            thinking_stream: false,
            session_resume: false,
            tool_intent: true,
            user_question: true,
            cancellable: true,
            control_protocol: false,
        }
    }

    fn run(&self, request: &AgentRequest) -> Result<Vec<AgentEvent>, AdapterError> {
        let language = crate::language_config_status().effective;
        if let Some(input) = &request.user_input {
            let run_id = request.id.clone();
            if let Some(answer) = extract_fake_pending_answer(input) {
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "answer_received".to_string(),
                        message: "received answer for pending Agent question".to_string(),
                    },
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: format!("Got your answer: {answer}"),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "answer handled without executing commands".to_string(),
                    },
                ]);
            }
            if let Some(tool_result) = extract_fake_tool_result(input) {
                let text = match tool_result.status.as_deref() {
                    Some("executed") => format!(
                        "Command result analysis for {request}: the approved Bash command finished. Review the native output above before the next step.",
                        request = tool_result.request
                    ),
                    Some("blocked" | "timed_out" | "failed") => format!(
                        "Command result analysis for {request}: the approved Bash command did not produce a successful execution result. Use the broker message above and request a simpler single read-only command if more evidence is needed.",
                        request = tool_result.request
                    ),
                    _ => format!(
                        "Command result analysis for {request}: review the native tool result above before the next step.",
                        request = tool_result.request
                    ),
                };
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "tool_result_received".to_string(),
                        message: "received approved tool result for same session".to_string(),
                    },
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text,
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "tool result handled without requesting another tool".to_string(),
                    },
                ]);
            }
            if input.contains("ShellCommandCompleted evidence") {
                let approval = input
                    .lines()
                    .find_map(|line| line.trim().strip_prefix("approval_id: "))
                    .unwrap_or("<unknown>");
                if input.contains("structured-before-recovery")
                    && !request
                        .context_hints
                        .iter()
                        .any(|hint| hint.contains("disable provider resume"))
                {
                    return Ok(vec![AgentEvent::AgentFailed {
                        run_id,
                        error: "Agent timed out: No provider response within 20s".to_string(),
                        error_code: Some(PROVIDER_TIMEOUT_ERROR_CODE.to_string()),
                        max_turns: None,
                    }]);
                }
                // Times out for the T1 continuation only, so the T2
                // same-session retry succeeds (T2-success presentation path).
                // Checked before the plain "trigger resume timeout" branch
                // because that trigger is a substring of this one.
                if input.contains("trigger resume timeout once")
                    && !request.context_hints.iter().any(|hint| {
                        hint.contains("disable provider resume")
                            || hint.contains("same-session retry")
                    })
                {
                    return Ok(vec![AgentEvent::AgentFailed {
                        run_id,
                        error: "Agent timed out: No provider response within 20s".to_string(),
                        error_code: Some(PROVIDER_TIMEOUT_ERROR_CODE.to_string()),
                        max_turns: None,
                    }]);
                }
                if input.contains("trigger resume timeout")
                    && !input.contains("trigger resume timeout once")
                    && !request
                        .context_hints
                        .iter()
                        .any(|hint| hint.contains("disable provider resume"))
                {
                    return Ok(vec![AgentEvent::AgentFailed {
                        run_id,
                        error: "Agent timed out: No provider response within 20s".to_string(),
                        error_code: Some(PROVIDER_TIMEOUT_ERROR_CODE.to_string()),
                        max_turns: None,
                    }]);
                }
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "shell_evidence_received".to_string(),
                        message: "received foreground shell evidence".to_string(),
                    },
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: format!(
                            "Command result analysis for {approval}: foreground shell evidence received. No additional shell command is required."
                        ),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "shell evidence handled without requesting another tool"
                            .to_string(),
                    },
                ]);
            }
            if input.contains("ShellEvidenceExcerpt") {
                if input.contains("history_index:") {
                    if input.contains("printf 'alpha\\nbeta\\ngamma\\n'") {
                        if let Some(output_id) = first_terminal_output_id(input) {
                            return Ok(vec![
                                AgentEvent::TextDelta {
                                    run_id: run_id.clone(),
                                    text: format!(
                                        "I found captured output.\n```cosh-request\noutput {output_id} tail\nlines 2\n```\n"
                                    ),
                                },
                                AgentEvent::AgentCompleted {
                                    run_id,
                                    summary: "requested captured output after history"
                                        .to_string(),
                                },
                            ]);
                        }
                    }
                    if input.contains("printf 'misroute-output\\n'") {
                        if let Some(output_id) = first_terminal_output_id(input) {
                            return Ok(vec![
                                AgentEvent::ToolCall {
                                    run_id: run_id.clone(),
                                    tool_id: Some("toolu-misroute".to_string()),
                                    name: "read_file".to_string(),
                                    input: format!(r#"{{"path":"{output_id}"}}"#),
                                },
                                AgentEvent::AgentCompleted {
                                    run_id,
                                    summary: "misroute terminal output read after history"
                                        .to_string(),
                                },
                            ]);
                        }
                    }
                    let text = if input.contains("token=<redacted>")
                        && !input.contains("command: echo token=super-secret")
                    {
                        "Redacted history index received by fake adapter."
                    } else {
                        "Evidence history index received by fake adapter."
                    };
                    return Ok(vec![
                        AgentEvent::StatusChanged {
                            run_id: run_id.clone(),
                            phase: "evidence_history_received".to_string(),
                            message: "received shell history index".to_string(),
                        },
                        AgentEvent::TextDelta {
                            run_id: run_id.clone(),
                            text: text.to_string(),
                        },
                        AgentEvent::AgentCompleted {
                            run_id,
                            summary: "evidence history handled".to_string(),
                        },
                    ]);
                }
                if input.contains("bounded_output_excerpt:") {
                    let excerpt = input
                        .split_once("bounded_output_excerpt:\n")
                        .map(|(_, excerpt)| excerpt.trim())
                        .unwrap_or("<missing excerpt>");
                    return Ok(vec![
                        AgentEvent::StatusChanged {
                            run_id: run_id.clone(),
                            phase: "evidence_excerpt_received".to_string(),
                            message: "received bounded shell evidence excerpt".to_string(),
                        },
                        AgentEvent::TextDelta {
                            run_id: run_id.clone(),
                            text: format!("Evidence excerpt received by fake adapter: {excerpt}"),
                        },
                        AgentEvent::AgentCompleted {
                            run_id,
                            summary: "evidence excerpt handled".to_string(),
                        },
                    ]);
                }
            }
            if let Some(approval_result) = extract_fake_approval_result(input) {
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "approval_result_received".to_string(),
                        message: "received approval denial for same session".to_string(),
                    },
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: format!(
                            "Command was not executed for {approval_result}. No shell output exists for that request."
                        ),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "approval result handled without executing commands".to_string(),
                    },
                ]);
            }
            if request
                .context_hints
                .iter()
                .any(|hint| hint == "__cosh_request_source=insight_prompt")
            {
                return Ok(fake_bound_insight_events(request, language, run_id));
            }
            if input.contains("adapter crash") {
                return Err(AdapterError {
                    message: "fake adapter crashed".to_string(),
                });
            }
            if input.contains("backend unavailable") || input.contains("adapter failure") {
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "failed".to_string(),
                        message: "simulating fake backend failure".to_string(),
                    },
                    AgentEvent::AgentFailed {
                        run_id,
                        error: "fake backend unavailable".to_string(),
                        error_code: None,
                        max_turns: None,
                    },
                ]);
            }
            if input.contains("context") {
                let context = request
                    .context_blocks
                    .iter()
                    .map(provider_safe_command_fact_line)
                    .collect::<Vec<_>>()
                    .join("\n");
                let runtime_hints = request.context_hints.join("\n");
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "context".to_string(),
                        message: "returning recent shell context".to_string(),
                    },
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: format!(
                            "Recent context visible to Agent:\n{}\nRuntime context hints visible to Agent:\n{}",
                            if context.is_empty() {
                                "<none>".to_string()
                            } else {
                                context
                            },
                            if runtime_hints.is_empty() {
                                "<none>".to_string()
                            } else {
                                runtime_hints
                            }
                        ),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "recent context fake analysis completed".to_string(),
                    },
                ]);
            }
            if request
                .context_hints
                .iter()
                .any(|hint| hint.starts_with("health_scan "))
            {
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "health_context".to_string(),
                        message: "returning startup health context".to_string(),
                    },
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: format!(
                            "Received shell prompt request: {input}\nRuntime context hints visible to Agent:\n{}",
                            request.context_hints.join("\n")
                        ),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "startup health context fake analysis completed".to_string(),
                    },
                ]);
            }
            if let Some(events) = fake_markdown_response(input, &run_id) {
                return Ok(events);
            }
            if input.contains("ask") && input.contains("question") {
                let (question, options, selection_mode) = if input.contains("multi") {
                    (
                        "Choose checks to run".to_string(),
                        vec![
                            "Lint".to_string(),
                            "Unit tests".to_string(),
                            "Raw shell smoke".to_string(),
                        ],
                        QuestionSelectionMode::Multiple,
                    )
                } else if input.contains("free") {
                    (
                        "Tell me the branch name to inspect".to_string(),
                        Vec::new(),
                        QuestionSelectionMode::Single,
                    )
                } else {
                    (
                        "Choose a color for the next step".to_string(),
                        vec!["Green".to_string(), "Blue".to_string(), "Gray".to_string()],
                        QuestionSelectionMode::Single,
                    )
                };
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "question".to_string(),
                        message: "asking user to choose an option".to_string(),
                    },
                    AgentEvent::UserQuestion {
                        run_id: run_id.clone(),
                        provider_request_id: None,
                        question,
                        options,
                        allow_free_text: true,
                        selection_mode,
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "question displayed without executing commands".to_string(),
                    },
                ]);
            }
            if input.contains("readonly builtin tool") {
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "routing".to_string(),
                        message: "matching read-only builtin fake tool workflow".to_string(),
                    },
                    AgentEvent::ToolCall {
                        run_id: run_id.clone(),
                        tool_id: None,
                        name: "Read".to_string(),
                        input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
                    },
                    AgentEvent::ToolCall {
                        run_id: run_id.clone(),
                        tool_id: None,
                        name: "Grep".to_string(),
                        input: r#"{"pattern":"cosh","path":"crates/cosh-shell"}"#.to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "read-only builtin tool request completed".to_string(),
                    },
                ]);
            }
            if input.contains("unsafe tool") {
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "routing".to_string(),
                        message: "matching shell-first request to unsafe fake tool workflow"
                            .to_string(),
                    },
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: match language {
                            crate::Language::EnUs => {
                                format!("Received shell prompt request: {input}")
                            }
                            crate::Language::ZhCn => format!("已收到 Shell 提示请求：{input}"),
                        },
                    },
                    AgentEvent::ToolCall {
                        run_id: run_id.clone(),
                        tool_id: None,
                        name: "Bash".to_string(),
                        input: "touch /tmp/cosh-shell-fake-action-should-not-run".to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "unsafe tool request requires approval".to_string(),
                    },
                ]);
            }
            if input.contains("long tool") {
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "routing".to_string(),
                        message: "matching long-running fake tool workflow".to_string(),
                    },
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: format!("Received shell prompt request: {input}"),
                    },
                    AgentEvent::ToolCall {
                        run_id: run_id.clone(),
                        tool_id: None,
                        name: "Bash".to_string(),
                        input: "sleep 4; printf done".to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "long-running tool request requires approval".to_string(),
                    },
                ]);
            }
            if input.contains("agent memory hook fallback") {
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "routing".to_string(),
                        message: "matching local agent fallback memory hook workflow".to_string(),
                    },
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: format!("Received shell prompt request: {input}"),
                    },
                    AgentEvent::ToolCall {
                        run_id: run_id.clone(),
                        tool_id: None,
                        name: "Bash".to_string(),
                        input: "free -m; touch /tmp/cosh-shell-fake-memory-hook-marker".to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "local agent fallback requires approval".to_string(),
                    },
                ]);
            }
            if input.contains("provider native tool") {
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "routing".to_string(),
                        message: "matching provider-native fake tool workflow".to_string(),
                    },
                    AgentEvent::ToolPermissionRequest {
                        run_id: run_id.clone(),
                        request_id: "ctrl-1".to_string(),
                        tool_name: "run_shell_command".to_string(),
                        tool_input: serde_json::json!({ "command": "git status" }),
                        tool_use_id: "toolu-1".to_string(),
                        hook_requires_approval: false,
                        audit_ref: None,
                    },
                    AgentEvent::ToolOutputDelta {
                        run_id: run_id.clone(),
                        tool_id: "toolu-1".to_string(),
                        stream: "stdout".to_string(),
                        text: "On branch main\nnothing to commit\n".to_string(),
                    },
                    AgentEvent::ToolCompleted {
                        run_id: run_id.clone(),
                        tool_id: "toolu-1".to_string(),
                        status: "completed".to_string(),
                    },
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: "Provider-native tool result rendered.".to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "provider-native fake tool completed".to_string(),
                    },
                ]);
            }
            if input.contains("provider interactive failure") {
                return Ok(vec![
                    AgentEvent::ToolCall {
                        run_id: run_id.clone(),
                        tool_id: Some("toolu-tty".to_string()),
                        name: "run_shell_command".to_string(),
                        input: r#"{"command":"git status"}"#.to_string(),
                    },
                    AgentEvent::ToolOutputDelta {
                        run_id: run_id.clone(),
                        tool_id: "toolu-tty".to_string(),
                        stream: "stderr".to_string(),
                        text: "sudo: a terminal is required\n".to_string(),
                    },
                    AgentEvent::ToolCompleted {
                        run_id: run_id.clone(),
                        tool_id: "toolu-tty".to_string(),
                        status: "error".to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "provider interactive failure rendered".to_string(),
                    },
                ]);
            }
            if input.contains("tool output finalization") {
                return Ok(vec![
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: "Before tool **markdown**.\n\n".to_string(),
                    },
                    AgentEvent::ToolOutputDelta {
                        run_id: run_id.clone(),
                        tool_id: "toolu-md".to_string(),
                        stream: "stdout".to_string(),
                        text: "clean\n".to_string(),
                    },
                    AgentEvent::ToolCompleted {
                        run_id: run_id.clone(),
                        tool_id: "toolu-md".to_string(),
                        status: "success".to_string(),
                    },
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: "\nAfter tool **markdown**.".to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "markdown around provider tool output completed".to_string(),
                    },
                ]);
            }
            if input.contains("request shell history evidence") {
                return Ok(vec![
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: "I need shell history.\n```cosh-request\nhistory\n```\n".to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "requested shell history evidence".to_string(),
                    },
                ]);
            }
            if input.contains("request captured output evidence") {
                if let Some(output_id) =
                    first_request_output_id(request).or_else(|| first_terminal_output_id(input))
                {
                    return Ok(vec![
                        AgentEvent::TextDelta {
                            run_id: run_id.clone(),
                            text: format!(
                                "I need captured output.\n```cosh-request\noutput {output_id} tail\nlines 2\n```\n"
                            ),
                        },
                        AgentEvent::AgentCompleted {
                            run_id,
                            summary: "requested captured output evidence".to_string(),
                        },
                    ]);
                }
                return Ok(vec![
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: "I need to find the captured output first.\n```cosh-request\nhistory\n```\n"
                            .to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "requested shell history before captured output".to_string(),
                    },
                ]);
            }
            if input.contains("request invalid shell evidence") {
                return Ok(vec![
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: "I need invalid shell evidence.\n```cosh-request\nread terminal-output://raw-session/cmd-1\n```\n"
                            .to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "requested invalid shell evidence".to_string(),
                    },
                ]);
            }
            if input.contains("recommend evidence instruction check") {
                if input.contains("cosh_shell_evidence") || input.contains("```cosh-request") {
                    return Ok(vec![AgentEvent::AgentFailed {
                        run_id,
                        error: "recommend prompt exposed shell evidence request instructions"
                            .to_string(),
                        error_code: None,
                        max_turns: None,
                    }]);
                }
                return Ok(vec![
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: "Recommend evidence instructions suppressed.".to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "recommend evidence instruction check completed".to_string(),
                    },
                ]);
            }
            if input.contains("list recent commands only evidence check") {
                if input.contains("bounded_output_excerpt:") {
                    return Ok(vec![AgentEvent::AgentFailed {
                        run_id,
                        error: "list-only prompt included output excerpt".to_string(),
                        error_code: None,
                        max_turns: None,
                    }]);
                }
                return Ok(vec![
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: "Recent command facts only; no output read requested.".to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "list-only evidence check completed".to_string(),
                    },
                ]);
            }
            if input.contains("recommendation fixture") {
                let summary = match language {
                    crate::Language::EnUs => {
                        "Explicit compatibility fixture for display-only recommendations."
                    }
                    crate::Language::ZhCn => "用于验证仅展示推荐兼容性的显式 fixture。",
                };
                return Ok(vec![
                    AgentEvent::Recommendation {
                        run_id: run_id.clone(),
                        summary: summary.to_string(),
                        commands: vec![
                            "pwd".to_string(),
                            "echo $PATH".to_string(),
                            "printf 'fixture recommendation\\n'".to_string(),
                        ],
                        auto_execute: false,
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "recommendation fixture completed".to_string(),
                    },
                ]);
            }
            if input.contains("misroute terminal output read") {
                if let Some(output_id) =
                    first_request_output_id(request).or_else(|| first_terminal_output_id(input))
                {
                    return Ok(vec![
                        AgentEvent::ToolCall {
                            run_id: run_id.clone(),
                            tool_id: Some("toolu-misroute".to_string()),
                            name: "read_file".to_string(),
                            input: format!(r#"{{"path":"{output_id}"}}"#),
                        },
                        AgentEvent::AgentCompleted {
                            run_id,
                            summary: "misroute terminal output read rendered".to_string(),
                        },
                    ]);
                }
                return Ok(vec![
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: "I need to locate the captured output first.\n```cosh-request\nhistory\n```\n"
                            .to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "requested shell history before misroute".to_string(),
                    },
                ]);
            }
            if input.contains("tool") {
                return Ok(vec![
                    AgentEvent::StatusChanged {
                        run_id: run_id.clone(),
                        phase: "routing".to_string(),
                        message: "matching shell-first request to fake tool workflow".to_string(),
                    },
                    AgentEvent::TextDelta {
                        run_id: run_id.clone(),
                        text: format!("Received shell prompt request: {input}"),
                    },
                    AgentEvent::ToolCall {
                        run_id: run_id.clone(),
                        tool_id: None,
                        name: "shell".to_string(),
                        input: "git status".to_string(),
                    },
                    AgentEvent::ToolOutputDelta {
                        run_id: run_id.clone(),
                        tool_id: "tool-1".to_string(),
                        stream: "stdout".to_string(),
                        text: fake_long_tool_output(),
                    },
                    AgentEvent::ToolCompleted {
                        run_id: run_id.clone(),
                        tool_id: "tool-1".to_string(),
                        status: "completed".to_string(),
                    },
                    AgentEvent::Action {
                        run_id: run_id.clone(),
                        command: "touch /tmp/cosh-shell-fake-action-should-not-run".to_string(),
                    },
                    AgentEvent::AgentCompleted {
                        run_id,
                        summary: "analysis completed without executing commands".to_string(),
                    },
                ]);
            }
            return Ok(vec![
                AgentEvent::StatusChanged {
                    run_id: run_id.clone(),
                    phase: "routing".to_string(),
                    message: "matching shell-first request to fake guidance workflow".to_string(),
                },
                AgentEvent::TextDelta {
                    run_id: run_id.clone(),
                    text: match language {
                        crate::Language::EnUs => format!("Received shell prompt request: {input}"),
                        crate::Language::ZhCn => format!("已收到 Shell 提示请求：{input}"),
                    },
                },
                AgentEvent::Recommendation {
                    run_id: run_id.clone(),
                    summary: match language {
                        crate::Language::EnUs => {
                            "Ask the configured Agent backend for recommend-only shell guidance."
                                .to_string()
                        }
                        crate::Language::ZhCn => {
                            "请使用已配置的 Agent 后端提供仅建议的 Shell 指引。".to_string()
                        }
                    },
                    commands: Vec::new(),
                    auto_execute: false,
                },
                AgentEvent::AgentCompleted {
                    run_id,
                    summary: "analysis completed without executing commands".to_string(),
                },
            ]);
        }

        let run_id = request.id.clone();
        Ok(vec![
            AgentEvent::StatusChanged {
                run_id: run_id.clone(),
                phase: "analyzing".to_string(),
                message: "building failed command context for fake adapter".to_string(),
            },
            AgentEvent::TextDelta {
                run_id: run_id.clone(),
                text: match crate::language_config_status().effective {
                    crate::Language::EnUs => format!(
                        "The command `{}` failed with exit code {}.",
                        request.command_block.command, request.command_block.exit_code
                    ),
                    crate::Language::ZhCn => format!(
                        "命令 `{}` 以退出码 {} 失败。",
                        request.command_block.command, request.command_block.exit_code
                    ),
                },
            },
            AgentEvent::AgentCompleted {
                run_id,
                summary: "analysis completed without executing commands".to_string(),
            },
        ])
    }

    fn run_stream(
        &self,
        request: &AgentRequest,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
    ) -> Result<(), AdapterError> {
        let input = request
            .user_input
            .as_deref()
            .unwrap_or(request.command_block.command.as_str());

        if input.contains("ShellCommandCompleted evidence") {
            for event in self.run(request)? {
                sink(event)?;
            }
            return Ok(());
        }

        if emit_fake_markdown_stream(input, request, sink)? {
            return Ok(());
        }

        if emit_fake_tool_approval_stream(input, request, sink)? {
            return Ok(());
        }

        if emit_fake_stale_question_stream(input, request, sink)? {
            return Ok(());
        }

        if emit_fake_late_card_or_artifact_stream(input, request, sink)? {
            return Ok(());
        }

        if emit_fake_control_protocol_stream(input, request, sink)? {
            return Ok(());
        }

        if !input.contains("slow") {
            for event in self.run(request)? {
                sink(event)?;
            }
            return Ok(());
        }

        emit_fake_slow_stream(input, request, sink)
    }
}

fn fake_bound_insight_events(
    request: &AgentRequest,
    language: crate::Language,
    run_id: String,
) -> Vec<AgentEvent> {
    let text = match (language, request.command_block.exit_code) {
        (crate::Language::EnUs, 0) => {
            "Analyzed the bounded output evidence without executing another command.".to_string()
        }
        (crate::Language::ZhCn, 0) => "已基于有界输出证据完成分析，未执行其他命令。".to_string(),
        (crate::Language::EnUs, exit_code) => format!(
            "The command `{}` failed with exit code {exit_code}.",
            request.command_block.command
        ),
        (crate::Language::ZhCn, exit_code) => format!(
            "命令 `{}` 以退出码 {exit_code} 失败。",
            request.command_block.command
        ),
    };
    vec![
        AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "analyzing".to_string(),
            message: "analyzing bounded insight evidence".to_string(),
        },
        AgentEvent::TextDelta {
            run_id: run_id.clone(),
            text,
        },
        AgentEvent::AgentCompleted {
            run_id,
            summary: "analysis completed without executing commands".to_string(),
        },
    ]
}

fn first_terminal_output_id(input: &str) -> Option<String> {
    input.split_whitespace().find_map(|token| {
        let start = token.find("terminal-output://")?;
        Some(
            token[start..]
                .trim_matches(|ch: char| matches!(ch, ',' | ';' | '.' | '`'))
                .to_string(),
        )
    })
}

fn first_request_output_id(request: &AgentRequest) -> Option<String> {
    request
        .context_blocks
        .iter()
        .find(|block| block.output.terminal_output_ref.is_some())
        .map(|block| terminal_output_id(&block.session_id, &block.id))
}
